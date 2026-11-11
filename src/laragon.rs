//! Working alongside an existing Laragon installation:
//!
//! * its projects folder (www) can be devstack's projects folder,
//! * its installed versions (PHP, MySQL/MariaDB, Node.js, Python, …) are listed next to
//!   devstack's and used in place; devstack never writes into them (php.ini for Laragon's PHP
//!   goes to devstack's etc/php/<version>/),
//! * its database folders are used in place by the matching MySQL/MariaDB/PostgreSQL series
//!   (Laragon's `mysql-8` holds MySQL 8.0). A series Laragon has no folder for gets one in
//!   devstack's own data folder.
//!
//! Laragon and devstack must not run at the same time: same ports, same database folders.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::core::App;
use crate::versions::Version;
use crate::Log;

pub fn looks_like_laragon(p: &Path) -> bool {
    p.join("laragon.exe").is_file()
}

/// The Laragon folder devstack is set to use (only if it still looks like Laragon).
pub fn root(app: &App) -> Option<PathBuf> {
    let p = app.settings.str("laragon_dir");
    let p = p.trim();
    (!p.is_empty()).then(|| PathBuf::from(p)).filter(|p| looks_like_laragon(p))
}

/// The configured Laragon folder, else one found where the projects folder already points
/// (…\laragon\www), above or next to devstack's folder, or in the usual places (C:\laragon,
/// D:\laragon, …).
pub fn detect(app: &App) -> Option<PathBuf> {
    if let Some(r) = root(app) {
        return Some(r);
    }
    let mut candidates: Vec<PathBuf> = vec![];
    let www = app.settings.str("www_dir");
    if let Some(parent) = Path::new(www.trim()).parent() {
        candidates.push(parent.to_path_buf());
    }
    for dir in app.paths.root.ancestors() {
        candidates.push(dir.to_path_buf());
        candidates.push(dir.join("laragon"));
    }
    for drive in ['C', 'D', 'E', 'F', 'G'] {
        candidates.push(PathBuf::from(format!(r"{drive}:\laragon")));
    }
    candidates.into_iter().find(|p| looks_like_laragon(p))
}

pub fn versions_on(app: &App) -> bool {
    app.settings.bool("laragon.versions")
}

pub fn data_on(app: &App) -> bool {
    app.settings.bool("laragon.data")
}

/// Laragon's versions of a tool, when "Use Laragon's installed versions" is on. Not for
/// phpMyAdmin: devstack writes its config into the phpMyAdmin folder.
pub fn versions(app: &App, tool: &str) -> Vec<Version> {
    if tool == "phpmyadmin" || !versions_on(app) {
        return vec![];
    }
    let Some(root) = root(app) else { return vec![] };
    crate::versions::list(&root.join("bin"), tool)
        .into_iter()
        .map(|mut v| {
            v.external = true;
            v
        })
        .collect()
}

/// Laragon's database folder for a series, when "Use Laragon's databases" is on and Laragon
/// has one. `kind` is "mysql", "mariadb" or "postgresql".
pub fn data_dir(app: &App, kind: &str, series: &str) -> Option<PathBuf> {
    if !data_on(app) {
        return None;
    }
    let data = root(app)?.join("data");
    let major = series.split('.').next().unwrap_or("");
    let mut names = vec![format!("{kind}-{series}")];
    match kind {
        "mysql" if series == "8.0" => names.push("mysql-8".into()), // Laragon's name for 8.0
        "mysql" if major == "5" => names.push("mysql".into()),
        "postgresql" => names.push("postgresql".into()),
        _ => {}
    }
    // Only folders that really hold a database (MySQL's system schema, PostgreSQL's PG_VERSION).
    let real = |d: &Path| if kind == "postgresql" { d.join("PG_VERSION").is_file() } else { d.join("mysql").is_dir() };
    names.into_iter().map(|n| data.join(n)).find(|d| real(d))
}

/// Laragon's own builds of a tool, whether or not devstack is set to list them.
fn own_versions(app: &App, tool: &str) -> Vec<Version> {
    root(app).map(|r| crate::versions::list(&r.join("bin"), tool)).unwrap_or_default()
}

/// Refuse to open Laragon's databases with a server build Laragon doesn't have. MySQL and
/// MariaDB upgrade the data files the first time a newer build starts on them, and Laragon's
/// older build may then refuse to open them. Laragon's own builds, and devstack builds with
/// the same version number, are fine; "Let other versions upgrade its databases" allows the rest.
pub fn check_data_version(app: &App, v: &Version, data_dir: &Path) -> Result<()> {
    if !owns(app, data_dir) || v.external || app.settings.bool("laragon.allow_upgrade") {
        return Ok(());
    }
    let maria = crate::services::mysql_is_mariadb(v);
    let theirs: Vec<String> = own_versions(app, "mysql")
        .into_iter()
        .filter(|x| x.series() == v.series() && crate::services::mysql_is_mariadb(x) == maria)
        .map(|x| x.number())
        .collect();
    if theirs.contains(&v.number()) {
        return Ok(());
    }
    Err(crate::error::service_err(format!(
        "{} would open Laragon's databases in {} and upgrade them; Laragon's own version ({}) may then refuse to open them. \
         Pick Laragon's version on the Overview (marked Laragon), or turn on \"Let other versions upgrade its databases\" in Settings > Laragon.",
        v.name,
        data_dir.display(),
        if theirs.is_empty() { "none for this series".to_string() } else { theirs.join(", ") }
    )))
}

/// The MySQL/MariaDB build whose Laragon data folder was used most recently.
fn last_used_database(app: &App) -> Option<String> {
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for v in own_versions(app, "mysql") {
        let kind = if crate::services::mysql_is_mariadb(&v) { "mariadb" } else { "mysql" };
        let Some(dir) = data_dir(app, kind, &v.series()) else { continue };
        let Ok(when) = std::fs::metadata(&dir).and_then(|m| m.modified()) else { continue };
        // newest folder wins; for the same folder the newest build (the list is newest first)
        if best.as_ref().is_none_or(|(t, _)| when > *t) {
            best = Some((when, v.name.clone()));
        }
    }
    best.map(|(_, name)| name)
}

/// True if `path` is inside the Laragon folder in use (and not inside devstack's own folder,
/// which may itself sit somewhere under Laragon's, e.g. in its www).
pub fn owns(app: &App, path: &Path) -> bool {
    root(app).is_some_and(|r| path.starts_with(&r)) && !path.starts_with(&app.paths.root)
}

/// Use Laragon's projects, installed versions and databases.
pub fn use_all(app: &App, root: &Path, log: Log) -> Result<()> {
    if !looks_like_laragon(root) {
        return Err(anyhow!("{} doesn't look like a Laragon folder (no laragon.exe).", root.display()));
    }
    app.settings.put("laragon_dir", json!(root.display().to_string()));
    app.settings.put("www_dir", json!(root.join("www").display().to_string()));
    app.settings.put("laragon.versions", json!(true));
    app.settings.put("laragon.data", json!(true));
    app.settings.put("laragon.asked", json!(true));
    app.settings.save()?;
    app.paths().ensure()?;
    // With no MySQL choice saved yet, start with the one Laragon was using last, so the same
    // databases show up.
    if app.settings.str("versions.mysql").is_empty() {
        if let Some(name) = last_used_database(app) {
            app.settings.set("versions.mysql", json!(name))?;
        }
    }
    log(&format!(
        "Using Laragon at {}: its projects, installed versions and databases. Don't run Laragon at the same time.",
        root.display()
    ));
    Ok(())
}

/// Stop using Laragon's folders (devstack's own www, versions and data folders again).
pub fn stop_using(app: &App, log: Log) -> Result<()> {
    if let Some(r) = root(app) {
        if Path::new(&app.settings.str("www_dir")) == r.join("www") {
            app.settings.put("www_dir", json!(""));
        }
    }
    app.settings.put("laragon.versions", json!(false));
    app.settings.put("laragon.data", json!(false));
    app.settings.save()?;
    app.paths().ensure()?;
    log("No longer using Laragon's folders.");
    Ok(())
}
