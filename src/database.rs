//! MySQL/MariaDB and PostgreSQL helpers: create databases, root password, backups, recovery.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use crate::core::App;
use crate::error::service_err;
use crate::services::{mysql_bin, mysql_client_cnf, mysql_data_dir, Service};
use crate::util::{command, exe, fwd, now_secs, run, timestamp, CmdOut};
use crate::versions::Version;
use crate::Log;

const SYSTEM_DBS: &[&str] = &["information_schema", "performance_schema", "mysql", "sys"];

pub struct MySqlTools<'a> {
    app: &'a App,
}

impl<'a> MySqlTools<'a> {
    pub fn new(app: &'a App) -> Self {
        MySqlTools { app }
    }

    fn svc(&self) -> &dyn Service {
        self.app.svc("mysql")
    }

    fn version(&self) -> Result<Version> {
        self.svc().version(self.app).ok_or_else(|| service_err("MySQL is not installed."))
    }

    fn require_running(&self) -> Result<()> {
        if !self.svc().running(self.app) {
            return Err(service_err("Start MySQL first."));
        }
        Ok(())
    }

    fn client(&self, names: &[&str], args: Vec<String>) -> Result<std::process::Command> {
        let v = self.version()?;
        let mut all = vec![format!("--defaults-extra-file={}", mysql_client_cnf(self.app).display())];
        all.extend(args);
        Ok(command(mysql_bin(&v, names), all))
    }

    fn check(out: CmdOut) -> Result<String> {
        if out.ok() {
            Ok(out.stdout)
        } else {
            Err(service_err(out.message()))
        }
    }

    pub fn sql(&self, statement: &str) -> Result<String> {
        self.require_running()?;
        let c = self.client(&["mysql", "mariadb"], vec!["-N".into(), "-B".into(), "-e".into(), statement.into()])?;
        Self::check(run(c, Duration::from_secs(60))?)
    }

    pub fn databases(&self) -> Result<Vec<String>> {
        Ok(self.sql("SHOW DATABASES")?.split_whitespace().map(str::to_string).collect())
    }

    pub fn create_database(&self, name: &str) -> Result<()> {
        let safe = name.replace('`', "");
        self.sql(&format!("CREATE DATABASE IF NOT EXISTS `{safe}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"))?;
        Ok(())
    }

    pub fn change_root_password(&self, new: &str, log: Log) -> Result<()> {
        let esc = new.replace('\\', "\\\\").replace('\'', "\\'");
        self.sql(&format!("ALTER USER 'root'@'localhost' IDENTIFIED BY '{esc}'; FLUSH PRIVILEGES;"))?;
        self.app.secrets.set("mysql_root_password", json!(new))?;
        self.app.generate(log)?;
        log("MySQL root password changed");
        Ok(())
    }

    pub fn backup_all(&self, log: Log) -> Result<PathBuf> {
        self.require_running()?;
        let out = self.app.paths.backup.join("mysql").join(timestamp());
        fs::create_dir_all(&out)?;
        let dbs: Vec<String> = self.databases()?.into_iter().filter(|d| !SYSTEM_DBS.contains(&d.to_lowercase().as_str())).collect();
        for db in &dbs {
            let c = self.client(&["mysqldump", "mariadb-dump"], vec![
                "--single-transaction".into(), "--routines".into(), "--triggers".into(), "--events".into(),
                "--databases".into(), db.clone(), format!("--result-file={}", out.join(format!("{db}.sql")).display()),
            ])?;
            let r = run(c, Duration::from_secs(3600))?;
            if !r.ok() {
                log(&format!("Backup of {db} failed: {}", r.message()));
            }
        }
        log(&format!("Backed up {} database(s) to {}", dbs.len(), out.display()));
        prune(&self.app.paths.backup.join("mysql"), self.app.settings.u64("backup.keep", 5) as usize);
        Ok(out)
    }

    pub fn restore(&self, folder: &Path, log: Log) -> Result<()> {
        self.require_running()?;
        let mut files: Vec<PathBuf> = fs::read_dir(folder)?.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "sql").unwrap_or(false)).collect();
        files.sort();
        for f in files {
            let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
            let c = self.client(&["mysql", "mariadb"], vec!["-e".into(), format!("source {}", fwd(&f))])?;
            let r = run(c, Duration::from_secs(3600))?;
            log(&if r.ok() { format!("Restored {stem}") } else { format!("Restore of {stem} failed: {}", r.message()) });
        }
        Ok(())
    }

    /// Move a broken data folder aside and start fresh. Returns the newest backup, if any.
    pub fn recover(&self, log: Log) -> Result<Option<PathBuf>> {
        if self.svc().running(self.app) {
            return Err(service_err("Stop MySQL first."));
        }
        let v = self.version()?;
        let dd = mysql_data_dir(self.app, &v);
        if dd.exists() {
            let dest = self.app.paths.backup.join(format!("broken-{}-{}", dd.file_name().unwrap().to_string_lossy(), timestamp()));
            fs::create_dir_all(&self.app.paths.backup)?;
            fs::rename(&dd, &dest)?;
            log(&format!("Moved the old data folder to {}", dest.display()));
        }
        self.svc().start(self.app, log)?;
        Ok(newest_backup(&self.app.paths.backup.join("mysql")))
    }
}

pub fn newest_backup(folder: &Path) -> Option<PathBuf> {
    let mut runs: Vec<PathBuf> = fs::read_dir(folder).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    runs.sort();
    runs.pop()
}

pub fn postgres_backup(app: &App, log: Log) -> Result<PathBuf> {
    let svc = app.svc("postgresql");
    if !svc.running(app) {
        return Err(service_err("Start PostgreSQL first."));
    }
    let v = svc.version(app).ok_or_else(|| service_err("PostgreSQL is not installed."))?;
    let out = app.paths.backup.join("postgresql").join(timestamp());
    fs::create_dir_all(&out)?;
    let c = command(v.path.join("bin").join(exe("pg_dumpall")), [
        "-h".to_string(), "127.0.0.1".into(), "-p".into(), svc.ports(app)[0].to_string(), "-U".into(), "postgres".into(),
        "-f".into(), out.join("all.sql").display().to_string(),
    ]);
    let r = run(c, Duration::from_secs(3600))?;
    if !r.ok() {
        return Err(service_err(r.message()));
    }
    log(&format!("PostgreSQL backed up to {}", out.display()));
    prune(&app.paths.backup.join("postgresql"), app.settings.u64("backup.keep", 5) as usize);
    Ok(out)
}

pub fn prune(folder: &Path, keep: usize) {
    let Ok(rd) = fs::read_dir(folder) else { return };
    let mut runs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    runs.sort();
    runs.reverse();
    for old in runs.into_iter().skip(keep.max(1)) {
        let _ = fs::remove_dir_all(old);
    }
}

/// Back up running databases if the interval has passed (or `force`).
pub fn auto_backup(app: &App, log: Log, force: bool) -> Result<()> {
    if !force && !app.settings.bool("backup.enabled") {
        return Ok(());
    }
    let hours = app.settings.u64("backup.interval_hours", 8);
    let last = app.state.value("last_backup").as_u64().unwrap_or(0);
    if !force && now_secs().saturating_sub(last) < hours * 3600 {
        return Ok(());
    }
    let mut did = false;
    if app.svc("mysql").running(app) {
        MySqlTools::new(app).backup_all(log)?;
        did = true;
    }
    if app.svc("postgresql").running(app) {
        postgres_backup(app, log)?;
        did = true;
    }
    if did {
        app.state.put("last_backup", json!(now_secs()))?;
    } else if force {
        log("No database is running, nothing to back up.");
    }
    Ok(())
}
