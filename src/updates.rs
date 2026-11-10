//! Daily background checks: newest PHP patches and the CA bundle (cacert.pem) that curl,
//! Composer and PHP's openssl use.

use std::collections::HashMap;
use std::fs;

use anyhow::{bail, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::core::App;
use crate::packages::{self, Package};
use crate::util::{hex, now_secs, version_key};
use crate::Log;

pub fn update_cacert(app: &App, log: Log) -> Result<bool> {
    let pem = packages::http_get("https://curl.se/ca/cacert.pem", None)?;
    let expected = String::from_utf8_lossy(&packages::http_get("https://curl.se/ca/cacert.pem.sha256", None)?)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    if hex(&Sha256::digest(&pem)) != expected {
        bail!("cacert.pem checksum mismatch; keeping the old file.");
    }
    let dest = app.paths.ssl.join("cacert.pem");
    if fs::read(&dest).map(|cur| cur == pem).unwrap_or(false) {
        return Ok(false);
    }
    fs::create_dir_all(&app.paths.ssl)?;
    fs::write(&dest, &pem)?;
    log("cacert.pem updated");
    Ok(true)
}

pub fn php_updates(app: &App, log: Log, install: bool) -> Result<Vec<String>> {
    let (_, data) = packages::php_releases()?;
    let mut best: HashMap<String, Vec<u64>> = HashMap::new();
    for v in app.versions("php") {
        let e = best.entry(v.series()).or_default();
        if v.key() > *e {
            *e = v.key();
        }
    }
    let active = app.active("php");
    let mut notes = vec![];
    let mut series_list: Vec<_> = best.iter().collect();
    series_list.sort();
    for (series, have) in series_list {
        let Some(rel) = data.get(series) else { continue };
        let latest = rel["version"].as_str().unwrap_or("").to_string();
        if version_key(&latest) <= *have {
            continue;
        }
        if !install {
            notes.push(format!("PHP {latest} is available"));
            continue;
        }
        let pkg = Package { id: format!("php-{series}"), label: format!("PHP {series}"), tool: "php".into(),
                            source: json!({"type": "php", "series": series}) };
        let path = packages::install(app, &pkg, log)?;
        notes.push(format!("Added PHP {latest}"));
        if app.settings.bool("php.auto_switch") && active.as_ref().map(|a| &a.series() == series).unwrap_or(false) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            app.switch_version("php", &name, log)?;
        }
    }
    if let Some(newest) = data.as_object().and_then(|o| o.keys().max_by_key(|k| version_key(k))) {
        if !best.contains_key(newest) {
            notes.push(format!("PHP {newest} is available on the Install page"));
        }
    }
    Ok(notes)
}

/// At most once a day (unless forced): refresh cacert.pem, look for new series of every tool
/// (and Laravel majors), and add new PHP patches.
pub fn daily(app: &App, log: Log, force: bool) -> Result<Vec<String>> {
    let last = app.state.value("last_update_check").as_u64().unwrap_or(0);
    if !force && now_secs().saturating_sub(last) < 86400 {
        return Ok(vec![]);
    }
    let mut notes = vec![];
    if app.settings.bool("updates.cacert") {
        match update_cacert(app, log) {
            Ok(true) => {
                app.generate(log)?;
            }
            Ok(false) => {}
            Err(e) => log(&format!("cacert.pem update failed: {e:#}")),
        }
    }
    match packages::discover(app, log) {
        Ok(0) => {}
        Ok(n) => notes.push(format!("{n} new version{} on the Install page", if n == 1 { "" } else { "s" })),
        Err(e) => log(&format!("Checking for new versions failed: {e:#}")),
    }
    if app.settings.bool("php.auto_update") && !app.versions("php").is_empty() {
        match php_updates(app, log, true) {
            Ok(n) => notes.extend(n),
            Err(e) => log(&format!("PHP update check failed: {e:#}")),
        }
    }
    app.state.put("last_update_check", json!(now_secs()))?;
    for n in &notes {
        log(n);
    }
    Ok(notes)
}
