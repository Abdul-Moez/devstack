//! One type per managed program. The [`Service`] trait's default methods do the generic work
//! (port checks, spawning, health checks, stopping); each service only says what's specific.
//!
//! To add a service: implement `Service`, add it to [`all`], add its executable to
//! `versions::TOOLS`, and a default to `settings::defaults()` (services + ports).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;

use crate::core::App;
use crate::error::{service_err, DsError};
use crate::procs::{alive, kill_tree, listening, port_open, port_owner, record, spawn, wait_exit};
use crate::settings::ProcRec;
use crate::util::{command, exe, fwd, path_sep, run, run_logged, tail, IS_WIN};
use crate::versions::Version;
use crate::Log;

pub fn describe_ports(ports: &[u16]) -> String {
    match ports.len() {
        0 => String::new(),
        1 => format!(" on port {}", ports[0]),
        n if n > 2 && ports.windows(2).all(|w| w[1] == w[0] + 1) => format!(" on ports {}-{}", ports[0], ports[n - 1]),
        _ => format!(" on ports {}", ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")),
    }
}

pub trait Service: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn tool(&self) -> &'static str;
    /// Start order (stop runs in reverse).
    fn order(&self) -> u32 {
        50
    }
    /// Databases are never killed without asking.
    fn database(&self) -> bool {
        false
    }
    fn start_timeout(&self) -> u64 {
        20
    }
    fn stop_timeout(&self) -> u64 {
        15
    }

    fn version(&self, app: &App) -> Option<Version> {
        app.active(self.tool())
    }
    fn installed(&self, app: &App) -> bool {
        self.version(app).is_some()
    }
    fn enabled(&self, app: &App) -> bool {
        app.settings.bool(&format!("services.{}", self.id()))
    }
    fn set_enabled(&self, app: &App, on: bool) -> Result<()> {
        app.settings.set(&format!("services.{}", self.id()), json!(on))
    }

    fn ports(&self, _app: &App) -> Vec<u16> {
        vec![]
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>>;
    fn env(&self, _app: &App, _v: &Version) -> Option<HashMap<String, String>> {
        None
    }
    fn cwd(&self, _app: &App, v: &Version) -> PathBuf {
        v.path.clone()
    }
    /// Create folders, initialize data. Runs before every start.
    fn prepare(&self, _app: &App, _v: &Version, _log: Log) -> Result<()> {
        Ok(())
    }
    /// Fail if the generated config is invalid.
    fn check_config(&self, _app: &App, _v: &Version) -> Result<()> {
        Ok(())
    }
    /// Ask the program to shut down. Ok(true) if the request was sent.
    fn clean_stop(&self, _app: &App, _v: &Version) -> Result<bool> {
        Ok(false)
    }
    /// Add info (e.g. the port) to process records after spawning.
    fn annotate(&self, _app: &App, _recs: &mut [ProcRec]) {}

    fn log_path(&self, app: &App, i: usize, n: usize) -> PathBuf {
        if n <= 1 {
            app.paths.logs.join(format!("{}.log", self.id()))
        } else {
            app.paths.logs.join(format!("{}-{i}.log", self.id()))
        }
    }

    fn records(&self, app: &App) -> Vec<ProcRec> {
        app.state.procs(self.id())
    }
    fn running(&self, app: &App) -> bool {
        self.records(app).iter().any(alive)
    }
    fn status(&self, app: &App) -> String {
        generic_status(self, app)
    }
    fn start(&self, app: &App, log: Log) -> Result<()> {
        generic_start(self, app, log)
    }
    fn stop(&self, app: &App, log: Log, force: bool) -> Result<()> {
        generic_stop(self, app, log, force)
    }
    fn restart(&self, app: &App, log: Log) -> Result<()> {
        self.stop(app, log, false)?;
        self.start(app, log)
    }
    fn reload(&self, app: &App, log: Log) -> Result<()> {
        self.restart(app, log)
    }
}

pub fn generic_status<S: Service + ?Sized>(s: &S, app: &App) -> String {
    if !s.installed(app) {
        return "not installed".into();
    }
    let recs = s.records(app);
    let up = recs.iter().filter(|r| alive(r)).count();
    if up == 0 {
        "stopped".into()
    } else if up == recs.len() {
        "running".into()
    } else {
        "degraded".into()
    }
}

pub fn full_env<S: Service + ?Sized>(s: &S, app: &App, v: &Version) -> Option<HashMap<String, String>> {
    s.env(app, v).map(|extra| crate::util::env_with(&extra))
}

pub fn generic_start<S: Service + ?Sized>(s: &S, app: &App, log: Log) -> Result<()> {
    if s.running(app) {
        log(&format!("{} is already running", s.name()));
        return Ok(());
    }
    let v = s.version(app).ok_or_else(|| service_err(format!("{} is not installed. Add it from Tools > Quick add.", s.name())))?;
    let table = listening();
    for p in s.ports(app) {
        if let Some(owner) = port_owner(p, &table) {
            return Err(service_err(format!(
                "{}: port {p} is already in use by {owner}. Stop that program or change the port in Preferences.",
                s.name()
            )));
        }
    }
    s.prepare(app, &v, log)?;
    s.check_config(app, &v)?;
    let env = full_env(s, app, &v);
    let cmds = s.commands(app, &v);
    let n = cmds.len();
    let mut recs = Vec::new();
    for (i, cmd) in cmds.iter().enumerate() {
        match spawn(cmd, &s.cwd(app, &v), env.as_ref(), &s.log_path(app, i, n)) {
            Ok(r) => recs.push(r),
            Err(e) => {
                abort(s, app, &recs);
                return Err(e);
            }
        }
    }
    s.annotate(app, &mut recs);
    app.state.set_procs(s.id(), &recs)?;
    wait_ready(s, app, &recs, n)?;
    log(&format!("{} {} started{}", s.name(), v.number(), describe_ports(&s.ports(app))));
    Ok(())
}

fn abort<S: Service + ?Sized>(s: &S, app: &App, recs: &[ProcRec]) {
    for r in recs {
        kill_tree(r.pid, Duration::from_secs(5));
    }
    let _ = app.state.set_procs(s.id(), &[]);
}

fn wait_ready<S: Service + ?Sized>(s: &S, app: &App, recs: &[ProcRec], n: usize) -> Result<()> {
    let start = Instant::now();
    let ports = s.ports(app);
    let dead = |recs: &[ProcRec]| recs.iter().position(|r| !alive(r));
    while start.elapsed() < Duration::from_secs(s.start_timeout()) {
        if let Some(i) = dead(recs) {
            abort(s, app, recs);
            return Err(service_err(format!(
                "{} exited during startup. Last log lines:\n{}",
                s.name(),
                tail(&s.log_path(app, i, n), 20)
            )));
        }
        if ports.iter().all(|p| port_open(*p)) {
            thread::sleep(Duration::from_millis(750)); // catch programs that bind and then abort
            if let Some(i) = dead(recs) {
                abort(s, app, recs);
                return Err(service_err(format!(
                    "{} stopped right after starting. Last log lines:\n{}",
                    s.name(),
                    tail(&s.log_path(app, i, n), 20)
                )));
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    abort(s, app, recs);
    Err(service_err(format!(
        "{} did not become ready within {}s. Last log lines:\n{}",
        s.name(),
        s.start_timeout(),
        tail(&s.log_path(app, 0, n), 20)
    )))
}

pub fn generic_stop<S: Service + ?Sized>(s: &S, app: &App, log: Log, force: bool) -> Result<()> {
    let recs: Vec<ProcRec> = s.records(app).into_iter().filter(alive).collect();
    if recs.is_empty() {
        app.state.set_procs(s.id(), &[])?;
        return Ok(());
    }
    let mut sent = false;
    if let Some(v) = s.version(app) {
        match s.clean_stop(app, &v) {
            Ok(b) => sent = b,
            Err(e) => log(&format!("{}: clean shutdown failed: {e}", s.name())),
        }
    }
    if !(sent && wait_exit(&recs, Duration::from_secs(s.stop_timeout()))) {
        if s.database() && !force {
            return Err(DsError::NeedsForce {
                sid: s.id().to_string(),
                msg: format!(
                    "{} did not shut down cleanly. Force it to stop? Unsaved data may be lost (it will recover on next start).",
                    s.name()
                ),
            }
            .into());
        }
        for r in &recs {
            kill_tree(r.pid, Duration::from_secs(5));
        }
    }
    app.state.set_procs(s.id(), &[])?;
    log(&format!("{} stopped", s.name()));
    Ok(())
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn check(out: Result<crate::util::CmdOut>, what: &str) -> Result<()> {
    let out = out?;
    if out.ok() {
        Ok(())
    } else {
        Err(service_err(format!("{what} config test failed:\n{}", out.message())))
    }
}

// =========================================================================== Apache
pub struct Apache;

impl Apache {
    fn conf(app: &App) -> PathBuf {
        app.paths.etc.join("apache").join("httpd.conf")
    }
    fn args(app: &App, v: &Version) -> Vec<String> {
        vec!["-d".into(), fwd(&v.path), "-f".into(), fwd(Self::conf(app))]
    }
}

impl Service for Apache {
    fn id(&self) -> &'static str { "apache" }
    fn name(&self) -> &'static str { "Apache" }
    fn tool(&self) -> &'static str { "apache" }
    fn order(&self) -> u32 { 90 }

    fn enabled(&self, app: &App) -> bool {
        app.settings.str("web_server") == "apache"
    }
    fn set_enabled(&self, app: &App, on: bool) -> Result<()> {
        app.settings.set("web_server", json!(if on { "apache" } else { "" }))
    }
    fn ports(&self, app: &App) -> Vec<u16> {
        web_ports(app)
    }
    fn env(&self, app: &App, _v: &Version) -> Option<HashMap<String, String>> {
        // PHP's folder on PATH so the embedded PHP module's extension DLLs load.
        let php = app.active("php")?;
        let path = format!("{}{}{}", php.path.display(), path_sep(), std::env::var("PATH").unwrap_or_default());
        Some(HashMap::from([("PATH".to_string(), path)]))
    }
    fn prepare(&self, app: &App, _v: &Version, _log: Log) -> Result<()> {
        fs::create_dir_all(app.paths.tmp.join("apache"))?;
        Ok(())
    }
    fn check_config(&self, app: &App, v: &Version) -> Result<()> {
        let mut c = command(&v.exe, Self::args(app, v));
        c.arg("-t").current_dir(&v.path);
        if let Some(env) = full_env(self, app, v) {
            c.env_clear().envs(env);
        }
        check(run(c, secs(30)), "Apache")
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>> {
        let mut a = vec![fwd(&v.exe)];
        a.extend(Self::args(app, v));
        a.push("-DFOREGROUND".into());
        vec![a]
    }
}

fn web_ports(app: &App) -> Vec<u16> {
    let mut p = vec![app.settings.port("http")];
    if app.settings.bool("ssl") {
        p.push(app.settings.port("https"));
    }
    p
}

// =========================================================================== Nginx
pub struct Nginx;

impl Nginx {
    fn args(app: &App) -> Vec<String> {
        vec!["-p".into(), format!("{}/", fwd(&app.paths.root)), "-c".into(), fwd(app.paths.etc.join("nginx").join("nginx.conf"))]
    }
    fn ctl(app: &App, v: &Version, extra: &[&str]) -> Result<crate::util::CmdOut> {
        let mut c = command(&v.exe, Self::args(app));
        c.args(extra).current_dir(&v.path);
        run(c, secs(30))
    }
}

impl Service for Nginx {
    fn id(&self) -> &'static str { "nginx" }
    fn name(&self) -> &'static str { "Nginx" }
    fn tool(&self) -> &'static str { "nginx" }
    fn order(&self) -> u32 { 90 }

    fn enabled(&self, app: &App) -> bool {
        app.settings.str("web_server") == "nginx"
    }
    fn set_enabled(&self, app: &App, on: bool) -> Result<()> {
        app.settings.set("web_server", json!(if on { "nginx" } else { "" }))
    }
    fn ports(&self, app: &App) -> Vec<u16> {
        web_ports(app)
    }
    fn prepare(&self, app: &App, _v: &Version, _log: Log) -> Result<()> {
        for d in ["client_body", "proxy", "fastcgi", "uwsgi", "scgi"] {
            fs::create_dir_all(app.paths.tmp.join("nginx").join(d))?;
        }
        Ok(())
    }
    fn check_config(&self, app: &App, v: &Version) -> Result<()> {
        check(Self::ctl(app, v, &["-t"]), "Nginx")
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>> {
        let mut a = vec![fwd(&v.exe)];
        a.extend(Self::args(app));
        if !IS_WIN {
            a.extend(["-g".to_string(), "daemon off;".to_string()]); // Windows nginx never daemonizes
        }
        vec![a]
    }
    fn clean_stop(&self, app: &App, v: &Version) -> Result<bool> {
        Ok(Self::ctl(app, v, &["-s", "quit"])?.ok())
    }
    fn reload(&self, app: &App, log: Log) -> Result<()> {
        let Some(v) = self.version(app) else { return self.restart(app, log) };
        if !self.running(app) {
            return self.restart(app, log);
        }
        self.check_config(app, &v)?;
        if !Self::ctl(app, &v, &["-s", "reload"])?.ok() {
            return self.restart(app, log);
        }
        thread::sleep(Duration::from_millis(600)); // workers swap asynchronously after the signal
        log("Nginx reloaded");
        Ok(())
    }
}

// =========================================================================== PHP pool
/// A pool of php-cgi processes on consecutive ports. Nginx always uses it; Apache only when
/// the PHP build has no Apache module (non-thread-safe builds). Enabled automatically.
pub struct Php;

pub fn php_children(app: &App) -> u16 {
    u16::try_from(app.settings.u64("php.children", 10).clamp(4, 64)).unwrap_or(10)
}

pub fn php_pool_ports(app: &App) -> Vec<u16> {
    let base = app
        .state
        .value("php_port_base")
        .as_u64()
        .and_then(|b| u16::try_from(b).ok())
        .filter(|b| *b != 0)
        .unwrap_or_else(|| app.settings.port("php"));
    (0..php_children(app)).filter_map(|i| base.checked_add(i)).collect()
}

/// Pick the pool's base port before configs are generated.
pub fn php_plan_ports(app: &App) -> Result<()> {
    if Php.running(app) {
        return Ok(());
    }
    let mut base = app.settings.port("php");
    let n = php_children(app);
    let table = listening();
    if (base..base.saturating_add(n)).any(|p| table.contains_key(&p)) {
        base = app.settings.port("php_fallback");
    }
    app.state.put("php_port_base", json!(base))
}

pub fn php_in_apache(app: &App) -> bool {
    app.settings.str("web_server") == "apache" && crate::configs::php_apache_module(app, None).is_some()
}

fn php_command(v: &Version, port: u16) -> Vec<String> {
    vec![fwd(&v.exe), "-b".into(), format!("127.0.0.1:{port}"), "-c".into(), fwd(v.path.join("php.ini"))]
}

/// Restart pool members that died. Called periodically by the GUI.
pub fn php_heal(app: &App, log: Log) -> Result<usize> {
    let mut recs = Php.records(app);
    if recs.is_empty() || !recs.iter().any(alive) {
        return Ok(0);
    }
    let Some(v) = Php.version(app) else { return Ok(0) };
    let env = full_env(&Php, app, &v);
    let n = recs.len();
    let mut fixed = 0;
    for (i, rec) in recs.iter_mut().enumerate() {
        if !alive(rec) {
            if let Some(port) = rec.port {
                let mut new = spawn(&php_command(&v, port), &v.path, env.as_ref(), &Php.log_path(app, i, n))?;
                new.port = Some(port);
                *rec = new;
                fixed += 1;
            }
        }
    }
    if fixed > 0 {
        app.state.set_procs("php", &recs)?;
        log(&format!("PHP: restarted {fixed} pool process(es)"));
    }
    Ok(fixed)
}

impl Service for Php {
    fn id(&self) -> &'static str { "php" }
    fn name(&self) -> &'static str { "PHP" }
    fn tool(&self) -> &'static str { "php" }
    fn order(&self) -> u32 { 80 }
    fn start_timeout(&self) -> u64 { 15 }

    fn enabled(&self, app: &App) -> bool {
        match app.settings.str("web_server").as_str() {
            "nginx" => true,
            "apache" => !php_in_apache(app),
            _ => false,
        }
    }
    fn set_enabled(&self, _app: &App, _on: bool) -> Result<()> {
        Ok(()) // follows the web server choice
    }
    fn status(&self, app: &App) -> String {
        if !self.running(app) && self.installed(app) && php_in_apache(app) {
            return "in Apache".into();
        }
        generic_status(self, app)
    }
    fn ports(&self, app: &App) -> Vec<u16> {
        php_pool_ports(app)
    }
    fn env(&self, app: &App, v: &Version) -> Option<HashMap<String, String>> {
        Some(HashMap::from([
            ("PHP_FCGI_MAX_REQUESTS".into(), app.settings.u64("php.max_requests", 0).to_string()),
            ("PHP_FCGI_CHILDREN".into(), "0".into()),
            ("PHPRC".into(), v.path.display().to_string()),
            ("PHP_INI_SCAN_DIR".into(), String::new()),
        ]))
    }
    fn prepare(&self, app: &App, _v: &Version, _log: Log) -> Result<()> {
        for d in ["sessions", "uploads"] {
            fs::create_dir_all(app.paths.tmp.join("php").join(d))?;
        }
        Ok(())
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>> {
        php_pool_ports(app).into_iter().map(|p| php_command(v, p)).collect()
    }
    fn annotate(&self, app: &App, recs: &mut [ProcRec]) {
        for (r, p) in recs.iter_mut().zip(php_pool_ports(app)) {
            r.port = Some(p);
        }
    }
}

// =========================================================================== MySQL / MariaDB
pub struct MySql;

pub fn mysql_is_mariadb(v: &Version) -> bool {
    v.path.join("bin").join(exe("mariadb-install-db")).exists() || v.path.join("bin").join(exe("mariadbd")).exists()
}

pub const VC2010_URL: &str = "https://download.microsoft.com/download/1/6/5/165255E7-1014-4D0A-B094-B6A430A6BFFC/vcredist_x64.exe";

/// MySQL 5.6 builds link against the Visual C++ 2010 runtime (newer ones use the current
/// one). Without it mysqld dies with a "MSVCR100.dll not found" popup; say so up front.
pub fn mysql_missing_runtime(v: &Version) -> Option<String> {
    if !IS_WIN || mysql_is_mariadb(v) || v.key() >= vec![5, 7] {
        return None;
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let present = PathBuf::from(root).join("System32").join("msvcr100.dll").exists();
    (!present).then(|| format!("{} needs the Visual C++ 2010 runtime (x64). Install it from {VC2010_URL}", v.name))
}

pub fn mysql_data_dir(app: &App, v: &Version) -> PathBuf {
    let prefix = if mysql_is_mariadb(v) { "mariadb" } else { "mysql" };
    app.paths.data.join(format!("{prefix}-{}", v.series()))
}

pub fn mysql_client_cnf(app: &App) -> PathBuf {
    app.paths.etc.join("mysql").join("client.cnf")
}

/// First existing tool among `names` in the version's bin folder.
pub fn mysql_bin(v: &Version, names: &[&str]) -> PathBuf {
    for n in names {
        let p = v.path.join("bin").join(exe(n));
        if p.exists() {
            return p;
        }
    }
    v.path.join("bin").join(exe(names[0]))
}

impl MySql {
    fn ini(app: &App) -> PathBuf {
        app.paths.etc.join("mysql").join("my.ini")
    }
}

impl Service for MySql {
    fn id(&self) -> &'static str { "mysql" }
    fn name(&self) -> &'static str { "MySQL" }
    fn tool(&self) -> &'static str { "mysql" }
    fn order(&self) -> u32 { 10 }
    fn database(&self) -> bool { true }
    fn start_timeout(&self) -> u64 { 90 }
    fn stop_timeout(&self) -> u64 { 60 }

    fn ports(&self, app: &App) -> Vec<u16> {
        vec![app.settings.port("mysql")]
    }
    fn prepare(&self, app: &App, v: &Version, log: Log) -> Result<()> {
        if let Some(msg) = mysql_missing_runtime(v) {
            return Err(service_err(msg));
        }
        fs::create_dir_all(app.paths.tmp.join("mysql"))?;
        let dd = mysql_data_dir(app, v);
        if dd.exists() {
            return Ok(());
        }
        log(&format!("Initializing {} {} data folder (first run, can take a minute)…", self.name(), v.series()));
        fs::create_dir_all(dd.parent().unwrap())?;
        if !mysql_is_mariadb(v) && v.key() < vec![5, 7] {
            // MySQL 5.6 and older have no --initialize; the zip ships a ready data folder.
            let template = v.path.join("data");
            if !template.join("mysql").is_dir() {
                return Err(service_err(format!("{} has no data folder to start from.", v.name)));
            }
            crate::util::copy_dir(&template, &dd).inspect_err(|_| {
                let _ = fs::remove_dir_all(&dd);
            })?;
            return Ok(());
        }
        let c = if mysql_is_mariadb(v) {
            command(mysql_bin(v, &["mariadb-install-db", "mysql_install_db"]), [format!("--datadir={}", dd.display())])
        } else {
            command(&v.exe, [format!("--defaults-file={}", Self::ini(app).display()), "--initialize-insecure".into(), "--console".into()])
        };
        let mut c = c;
        c.current_dir(&v.path);
        let out = run(c, secs(600))?;
        if !out.ok() {
            let _ = fs::remove_dir_all(&dd);
            let msg = out.message();
            return Err(service_err(format!("{} initialization failed:\n{}", self.name(), &msg[msg.len().saturating_sub(3000)..])));
        }
        if !app.secrets.str("mysql_root_password").is_empty() {
            log("New data folder: root password is empty again.");
            app.secrets.set("mysql_root_password", json!(""))?;
            app.generate(log)?;
        }
        Ok(())
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>> {
        vec![vec![fwd(&v.exe), format!("--defaults-file={}", Self::ini(app).display()), "--console".into()]]
    }
    fn clean_stop(&self, app: &App, v: &Version) -> Result<bool> {
        let c = command(
            mysql_bin(v, &["mysqladmin", "mariadb-admin"]),
            [format!("--defaults-extra-file={}", mysql_client_cnf(app).display()), "shutdown".into()],
        );
        Ok(run(c, secs(30))?.ok())
    }
}

// =========================================================================== PostgreSQL
pub struct Postgres;

pub fn pg_data_dir(app: &App, v: &Version) -> PathBuf {
    app.paths.data.join(format!("postgresql-{}", v.major()))
}

impl Postgres {
    fn bin(v: &Version, name: &str) -> PathBuf {
        v.path.join("bin").join(exe(name))
    }
    fn ctl_log(app: &App) -> PathBuf {
        app.paths.logs.join("postgresql-ctl.log")
    }
}

impl Service for Postgres {
    fn id(&self) -> &'static str { "postgresql" }
    fn name(&self) -> &'static str { "PostgreSQL" }
    fn tool(&self) -> &'static str { "postgresql" }
    fn order(&self) -> u32 { 10 }
    fn database(&self) -> bool { true }
    fn start_timeout(&self) -> u64 { 60 }
    fn stop_timeout(&self) -> u64 { 60 }

    fn ports(&self, app: &App) -> Vec<u16> {
        vec![app.settings.port("postgresql")]
    }
    fn commands(&self, _app: &App, _v: &Version) -> Vec<Vec<String>> {
        vec![] // started through pg_ctl, see start()
    }
    fn prepare(&self, app: &App, v: &Version, log: Log) -> Result<()> {
        let dd = pg_data_dir(app, v);
        if dd.exists() {
            return Ok(());
        }
        log(&format!("Initializing PostgreSQL {} data folder (first run)…", v.major()));
        let mut c = command(Self::bin(v, "initdb"), ["-D".to_string(), dd.display().to_string(), "-U".into(), "postgres".into(),
                                                    "-A".into(), "trust".into(), "-E".into(), "UTF8".into(), "--no-locale".into()]);
        c.current_dir(&v.path);
        let out = run(c, secs(600))?;
        if !out.ok() {
            let _ = fs::remove_dir_all(&dd);
            return Err(service_err(format!("initdb failed:\n{}", out.message())));
        }
        let mut conf = fs::OpenOptions::new().append(true).open(dd.join("postgresql.conf"))?;
        use std::io::Write;
        writeln!(conf, "\n# devstack: your settings go in usr/postgresql.conf\ninclude_if_exists = '../../usr/postgresql.conf'")?;
        Ok(())
    }
    fn start(&self, app: &App, log: Log) -> Result<()> {
        // pg_ctl handles running from an admin account; postgres.exe alone refuses to.
        if self.running(app) {
            log("PostgreSQL is already running");
            return Ok(());
        }
        let v = self.version(app).ok_or_else(|| service_err("PostgreSQL is not installed. Add it from Tools > Quick add."))?;
        let port = self.ports(app)[0];
        if let Some(owner) = port_owner(port, &listening()) {
            return Err(service_err(format!("PostgreSQL: port {port} is already in use by {owner}.")));
        }
        self.prepare(app, &v, log)?;
        let dd = pg_data_dir(app, &v);
        let mut c = command(Self::bin(&v, "pg_ctl"), [
            "start".to_string(), "-w".into(), "-t".into(), "60".into(), "-D".into(), dd.display().to_string(),
            "-l".into(), self.log_path(app, 0, 1).display().to_string(),
            "-o".into(), format!("-p {port} -c listen_addresses=127.0.0.1"),
        ]);
        c.current_dir(&v.path);
        if run_logged(c, &Self::ctl_log(app), secs(90))? != 0 {
            return Err(service_err(format!(
                "PostgreSQL failed to start:\n{}\n{}",
                tail(&Self::ctl_log(app), 10),
                tail(&self.log_path(app, 0, 1), 10)
            )));
        }
        let pid: u32 = fs::read_to_string(dd.join("postmaster.pid"))?.lines().next().unwrap_or("0").trim().parse().unwrap_or(0);
        app.state.set_procs(self.id(), &[record(pid)])?;
        log(&format!("PostgreSQL {} started on port {port}", v.number()));
        Ok(())
    }
    fn clean_stop(&self, app: &App, v: &Version) -> Result<bool> {
        let mut c = command(Self::bin(v, "pg_ctl"), ["stop".to_string(), "-D".into(), pg_data_dir(app, v).display().to_string(),
                                                      "-m".into(), "fast".into(), "-w".into()]);
        c.current_dir(&v.path);
        Ok(run_logged(c, &Self::ctl_log(app), secs(90))? == 0)
    }
}

// =========================================================================== MongoDB
/// Not marked as a database: mongod has no CLI shutdown on Windows. A hard stop is safe
/// because WiredTiger journals every write and recovers on the next start.
pub struct Mongo;

impl Service for Mongo {
    fn id(&self) -> &'static str { "mongodb" }
    fn name(&self) -> &'static str { "MongoDB" }
    fn tool(&self) -> &'static str { "mongodb" }
    fn order(&self) -> u32 { 15 }
    fn start_timeout(&self) -> u64 { 60 }

    fn ports(&self, app: &App) -> Vec<u16> {
        vec![app.settings.port("mongodb")]
    }
    fn prepare(&self, app: &App, v: &Version, _log: Log) -> Result<()> {
        fs::create_dir_all(app.paths.data.join(format!("mongodb-{}", v.major())))?;
        Ok(())
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>> {
        vec![vec![fwd(&v.exe), "--dbpath".into(), fwd(app.paths.data.join(format!("mongodb-{}", v.major()))),
                  "--port".into(), self.ports(app)[0].to_string(), "--bind_ip".into(), "127.0.0.1".into()]]
    }
}

// =========================================================================== Redis
pub struct Redis;

impl Service for Redis {
    fn id(&self) -> &'static str { "redis" }
    fn name(&self) -> &'static str { "Redis" }
    fn tool(&self) -> &'static str { "redis" }
    fn order(&self) -> u32 { 20 }

    fn ports(&self, app: &App) -> Vec<u16> {
        vec![app.settings.port("redis")]
    }
    fn cwd(&self, app: &App, _v: &Version) -> PathBuf {
        app.paths.data.join("redis")
    }
    fn commands(&self, _app: &App, v: &Version) -> Vec<Vec<String>> {
        // redis.conf is generated into data/redis/ and loaded by relative path, because some
        // Windows builds of Redis don't understand C:/ paths.
        vec![vec![fwd(&v.exe), "redis.conf".into()]]
    }
    fn clean_stop(&self, app: &App, v: &Version) -> Result<bool> {
        let cli = v.path.join(exe("redis-cli"));
        if !cli.exists() {
            return Ok(false);
        }
        let _ = run(command(cli, ["-p".to_string(), self.ports(app)[0].to_string(), "shutdown".into()]), secs(15));
        Ok(true)
    }
}

// =========================================================================== Memcached
pub struct Memcached;

impl Service for Memcached {
    fn id(&self) -> &'static str { "memcached" }
    fn name(&self) -> &'static str { "Memcached" }
    fn tool(&self) -> &'static str { "memcached" }
    fn order(&self) -> u32 { 20 }

    fn ports(&self, app: &App) -> Vec<u16> {
        vec![app.settings.port("memcached")]
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>> {
        vec![vec![fwd(&v.exe), "-l".into(), "127.0.0.1".into(), "-p".into(), self.ports(app)[0].to_string(),
                  "-m".into(), app.settings.u64("memcached.memory_mb", 64).to_string()]]
    }
}

// =========================================================================== Mailpit
pub struct Mailpit;

impl Service for Mailpit {
    fn id(&self) -> &'static str { "mailpit" }
    fn name(&self) -> &'static str { "Mailpit" }
    fn tool(&self) -> &'static str { "mailpit" }
    fn order(&self) -> u32 { 30 }

    fn ports(&self, app: &App) -> Vec<u16> {
        vec![app.settings.port("mailpit_smtp"), app.settings.port("mailpit_ui")]
    }
    fn prepare(&self, app: &App, _v: &Version, _log: Log) -> Result<()> {
        fs::create_dir_all(app.paths.data.join("mailpit"))?;
        Ok(())
    }
    fn commands(&self, app: &App, v: &Version) -> Vec<Vec<String>> {
        let p = self.ports(app);
        vec![vec![fwd(&v.exe), "--smtp".into(), format!("127.0.0.1:{}", p[0]), "--listen".into(), format!("127.0.0.1:{}", p[1]),
                  "--database".into(), fwd(app.paths.data.join("mailpit").join("mailpit.db"))]]
    }
}

/// Every service, in start order.
pub fn all() -> Vec<Box<dyn Service>> {
    let mut v: Vec<Box<dyn Service>> = vec![
        Box::new(MySql), Box::new(Postgres), Box::new(Mongo), Box::new(Redis), Box::new(Memcached),
        Box::new(Mailpit), Box::new(Php), Box::new(Apache), Box::new(Nginx),
    ];
    v.sort_by_key(|s| s.order());
    v
}
