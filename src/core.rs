//! [`App`] ties everything together. The CLI and the GUI both drive it.
//!
//! Locking: public operations that change running services take `op_lock` so the GUI's
//! background jobs never run two of them at once. Internal `*_inner` functions assume the
//! lock is held, which is how operations call each other without deadlocking.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::error::{service_err, DsError};
use crate::paths::{find_root, Paths};
use crate::services::{self, php_plan_ports, Service};
use crate::settings::{defaults, secret_defaults, JsonStore, State};
use crate::sites::{self, Site};
use crate::util::{fwd, path_sep, IS_WIN};
use crate::versions::{self, Version};
use crate::{configs, hosts, share, ssl, updates, winutil, Log};

/// Tools a project can pin to its own version.
pub const PROJECT_TOOLS: &[&str] = &["php", "mysql", "postgresql", "nodejs", "python"];

/// Tools exposed on PATH (via bin/current/<tool> junctions) and the subfolders that hold executables.
pub const PATH_TOOLS: &[(&str, &[&str])] = &[
    ("php", &[""]), ("composer", &[""]), ("nodejs", &[""]), ("python", &["", "Scripts"]), ("git", &["cmd"]),
    ("go", &["bin"]), ("mysql", &["bin"]), ("postgresql", &["bin"]), ("mongodb", &["bin"]), ("ngrok", &[""]), ("redis", &[""]),
];

pub struct App {
    /// Folders derived from the root. `www` is the startup value; use [`App::paths`] for the
    /// current one (it can change in Preferences).
    pub paths: Paths,
    pub settings: JsonStore,
    pub secrets: JsonStore,
    pub state: State,
    services: Vec<Box<dyn Service>>,
    op_lock: Mutex<()>,
}

impl App {
    pub fn new(root: Option<PathBuf>) -> Result<Self> {
        // Config files need absolute paths, so a relative --root is resolved here.
        let root = match root {
            Some(r) => {
                fs::create_dir_all(&r)?;
                r.canonicalize().map(crate::paths::strip_verbatim).unwrap_or(r)
            }
            None => find_root(),
        };
        let mut paths = Paths::new(root);
        fs::create_dir_all(&paths.root)?;
        let settings = JsonStore::open(paths.settings_file.clone(), defaults())?;
        if !settings.exists() {
            settings.save()?;
        }
        paths.set_www(&settings.str("www_dir"));
        paths.ensure()?;
        let secrets = JsonStore::open(paths.usr.join("secrets.json"), secret_defaults())?;
        let state = State::open(paths.tmp.join("state.json"))?;
        Ok(App { paths, settings, secrets, state, services: services::all(), op_lock: Mutex::new(()) })
    }

    pub fn lock(&self) -> MutexGuard<'_, ()> {
        self.op_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The operation lock if nothing else holds it (background maintenance uses this).
    pub fn try_lock(&self) -> Option<MutexGuard<'_, ()>> {
        self.op_lock.try_lock().ok()
    }

    /// Paths with the current projects folder.
    pub fn paths(&self) -> Paths {
        let mut p = self.paths.clone();
        p.set_www(&self.settings.str("www_dir"));
        p
    }

    pub fn log_line(&self, msg: &str) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.paths.logs.join("devstack.log")) {
            let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }

    // ------------------------------------------------------------------ lookups
    pub fn services(&self) -> &[Box<dyn Service>] {
        &self.services
    }

    pub fn svc(&self, id: &str) -> &dyn Service {
        self.services.iter().find(|s| s.id() == id).map(|b| b.as_ref()).unwrap_or_else(|| panic!("unknown service {id}"))
    }

    pub fn find_svc(&self, id: &str) -> Result<&dyn Service> {
        self.services.iter().find(|s| s.id() == id).map(|b| b.as_ref()).ok_or_else(|| anyhow!("Unknown service '{id}'"))
    }

    pub fn service_for_tool(&self, tool: &str) -> Option<&dyn Service> {
        self.services.iter().find(|s| s.tool() == tool).map(|b| b.as_ref())
    }

    pub fn versions(&self, tool: &str) -> Vec<Version> {
        versions::list(&self.paths.bin, tool)
    }

    pub fn versions_root(&self, tool: &str) -> PathBuf {
        versions::root(&self.paths.bin, tool)
    }

    /// The version in use: the active project's choice for this tool, else the default.
    pub fn active(&self, tool: &str) -> Option<Version> {
        let all = self.versions(tool);
        if let Some(wanted) = self.project_versions().get(tool) {
            if let Some(v) = all.iter().find(|v| &v.name == wanted) {
                return Some(v.clone());
            }
        }
        let wanted = self.settings.str(&format!("versions.{tool}"));
        all.iter().find(|v| v.name == wanted).cloned().or_else(|| all.into_iter().next())
    }

    /// The default version (what's used when no project is chosen).
    pub fn default_version(&self, tool: &str) -> Option<Version> {
        let all = self.versions(tool);
        let wanted = self.settings.str(&format!("versions.{tool}"));
        all.iter().find(|v| v.name == wanted).cloned().or_else(|| all.into_iter().next())
    }

    /// The chosen project, if any.
    pub fn project(&self) -> Option<String> {
        self.state.value("project")["name"].as_str().map(str::to_string)
    }

    /// Versions the chosen project uses (empty with no project). Stored in tmp/state.json when
    /// the project is chosen, so every lookup doesn't re-read usr/sites.json.
    pub fn project_versions(&self) -> BTreeMap<String, String> {
        serde_json::from_value(self.state.value("project")["versions"].clone()).unwrap_or_default()
    }

    pub fn sites(&self) -> Vec<Site> {
        sites::discover(self)
    }

    pub fn web(&self) -> Option<&dyn Service> {
        match self.settings.str("web_server").as_str() {
            w @ ("apache" | "nginx") => Some(self.svc(w)),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ config
    fn prepare_ssl(&self, sites: &mut [Site]) -> Result<bool> {
        let mut changed = ssl::ensure_cert(&self.paths, "localhost", &["localhost".into(), "127.0.0.1".into()])?.2;
        for s in sites.iter_mut().filter(|s| s.ssl) {
            let mut names = vec![s.host.clone(), format!("*.{}", s.host)];
            names.extend(s.aliases.iter().cloned());
            let (crt, key, ch) = ssl::ensure_cert(&self.paths, &s.host, &names)?;
            s.cert = fwd(crt);
            s.key = fwd(key);
            changed |= ch;
        }
        Ok(changed)
    }

    fn generate_with_sites(&self) -> Result<(HashSet<String>, Vec<Site>)> {
        let mut sites = self.sites();
        let ssl_changed = if self.settings.bool("ssl") { self.prepare_ssl(&mut sites)? } else { false };
        let mut changed = configs::generate(self, &sites)?;
        if ssl_changed {
            changed.insert("apache".into());
            changed.insert("nginx".into());
        }
        Ok((changed, sites))
    }

    /// Render all configs. Returns ids of services whose config changed.
    pub fn generate(&self, _log: Log) -> Result<HashSet<String>> {
        Ok(self.generate_with_sites()?.0)
    }

    /// Everything that must happen before services start or reload.
    fn prepare(&self, log: Log) -> Result<HashSet<String>> {
        if self.settings.bool("updates.cacert") && !self.paths.ssl.join("cacert.pem").exists() {
            if let Err(e) = updates::update_cacert(self, log) {
                log(&format!("Couldn't download cacert.pem ({e:#}); continuing without it."));
            }
        }
        php_plan_ports(self)?;
        let (changed, sites) = self.generate_with_sites()?;
        let names: Vec<String> = sites.iter().flat_map(|s| std::iter::once(s.host.clone()).chain(s.aliases.iter().cloned())).collect();
        if let Err(e) = hosts::sync(self, &names, log) {
            log(&format!("WARNING: {e:#}"));
        }
        self.rebuild_links();
        Ok(changed)
    }

    // ------------------------------------------------------------------ lifecycle
    pub fn start_all(&self, log: Log) -> Result<Vec<String>> {
        let _g = self.lock();
        self.prepare(log)?;
        let mut failed = vec![];
        for s in &self.services {
            if !s.enabled(self) {
                continue;
            }
            if !s.installed(self) {
                log(&format!("{} is enabled but not installed; skipped (Install page).", s.name()));
                continue;
            }
            if let Err(e) = s.start(self, log) {
                failed.push(s.name().to_string());
                log(&format!("ERROR: {e:#}"));
            }
        }
        Ok(failed)
    }

    /// Returns ids of databases that refused to stop cleanly (call again with force).
    pub fn stop_all(&self, log: Log, force: bool) -> Result<Vec<String>> {
        let _g = self.lock();
        let mut pending = vec![];
        for s in self.services.iter().rev() {
            if let Err(e) = s.stop(self, log, force) {
                match e.downcast_ref::<DsError>() {
                    Some(DsError::NeedsForce { sid, msg }) => {
                        pending.push(sid.clone());
                        log(msg);
                    }
                    _ => log(&format!("ERROR: {e:#}")),
                }
            }
        }
        share::stop(self, log)?;
        Ok(pending)
    }

    pub fn start(&self, id: &str, log: Log) -> Result<()> {
        let _g = self.lock();
        self.start_inner(id, log)
    }

    fn start_inner(&self, id: &str, log: Log) -> Result<()> {
        let s = self.find_svc(id)?;
        self.prepare(log)?;
        if id == "apache" || id == "nginx" {
            self.sync_php_pool(log)?;
        }
        s.start(self, log)
    }

    /// Run the php-cgi pool only when the chosen web server needs it.
    fn sync_php_pool(&self, log: Log) -> Result<()> {
        let php = self.svc("php");
        if php.running(self) && !php.enabled(self) {
            php.stop(self, log, false)?;
        } else if php.enabled(self) && php.installed(self) && !php.running(self) {
            php.start(self, log)?;
        }
        Ok(())
    }

    pub fn stop(&self, id: &str, log: Log, force: bool) -> Result<()> {
        let _g = self.lock();
        self.find_svc(id)?.stop(self, log, force)
    }

    pub fn restart(&self, id: &str, log: Log) -> Result<()> {
        let _g = self.lock();
        let s = self.find_svc(id)?;
        s.stop(self, log, false)?;
        self.prepare(log)?;
        s.start(self, log)
    }

    /// Regenerate configs/hosts/certs and restart only what changed.
    pub fn reload(&self, log: Log) -> Result<()> {
        let _g = self.lock();
        self.reload_inner(log)
    }

    fn reload_inner(&self, log: Log) -> Result<()> {
        let changed = self.prepare(log)?;
        let mut touched = false;
        for s in &self.services {
            if changed.contains(s.id()) && s.running(self) {
                touched = true;
                log(&format!("Applying new config to {}…", s.name()));
                if let Err(e) = s.reload(self, log) {
                    log(&format!("ERROR: {e:#}"));
                }
            }
        }
        if !touched {
            log("Reloaded");
        }
        Ok(())
    }

    // ------------------------------------------------------------------ changes
    /// Change the default version of a tool (and restart its service if it's running).
    pub fn switch_version(&self, tool: &str, name: &str, log: Log) -> Result<()> {
        let _g = self.lock();
        if !self.versions(tool).iter().any(|v| v.name == name) {
            return Err(service_err(format!("{name} is not installed. Run 'ds versions {tool}'.")));
        }
        if let (Some(project), Some(theirs)) = (self.project(), self.project_versions().get(tool).cloned()) {
            // The chosen project decides this tool; only the default changes, for later.
            self.settings.set(&format!("versions.{tool}"), json!(name))?;
            log(&format!("Default {tool} is now {name}. Project {project} keeps using {theirs} until you choose another project."));
            return Ok(());
        }
        let svc = self.service_for_tool(tool);
        let was = svc.map(|s| s.running(self)).unwrap_or(false);
        if was {
            svc.unwrap().stop(self, log, false)?;
        }
        self.settings.set(&format!("versions.{tool}"), json!(name))?;
        self.reload_inner(log)?;
        if let Some(s) = svc {
            if was && s.enabled(self) {
                s.start(self, log)?;
            }
        }
        if tool == "php" && self.web().map(|w| w.running(self)).unwrap_or(false) {
            self.sync_php_pool(log)?; // e.g. switched between ts and nts builds
        }
        if tool == "mysql" {
            if let Some(v) = self.active("mysql") {
                let dd = services::mysql_data_dir(self, &v);
                log(&format!("Note: each MySQL series keeps its own data folder ({}).", dd.file_name().unwrap().to_string_lossy()));
            }
        }
        log(&format!("{tool}: now using {name}"));
        Ok(())
    }

    // ------------------------------------------------------------------ projects
    /// Choose a project (a site with its own versions in usr/sites.json), or `None` for the
    /// default versions. Running services whose version changes are stopped and started again
    /// with the new one; the rest just get their configs refreshed. Start all afterwards uses
    /// the project's versions.
    pub fn set_project(&self, name: Option<&str>, log: Log) -> Result<()> {
        let _g = self.lock();
        let wanted: BTreeMap<String, String> = match name {
            None => BTreeMap::new(),
            Some(n) => {
                let site = self
                    .sites()
                    .into_iter()
                    .find(|s| s.name == n)
                    .ok_or_else(|| service_err(format!("There's no project called {n} in www.")))?;
                for (tool, v) in &site.versions {
                    if !self.versions(tool).iter().any(|x| &x.name == v) {
                        return Err(service_err(format!("{n} uses {v}, which isn't installed. Install it, or change the project's versions on the Sites page.")));
                    }
                }
                site.versions
            }
        };
        let after = |tool: &str| wanted.get(tool).cloned().or_else(|| self.default_version(tool).map(|v| v.name));
        let mut restart = vec![];
        for s in self.services.iter().rev() {
            if s.running(self) && s.version(self).map(|v| v.name) != after(s.tool()) {
                s.stop(self, log, false)?; // a database that won't stop asks the user (NeedsForce)
                restart.push(s.id());
            }
        }
        match name {
            Some(n) => self.state.put("project", json!({"name": n, "versions": wanted}))?,
            None => self.state.put("project", Value::Null)?,
        }
        self.reload_inner(log)?; // new php.ini, Apache's PHP module path, PATH links, …
        for id in restart.iter().rev() {
            let s = self.find_svc(id)?;
            if s.enabled(self) {
                s.start(self, log)?;
            }
        }
        match name {
            Some(n) if wanted.is_empty() => log(&format!("Project {n} (it uses the default versions)")),
            Some(n) => log(&format!(
                "Project {n}: {}",
                wanted.iter().map(|(t, v)| format!("{t} {v}")).collect::<Vec<_>>().join(", ")
            )),
            None => log("Using the default versions"),
        }
        Ok(())
    }

    /// Save a project's versions; if it's the chosen project, switch to them right away.
    pub fn save_project_versions(&self, name: &str, versions: &BTreeMap<String, String>, log: Log) -> Result<()> {
        sites::set_versions(self, name, versions)?;
        log(&format!("Saved the versions for {name}"));
        if self.project().as_deref() == Some(name) {
            self.set_project(Some(name), log)?;
        }
        Ok(())
    }

    pub fn set_web_server(&self, which: &str, log: Log) -> Result<()> {
        let _g = self.lock();
        let old = self.web();
        let was = old.map(|o| o.running(self)).unwrap_or(false);
        if let (Some(o), true) = (old, was) {
            o.stop(self, log, false)?;
        }
        self.settings.set("web_server", json!(which))?;
        match self.web() {
            Some(new) if was => self.start_inner(new.id(), log)?,
            _ => {
                let php = self.svc("php");
                if php.running(self) && !php.enabled(self) {
                    php.stop(self, log, false)?;
                }
            }
        }
        log(&format!("Web server: {}", self.web().map(|w| w.name()).unwrap_or("none")));
        Ok(())
    }

    pub fn set_ssl(&self, on: bool, log: Log) -> Result<()> {
        let _g = self.lock();
        let web = self.web();
        let was = web.map(|w| w.running(self)).unwrap_or(false);
        if let (Some(w), true) = (web, was) {
            w.stop(self, log, false)?;
        }
        self.settings.set("ssl", json!(on))?;
        self.reload_inner(log)?;
        if let (Some(w), true) = (web, was) {
            w.start(self, log)?;
        }
        Ok(())
    }

    pub fn toggle_extension(&self, ext: &str, on: bool, log: Log) -> Result<()> {
        let _g = self.lock();
        let v = self.active("php").ok_or_else(|| service_err("PHP is not installed."))?;
        if ext == "xdebug" {
            self.settings.set("php.xdebug", json!(on))?;
        } else {
            let mut enabled = configs::php_enabled_extensions(self, &v);
            if on && !enabled.iter().any(|e| e == ext) {
                enabled.push(ext.to_string());
            }
            if !on {
                enabled.retain(|e| e != ext);
            }
            enabled.sort();
            self.settings.set(&format!("php.extensions.{}", v.series()), json!(enabled))?;
        }
        self.reload_inner(log)?;
        log(&format!("PHP extension {ext} {}", if on { "enabled" } else { "disabled" }));
        Ok(())
    }

    // ------------------------------------------------------------------ PATH / env
    pub fn rebuild_links(&self) {
        let cur = self.paths.bin.join("current");
        for (tool, _) in PATH_TOOLS {
            let link = cur.join(tool);
            let r = match self.active(tool) {
                Some(v) => winutil::make_link(&link, &v.path),
                None => winutil::remove_link(&link),
            };
            if let Err(e) = r {
                self.log_line(&format!("link {} failed: {e:#}", link.display()));
            }
        }
    }

    pub fn path_dirs(&self, via_links: bool) -> Vec<PathBuf> {
        let mut dirs = vec![];
        for (tool, subs) in PATH_TOOLS {
            let Some(v) = self.active(tool) else { continue };
            let base = if via_links { self.paths.bin.join("current").join(tool) } else { v.path.clone() };
            for s in *subs {
                dirs.push(if s.is_empty() { base.clone() } else { base.join(s) });
            }
        }
        dirs.push(self.paths.usr.join("composer").join("vendor").join("bin"));
        dirs
    }

    /// Environment for terminals, Composer and Quick app commands.
    pub fn tool_env(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        let path_key = env.keys().find(|k| k.eq_ignore_ascii_case("PATH")).cloned().unwrap_or_else(|| "PATH".into());
        let old = env.remove(&path_key).unwrap_or_default();
        let ours: Vec<String> = self.path_dirs(false).iter().map(|d| d.display().to_string()).collect();
        env.insert("PATH".into(), format!("{}{}{}", ours.join(path_sep()), path_sep(), old));
        env.insert("COMPOSER_HOME".into(), self.paths.usr.join("composer").display().to_string());
        if let Some(php) = self.active("php") {
            env.insert("PHPRC".into(), php.path.display().to_string());
        }
        env
    }

    pub fn add_to_path(&self, log: Log) -> Result<()> {
        self.rebuild_links();
        winutil::update_user_path(&self.path_dirs(true), &[])?;
        log("Added to your user PATH. Open a new terminal to use php, composer, node, mysql, ...");
        Ok(())
    }

    pub fn remove_from_path(&self, log: Log) -> Result<()> {
        let cur = self.paths.bin.join("current");
        let mut every: Vec<PathBuf> = vec![];
        for (t, subs) in PATH_TOOLS {
            for s in *subs {
                every.push(if s.is_empty() { cur.join(t) } else { cur.join(t).join(s) });
            }
        }
        every.push(self.paths.usr.join("composer").join("vendor").join("bin"));
        winutil::update_user_path(&[], &every)?;
        log("Removed from your user PATH");
        Ok(())
    }

    pub fn open_terminal(&self) -> Result<()> {
        let _ = self.generate(&|_| {});
        winutil::open_terminal(&self.paths().www, &self.tool_env(), &self.settings.str("terminal"))
    }

    pub fn edit(&self, path: &Path) -> Result<()> {
        winutil::edit_file(path, &self.settings.str("editor"))
    }

    pub fn set_autostart(&self, on: bool) -> Result<()> {
        self.settings.set("run_on_startup", json!(on))?;
        winutil::set_autostart(if on { Some(winutil::gui_command(&self.paths.root)) } else { None }.as_deref())
    }

    // ------------------------------------------------------------------ diagnostics
    pub fn doctor(&self) -> Vec<(bool, String)> {
        let mut out = vec![(true, format!("devstack {}", crate::VERSION))];
        let table = crate::procs::listening();
        for s in &self.services {
            let v = s.version(self);
            if s.enabled(self) || v.is_some() {
                out.push((v.is_some(), format!("{}: {}{}", s.name(), v.as_ref().map(|v| v.name.clone()).unwrap_or_else(|| "not installed".into()),
                                                if s.enabled(self) { "" } else { " (disabled)" })));
            }
            if s.enabled(self) && v.is_some() && !s.running(self) {
                for p in s.ports(self).into_iter().take(2) {
                    if let Some(owner) = crate::procs::port_owner(p, &table) {
                        out.push((false, format!("Port {p} for {} is taken by {owner}", s.name())));
                    }
                }
            }
        }
        out.push((self.paths.ssl.join("cacert.pem").exists(), "cacert.pem present".into()));
        if self.settings.bool("ssl") && IS_WIN {
            match ssl::is_trusted(&self.paths) {
                Ok(t) => out.push((t, "Local CA trusted by Windows".into())),
                Err(e) => out.push((false, format!("Couldn't check CA trust: {e:#}"))),
            }
        }
        if IS_WIN {
            const VC_URL: &str = "https://aka.ms/vc14/vc_redist.x64.exe";
            // Apache Lounge's current (VS2026 / VS18) builds need a 14.50+ runtime.
            out.push(match winutil::vc_runtime_version() {
                Some((14, minor, bld)) if minor >= 50 => (true, format!("Visual C++ runtime 14.{minor}.{bld} installed")),
                Some((maj, minor, bld)) => (false, format!("Visual C++ runtime {maj}.{minor}.{bld} is too old for current Apache builds: update from {VC_URL}")),
                None => (false, format!("Visual C++ runtime missing: install it from {VC_URL}")),
            });
            if let Some(msg) = self.active("mysql").and_then(|v| services::mysql_missing_runtime(&v)) {
                out.push((false, msg));
            }
        }
        if self.active("ngrok").is_some() {
            out.push((!self.secrets.str("ngrok_authtoken").is_empty(), "ngrok authtoken set".into()));
        }
        out
    }
}
