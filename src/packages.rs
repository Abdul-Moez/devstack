//! Quick add: download and install tools into bin/<tool>/<version>/.
//!
//! The catalogue is embedded (assets/packages.json). Add or override entries in
//! usr/packages.json (same format; an entry with the same "id" replaces the built-in one).
//!
//! Source types: php, node, github, composer, go, phpmyadmin, postgresql, mongodb, scrape, url.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::assets;
use crate::core::App;
use crate::error::package_err;
use crate::util::{hex, load_json, parse_version, safe_file_name, version_key};
use crate::versions::{candidates, Version};
use crate::Log;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) devstack/0.1";
pub const PHP_MIRRORS: &[&str] = &["https://downloads.php.net/~windows/releases/", "https://windows.php.net/downloads/releases/"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    pub label: String,
    pub tool: String,
    pub source: Value,
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub url: String,
    pub folder: String,
    /// Published checksums (hex). Whichever is present is verified after the download.
    pub sha256: Option<String>,
    pub sha512: Option<String>,
    pub filename: Option<String>,
}

// ------------------------------------------------------------------ http
fn client() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| {
        Client::builder()
            .user_agent(UA)
            // Also refuses a redirect from https:// to http://: several downloads have no
            // published checksum, so the connection is what vouches for them.
            .https_only(true)
            .connect_timeout(Duration::from_secs(30))
            .timeout(None)
            .build()
            .expect("http client")
    })
}

fn get(url: &str, referer: Option<&str>) -> Result<reqwest::blocking::Response> {
    let mut req = client().get(url);
    if let Some(r) = referer {
        req = req.header("Referer", r);
    }
    if url.contains("api.github.com") {
        req = req.header("Accept", "application/vnd.github+json");
    }
    let resp = req.send().with_context(|| format!("couldn't reach {url}"))?;
    resp.error_for_status().with_context(|| format!("download failed: {url}"))
}

pub fn http_get(url: &str, referer: Option<&str>) -> Result<Vec<u8>> {
    let mut r = get(url, referer)?;
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn http_json(url: &str) -> Result<Value> {
    serde_json::from_slice(&http_get(url, None)?).with_context(|| format!("{url} did not return JSON"))
}

pub fn download(url: &str, dest: &Path, referer: Option<&str>, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    if !url.starts_with("https://") {
        return Err(package_err(format!("Refusing to download over plain HTTP: {url}")));
    }
    fs::create_dir_all(dest.parent().unwrap())?;
    let part = dest.with_extension("part");
    let result = (|| -> Result<()> {
        let mut r = get(url, referer)?;
        let total = r.content_length().unwrap_or(0);
        let mut f = fs::File::create(&part)?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
        drop(f);
        fs::rename(&part, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// The SHA-256 for `file` from a published checksum list (`<hash>  <file name>` per line,
/// as in sha256sum output, Node's SHASUMS256.txt, Composer's and phpMyAdmin's .sha256 files).
pub fn published_sha256(list_url: &str, file: &str) -> Result<String> {
    let text = String::from_utf8_lossy(&http_get(list_url, None)?).into_owned();
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?, parts.next()?.trim_start_matches('*')))
        })
        .find(|(hash, name)| *name == file && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|(hash, _)| hash.to_lowercase())
        .ok_or_else(|| package_err(format!("No checksum for {file} in {list_url}.")))
}

pub fn sha512_of(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = sha2::Sha512::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

pub fn sha256_of(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

// ------------------------------------------------------------------ catalogue
/// Built-in entries, then series found by `discover` (cached in tmp/discovered.json), then
/// usr/packages.json (an entry with the same id replaces the built-in one).
pub fn catalogue(app: &App) -> Vec<Package> {
    let mut items: Vec<Package> = serde_json::from_str(assets::PACKAGES_JSON).unwrap_or_default();
    let found: Vec<Package> = load_json(&discovered_path(app))
        .and_then(|v| serde_json::from_value(v["packages"].clone()).ok())
        .unwrap_or_default();
    for p in found.into_iter().filter(trusted_discovered) {
        if !items.iter().any(|i| i.id == p.id) {
            let at = insert_position(&items, &p);
            items.insert(at, p);
        }
    }
    if let Some(extra) = load_json(&app.paths.usr.join("packages.json")) {
        let extra: Vec<Package> = serde_json::from_value(extra).unwrap_or_default();
        for e in extra {
            match items.iter_mut().find(|i| i.id == e.id) {
                Some(i) => *i = e,
                None => items.push(e),
            }
        }
    }
    items
}

/// A series or major the way the sources write it: "8.6", "3.16", "28".
fn plain_series(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{1,3}(\.\d{1,3})?$").unwrap()).is_match(s)
}

/// tmp/discovered.json is written by `discover`, but it is still a file on disk. Only entries
/// of the exact shapes `discover` writes are accepted from it; anything else (another source
/// type, a URL, odd names) is ignored. Your own entries belong in usr/packages.json.
fn trusted_discovered(p: &Package) -> bool {
    let series = match p.source.get("series").or_else(|| p.source.get("major")) {
        Some(Value::String(x)) => x.trim_end_matches('.').to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return false,
    };
    let known_pair = matches!(
        (p.tool.as_str(), s(&p.source, "type")),
        ("php", "php") | ("python", "python") | ("nodejs", "node") | ("mysql", "mariadb") | ("postgresql", "postgresql") | ("mongodb", "mongodb")
    );
    known_pair
        && plain_series(&series)
        && safe_file_name(&p.id)
        && p.label.len() <= 60
        && !p.label.chars().any(char::is_control)
        && p.source.as_object().is_some_and(|o| o.keys().all(|k| matches!(k.as_str(), "type" | "series" | "major")))
}

/// Version of an entry's series ("8.6", 28, …) for ordering; empty for "lts"/"newest" entries.
fn series_key(p: &Package) -> Vec<u64> {
    let v = p.source.get("series").or_else(|| p.source.get("major"));
    let s = match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let key = version_key(&s);
    if key.is_empty() {
        s.parse::<u64>().map(|n| vec![n]).unwrap_or_default() // a bare major like 28
    } else {
        key
    }
}

/// Where a discovered entry goes: among its tool's entries, before the first older series
/// (the built-in lists run newest first), or after the tool's last entry.
fn insert_position(items: &[Package], p: &Package) -> usize {
    let key = series_key(p);
    let same: Vec<usize> = items.iter().enumerate().filter(|(_, i)| i.tool == p.tool).map(|(n, _)| n).collect();
    same.iter()
        .copied()
        .find(|&n| {
            let k = series_key(&items[n]);
            !k.is_empty() && k < key
        })
        .or_else(|| same.last().map(|n| n + 1))
        .unwrap_or(items.len())
}

// ------------------------------------------------------------------ discovery
fn discovered_path(app: &App) -> PathBuf {
    app.paths.tmp.join("discovered.json")
}

/// The Laravel recipes found by the last `discover` (empty if it never ran).
pub fn discovered_laravel(app: &App) -> Vec<crate::quickapp::Recipe> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    static PACKAGE: OnceLock<Regex> = OnceLock::new();
    let name = NAME.get_or_init(|| Regex::new(r"^Laravel \d{1,3}(\.\d)?( LTS)?( \(PHP \d{1,2}\.\d{1,2}\+\))?$").unwrap());
    let package = PACKAGE.get_or_init(|| Regex::new(r"^laravel/laravel:(\^\d{1,3}\.0|5\.\d\.\*)$").unwrap());
    let all: Vec<crate::quickapp::Recipe> =
        load_json(&discovered_path(app)).and_then(|v| serde_json::from_value(v["laravel"].clone()).ok()).unwrap_or_default();
    // Only plain "composer create-project laravel/laravel:<version>" recipes are taken from
    // this generated file: never a command, a URL or pip packages.
    all.into_iter()
        .filter(|r| {
            r.kind == "composer"
                && name.is_match(&r.name)
                && r.package.as_deref().is_some_and(|p| package.is_match(p))
                && r.url.is_none()
                && r.command.is_none()
                && r.run.is_none()
                && r.pip.is_empty()
                && r.commands.is_empty()
                && !r.wordpress
        })
        .collect()
}

fn pkg(id: String, label: String, tool: &str, source: Value) -> Package {
    Package { id, label, tool: tool.into(), source }
}

/// Series the sources publish that the catalogue doesn't have yet. Pure, so it can be tested
/// with saved responses; `None` means that source couldn't be reached.
pub fn new_packages(
    have: &std::collections::HashSet<String>,
    php: Option<&Value>,
    nuget: Option<&Value>,
    node: Option<&Value>,
    mariadb: Option<&Value>,
    postgres: Option<&Value>,
    mongo: Option<&Value>,
) -> Vec<Package> {
    let mut out: Vec<Package> = vec![];
    let mut add = |p: Package| {
        if !have.contains(&p.id) && !out.iter().any(|o: &Package| o.id == p.id) {
            out.push(p);
        }
    };
    let series_of_version = |v: &str| v.split('.').take(2).collect::<Vec<_>>().join(".");
    if let Some(o) = php.and_then(Value::as_object) {
        for s in o.keys().filter(|s| plain_series(s) && s.contains('.')) {
            add(pkg(format!("php-{s}"), format!("PHP {s} (latest patch)"), "php", json!({"type": "php", "series": s})));
        }
    }
    if let Some(list) = nuget.and_then(|v| v["versions"].as_array()) {
        for s in list.iter().filter_map(Value::as_str).filter(|v| !v.contains('-')).map(series_of_version) {
            if plain_series(&s) && version_key(&s) >= vec![3, 8] {
                add(pkg(format!("python-{s}"), format!("Python {s}"), "python", json!({"type": "python", "series": s})));
            }
        }
    }
    if let Some(list) = node.and_then(Value::as_array) {
        let majors: Vec<u64> = list
            .iter()
            .filter(|r| r["files"].as_array().is_some_and(|f| f.iter().any(|x| x == "win-x64-zip")))
            .filter_map(|r| s(r, "version").trim_start_matches('v').split('.').next().and_then(|m| m.parse().ok()))
            .collect();
        let newest = majors.iter().copied().max().unwrap_or(0);
        // Even majors become LTS; odd ones are short-lived, so only the newest is offered.
        for m in majors.into_iter().filter(|m| *m >= 16 && (m % 2 == 0 || *m == newest)) {
            add(pkg(format!("nodejs-{m}"), format!("Node.js {m}"), "nodejs", json!({"type": "node", "major": m})));
        }
    }
    if let Some(list) = mariadb.and_then(|v| v["major_releases"].as_array()) {
        for r in list.iter().filter(|r| s(r, "release_status") == "Stable") {
            let id = s(r, "release_id");
            if !plain_series(id) {
                continue;
            }
            let lts = s(r, "release_support_type").contains("Long Term");
            add(pkg(format!("mariadb-{id}"), format!("MariaDB {id}{}", if lts { " LTS" } else { "" }), "mysql", json!({"type": "mariadb", "series": id})));
        }
    }
    if let Some(list) = postgres.and_then(Value::as_array) {
        for r in list.iter().filter(|r| r["supported"].as_bool().unwrap_or(false)) {
            let major = r["major"].to_string().trim_matches('"').to_string();
            if !plain_series(&major) {
                continue;
            }
            add(pkg(format!("postgresql-{major}"), format!("PostgreSQL {major}"), "postgresql", json!({"type": "postgresql", "major": major})));
        }
    }
    if let Some(list) = mongo.and_then(|v| v["versions"].as_array()) {
        for v in list.iter().filter(|v| v["production_release"].as_bool().unwrap_or(false)) {
            let series = series_of_version(s(v, "version"));
            if plain_series(&series) && version_key(&series) >= vec![6, 0] {
                add(pkg(format!("mongodb-{series}"), format!("MongoDB {series}"), "mongodb", json!({"type": "mongodb", "series": format!("{series}.")})));
            }
        }
    }
    out
}

/// Look for new series (PHP 8.6, Python 3.16, Node 28, MariaDB 13, PostgreSQL 19, Laravel 14,
/// …) and remember them in tmp/discovered.json, so they appear on the Install and New project
/// pages without a devstack update. Sources that can't be reached are skipped.
pub fn discover(app: &App, log: Log) -> Result<usize> {
    let mut have: std::collections::HashSet<String> =
        serde_json::from_str::<Vec<Package>>(assets::PACKAGES_JSON).unwrap_or_default().into_iter().map(|p| p.id).collect();
    if let Some(extra) = load_json(&app.paths.usr.join("packages.json")).and_then(|v| serde_json::from_value::<Vec<Package>>(v).ok()) {
        have.extend(extra.into_iter().map(|p| p.id));
    }
    let fetch = |name: &str, url: &str| match http_json(url) {
        Ok(v) => Some(v),
        Err(e) => {
            log(&format!("Couldn't check {name} for new versions: {e:#}"));
            None
        }
    };
    let php = php_releases().map(|(_, v)| v).map_err(|e| log(&format!("Couldn't check PHP for new versions: {e:#}"))).ok();
    let found = new_packages(
        &have,
        php.as_ref(),
        fetch("Python", "https://api.nuget.org/v3-flatcontainer/python/index.json").as_ref(),
        fetch("Node.js", "https://nodejs.org/dist/index.json").as_ref(),
        fetch("MariaDB", "https://downloads.mariadb.org/rest-api/mariadb/").as_ref(),
        fetch("PostgreSQL", "https://www.postgresql.org/versions.json").as_ref(),
        fetch("MongoDB", "https://downloads.mongodb.org/current.json").as_ref(),
    );
    let laravel = fetch("Laravel", "https://repo.packagist.org/p2/laravel/laravel.json")
        .map(|v| crate::quickapp::laravel_recipes(&v))
        .unwrap_or_else(|| discovered_laravel(app));
    let before: Vec<String> = load_json(&discovered_path(app))
        .and_then(|v| serde_json::from_value::<Vec<Package>>(v["packages"].clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.id)
        .collect();
    let fresh: Vec<&Package> = found.iter().filter(|p| !before.contains(&p.id)).collect();
    for p in &fresh {
        log(&format!("New on the Install page: {}", p.label));
    }
    let doc = json!({"checked": crate::util::now_secs(), "packages": found, "laravel": laravel});
    crate::util::write_if_changed(&discovered_path(app), &serde_json::to_string_pretty(&doc)?)?;
    Ok(fresh.len())
}

pub fn find(app: &App, id: &str) -> Result<Package> {
    catalogue(app)
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| package_err(format!("Unknown package '{id}'. Run 'ds packages' to list them.")))
}

// ------------------------------------------------------------------ resolvers
/// (base URL, releases.json) from the first PHP-for-Windows mirror that answers.
pub fn php_releases() -> Result<(String, Value)> {
    let mut last = None;
    for base in PHP_MIRRORS {
        match http_json(&format!("{base}releases.json")) {
            Ok(v) => return Ok((base.to_string(), v)),
            Err(e) => last = Some(e),
        }
    }
    Err(package_err(format!("Couldn't reach PHP for Windows downloads: {}", last.map(|e| format!("{e:#}")).unwrap_or_default())))
}

/// Newest build of an old PHP series from the archive folder (php.net no longer lists 7.0-7.3
/// in releases.json). The archive publishes no checksums for these.
fn php_archive(base: &str, series: &str, build: &str) -> Result<Resolved> {
    let page = format!("{base}archives/");
    let html = String::from_utf8_lossy(&http_get(&page, None)?).into_owned();
    let (patch, file) = newest_php_archive(&html, series, build)
        .ok_or_else(|| package_err(format!("No {build} x64 build of PHP {series} on windows.php.net.")))?;
    Ok(Resolved {
        url: format!("{page}{file}"),
        folder: format!("php-{series}.{patch}-{build}"),
        sha256: None,
        sha512: None,
        filename: None,
    })
}

/// (patch, file name) of the newest x64 zip of `series` ("ts" or "nts") in the archive listing.
/// File names look like php-7.1.33-Win32-VC14-x64.zip or php-7.4.33-nts-Win32-vc15-x64.zip.
pub fn newest_php_archive(html: &str, series: &str, build: &str) -> Option<(u64, String)> {
    let nts = if build == "nts" { "nts-" } else { "" };
    let re = Regex::new(&format!(r"php-{}\.(\d+)-{nts}Win32-(?i:v[cs])\d+-x64\.zip", regex::escape(series))).ok()?;
    re.captures_iter(html).filter_map(|c| Some((c[1].parse::<u64>().ok()?, c[0].to_string()))).max_by_key(|(patch, _)| *patch)
}

/// Python from the official NuGet package (python.org's release managers publish it). Unlike
/// the "embeddable" zip it's a full Python with pip and venv. NuGet lists a SHA-512 per package.
fn python_nuget(series: &str) -> Result<Resolved> {
    let index = http_json("https://api.nuget.org/v3-flatcontainer/python/index.json")?;
    let prefix = format!("{series}.");
    let version = index["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|v| v.starts_with(&prefix) && !v.contains('-')) // no pre-releases
        .max_by_key(|v| version_key(v))
        .ok_or_else(|| package_err(format!("No Python {series} release on nuget.org.")))?
        .to_string();
    let reg = http_json(&format!("https://api.nuget.org/v3/registration5-semver1/python/{version}.json"))?;
    let entry = http_json(s(&reg, "catalogEntry"))?;
    if !s(&entry, "packageHashAlgorithm").eq_ignore_ascii_case("SHA512") {
        return Err(package_err(format!("nuget.org lists no SHA-512 for Python {version}.")));
    }
    let hash = base64::engine::general_purpose::STANDARD
        .decode(s(&entry, "packageHash"))
        .map_err(|e| package_err(format!("Bad checksum for Python {version} on nuget.org: {e}")))?;
    Ok(Resolved {
        url: format!("https://api.nuget.org/v3-flatcontainer/python/{version}/python.{version}.nupkg"),
        folder: format!("python-{version}"),
        sha256: None,
        sha512: Some(hex(&hash)),
        filename: Some(format!("python-{version}.zip")),
    })
}

/// Newest MariaDB release of a series, from MariaDB's download API (which lists a SHA-256).
fn mariadb(series: &str) -> Result<Resolved> {
    let data = http_json(&format!("https://downloads.mariadb.org/rest-api/mariadb/{series}/latest/"))?;
    let releases = data["releases"].as_object().ok_or_else(|| package_err(format!("No MariaDB {series} release found.")))?;
    let (version, rel) = releases
        .iter()
        .max_by_key(|(v, _)| version_key(v))
        .ok_or_else(|| package_err(format!("No MariaDB {series} release found.")))?;
    let want = format!("mariadb-{version}-winx64.zip");
    let file = rel["files"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| s(f, "file_name") == want)
        .ok_or_else(|| package_err(format!("MariaDB {version} has no Windows zip.")))?;
    Ok(Resolved {
        url: s(file, "file_download_url").replacen("http://", "https://", 1),
        folder: format!("mariadb-{version}"),
        sha256: file["checksum"].get("sha256sum").and_then(Value::as_str).map(str::to_string),
        sha512: None,
        filename: Some(want),
    })
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Newest patch of a MySQL series on cdn.mysql.com. dev.mysql.com can't be read, but the CDN
/// serves every release: the current one under Downloads/, older ones under archives/. So
/// walk up from the last patch devstack knew about until a number doesn't exist.
fn mysql_newest(series: &str, from: u64) -> Result<Resolved> {
    let file = |p: u64| format!("mysql-{series}.{p}-winx64.zip");
    let exists = |url: &str| client().head(url).send().is_ok_and(|r| r.status().is_success());
    let current = |p: u64| format!("https://cdn.mysql.com/Downloads/MySQL-{series}/{}", file(p));
    let (mut newest, mut misses) = (None, 0);
    for p in from..from + 40 {
        if exists(&current(p)) {
            newest = Some(p);
            misses = 0;
        } else if newest.is_some() {
            break;
        } else {
            misses += 1;
            if misses > 6 {
                break;
            }
        }
    }
    let (url, patch) = match newest {
        Some(p) => (current(p), p),
        None => {
            let old = format!("https://cdn.mysql.com/archives/mysql-{series}/{}", file(from));
            if !exists(&old) {
                return Err(package_err(format!("Couldn't find MySQL {series} on cdn.mysql.com.")));
            }
            (old, from)
        }
    };
    Ok(Resolved { url, folder: format!("mysql-{series}.{patch}"), sha256: None, sha512: None, filename: None })
}

/// The first URL that answers a HEAD request with success.
fn first_available(urls: &[&str]) -> Option<String> {
    urls.iter()
        .find(|u| client().head(**u).send().is_ok_and(|r| r.status().is_success()))
        .map(|u| (*u).to_string())
}

/// "series" as a string, whether the catalogue wrote "3.12" or 3.12.
fn series_of(src: &Value) -> String {
    match src.get("series") {
        Some(Value::String(x)) => x.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn url_file(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string()
}

pub fn resolve(app: &App, item: &Package) -> Result<Resolved> {
    let src = &item.source;
    let tool = &item.tool;
    match s(src, "type") {
        "php" => {
            let (base, data) = php_releases()?;
            let mut series = src.get("series").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_else(|| "latest".into());
            if series == "latest" {
                series = data.as_object().and_then(|o| o.keys().max_by_key(|k| version_key(k)).cloned()).unwrap_or_default();
            }
            let build = app.settings.str("php.build");
            let build = if build == "nts" { build } else { "ts".to_string() };
            let Some(rel) = data.get(&series) else {
                return php_archive(&base, &series, &build); // 7.0-7.3: only in the archive folder
            };
            // Keys look like "ts-vs16-x64" (PHP 8) or "ts-vc15-x64" (PHP 7.4).
            let re = Regex::new(&format!(r"^{build}-v[cs]\d+-x64$")).unwrap();
            let mut keys: Vec<&String> = rel.as_object().map(|o| o.keys().collect()).unwrap_or_default();
            keys.sort();
            keys.reverse();
            let key = keys.into_iter().find(|k| re.is_match(k)).ok_or_else(|| package_err(format!("No {build} x64 build listed for PHP {series}.")))?;
            let zip = &rel[key]["zip"];
            Ok(Resolved {
                url: format!("{base}{}", s(zip, "path")),
                folder: format!("php-{}-{build}", s(rel, "version")),
                sha256: zip.get("sha256").and_then(Value::as_str).map(str::to_string),
                sha512: None,
                filename: None,
            })
        }
        "node" => {
            let major = src.get("major").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_else(|| "lts".into());
            let idx = http_json("https://nodejs.org/dist/index.json")?;
            for r in idx.as_array().into_iter().flatten() {
                let has_zip = r["files"].as_array().map(|f| f.iter().any(|x| x == "win-x64-zip")).unwrap_or(false);
                let v = s(r, "version");
                let lts = r.get("lts").map(|l| !(l.is_null() || l == &Value::Bool(false))).unwrap_or(false);
                if has_zip && ((major == "lts" && lts) || v.starts_with(&format!("v{major}."))) {
                    let file = format!("node-{v}-win-x64.zip");
                    return Ok(Resolved {
                        url: format!("https://nodejs.org/dist/{v}/{file}"),
                        folder: format!("node-{}", v.trim_start_matches('v')),
                        sha256: Some(published_sha256(&format!("https://nodejs.org/dist/{v}/SHASUMS256.txt"), &file)?),
                        sha512: None,
                        filename: None,
                    });
                }
            }
            Err(package_err(format!("No Node.js {major} release found.")))
        }
        "github" => {
            let repo = s(src, "repo");
            let rel = http_json(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
            let re = Regex::new(s(src, "asset")).map_err(|e| package_err(format!("bad asset regex: {e}")))?;
            let tag = s(&rel, "tag_name").to_string();
            let assets = rel["assets"].as_array().cloned().unwrap_or_default();
            for a in &assets {
                let name = s(a, "name");
                if re.is_match(name) {
                    let digest = s(a, "digest");
                    let version = parse_version(&tag).or_else(|| parse_version(name)).unwrap_or_else(|| tag.clone());
                    return Ok(Resolved {
                        url: s(a, "browser_download_url").to_string(),
                        folder: format!("{tool}-{version}"),
                        sha256: digest.strip_prefix("sha256:").map(str::to_string),
                        sha512: None,
                        filename: Some(name.to_string()),
                    });
                }
            }
            let names: Vec<&str> = assets.iter().map(|a| s(a, "name")).collect();
            Err(package_err(format!("No asset matching {} in {repo} {tag}. Assets: {}. Fix the regex in usr/packages.json.", s(src, "asset"), names.join(", "))))
        }
        "composer" => {
            // {"lts": true} picks Composer 2.2 LTS, the last one that runs on PHP < 7.2.5.
            let v = http_json("https://getcomposer.org/versions")?;
            let want_lts = src.get("lts").and_then(Value::as_bool).unwrap_or(false);
            let stable = v["stable"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|e| !want_lts || e["lts"].as_bool().unwrap_or(false))
                .ok_or_else(|| package_err("No matching Composer release on getcomposer.org."))?;
            let version = s(stable, "version");
            let sums = format!("https://getcomposer.org/download/{version}/composer.phar.sha256sum");
            Ok(Resolved {
                url: format!("https://getcomposer.org{}", s(stable, "path")),
                folder: format!("composer-{version}"),
                sha256: Some(published_sha256(&sums, "composer.phar")?),
                sha512: None,
                filename: Some("composer.phar".into()),
            })
        }
        "python" => python_nuget(&series_of(src)),
        "mariadb" => mariadb(&series_of(src)),
        "mysql" => mysql_newest(&series_of(src), src["from"].as_u64().unwrap_or(0)),
        "go" => {
            let rels = http_json("https://go.dev/dl/?mode=json")?;
            for r in rels.as_array().into_iter().flatten().filter(|r| r["stable"].as_bool().unwrap_or(false)) {
                for f in r["files"].as_array().into_iter().flatten() {
                    if s(f, "os") == "windows" && s(f, "arch") == "amd64" && s(f, "kind") == "archive" {
                        return Ok(Resolved {
                            url: format!("https://go.dev/dl/{}", s(f, "filename")),
                            folder: s(r, "version").replacen("go", "go-", 1),
                            sha256: f.get("sha256").and_then(Value::as_str).map(str::to_string),
                            sha512: None,
                            filename: Some(s(f, "filename").to_string()),
                        });
                    }
                }
            }
            Err(package_err("No stable Go release found."))
        }
        "phpmyadmin" => {
            if src.get("snapshot").and_then(Value::as_bool).unwrap_or(false) {
                return Ok(Resolved {
                    url: "https://files.phpmyadmin.net/snapshots/phpMyAdmin-6.0+snapshot-all-languages.zip".into(),
                    folder: "phpmyadmin-6.0-snapshot".into(),
                    sha256: None,
                    sha512: None,
                    filename: Some("phpMyAdmin-6.0-snapshot.zip".into()),
                });
            }
            let v = http_json("https://www.phpmyadmin.net/home_page/version.json")?;
            let ver = s(&v, "version");
            let file = format!("phpMyAdmin-{ver}-all-languages.zip");
            let url = format!("https://files.phpmyadmin.net/phpMyAdmin/{ver}/{file}");
            Ok(Resolved {
                sha256: Some(published_sha256(&format!("{url}.sha256"), &file)?),
                sha512: None,
                url,
                folder: format!("phpmyadmin-{ver}"),
                filename: None,
            })
        }
        "postgresql" => {
            let major = src["major"].to_string().trim_matches('"').to_string();
            for e in http_json("https://www.postgresql.org/versions.json")?.as_array().into_iter().flatten() {
                if e["major"].to_string().trim_matches('"') == major {
                    let minor = e["latestMinor"].to_string().trim_matches('"').to_string();
                    let v = format!("{major}.{minor}");
                    return Ok(Resolved {
                        url: format!("https://get.enterprisedb.com/postgresql/postgresql-{v}-1-windows-x64-binaries.zip"),
                        folder: format!("postgresql-{v}"),
                        sha256: None,
                        sha512: None,
                        filename: None,
                    });
                }
            }
            Err(package_err(format!("PostgreSQL {major} not found in postgresql.org/versions.json.")))
        }
        "mongodb" => {
            let prefix = s(src, "series");
            let cur = http_json("https://downloads.mongodb.org/current.json")?;
            for ver in cur["versions"].as_array().into_iter().flatten() {
                if !ver["production_release"].as_bool().unwrap_or(false) || !s(ver, "version").starts_with(prefix) {
                    continue;
                }
                for d in ver["downloads"].as_array().into_iter().flatten() {
                    let url = s(&d["archive"], "url");
                    if s(d, "target") == "windows" && s(d, "arch") == "x86_64" && url.ends_with(".zip") {
                        return Ok(Resolved {
                            url: url.to_string(),
                            folder: format!("mongodb-{}", s(ver, "version")),
                            sha256: d["archive"].get("sha256").and_then(Value::as_str).map(str::to_string),
                            sha512: None,
                            filename: None,
                        });
                    }
                }
            }
            Err(package_err("No MongoDB Windows zip found in downloads.mongodb.org/current.json."))
        }
        "scrape" => {
            // The regex either captures a link (group 1) or named groups used by url_template,
            // e.g. {version}, {series} (derived), or any (?P<name>...) group.
            let page = s(src, "page");
            let html = String::from_utf8_lossy(&http_get(page, Some(page))?).into_owned();
            let re = Regex::new(s(src, "pattern")).map_err(|e| package_err(format!("bad pattern: {e}")))?;
            let idx = src.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let caps = re.captures_iter(&html).nth(idx).ok_or_else(|| {
                package_err(format!("Couldn't find a download on {page}. The page may have changed; edit the pattern in usr/packages.json."))
            })?;
            let first = caps.get(1).or_else(|| caps.get(0)).map(|m| m.as_str().to_string()).unwrap_or_default();
            let (url, version) = if let Some(tpl) = src.get("url_template").and_then(Value::as_str) {
                let mut vars: Vec<(String, String)> = re
                    .capture_names()
                    .flatten()
                    .filter_map(|n| caps.name(n).map(|m| (n.to_string(), m.as_str().to_string())))
                    .collect();
                let version = vars.iter().find(|(k, _)| k == "version").map(|(_, v)| v.clone()).or_else(|| parse_version(&first)).unwrap_or_else(|| first.clone());
                let series = version.split('.').take(2).collect::<Vec<_>>().join(".");
                for (k, v) in [("version", version.clone()), ("series", series), ("match", first.clone())] {
                    if !vars.iter().any(|(n, _)| n == k) {
                        vars.push((k.to_string(), v));
                    }
                }
                let mut url = tpl.to_string();
                for (k, v) in &vars {
                    url = url.replace(&format!("{{{k}}}"), v);
                }
                (url, version)
            } else {
                let url = reqwest::Url::parse(page).and_then(|b| b.join(&first)).map(|u| u.to_string()).unwrap_or(first.clone());
                let version = parse_version(&url_file(&url)).unwrap_or_else(|| "latest".into());
                (url, version)
            };
            Ok(Resolved { url, folder: format!("{tool}-{version}"), sha256: None, sha512: None, filename: None })
        }
        "url" => {
            // "urls": the same file at several addresses, first one that exists wins (MySQL
            // moves a release from Downloads/ to archives/ once the next one is out).
            let url = match src.get("urls").and_then(Value::as_array) {
                Some(list) => {
                    let list: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
                    first_available(&list).ok_or_else(|| package_err(format!("None of these addresses answer: {}", list.join(", "))))?
                }
                None => s(src, "url").to_string(),
            };
            let folder = match s(src, "folder") {
                "" => format!("{tool}-{}", parse_version(&url_file(&url)).unwrap_or_else(|| "latest".into())),
                f => f.to_string(),
            };
            Ok(Resolved { sha256: src.get("sha256").and_then(Value::as_str).map(str::to_string), sha512: None, url, folder, filename: None })
        }
        other => Err(package_err(format!("Unknown source type '{other}'."))),
    }
}

// ------------------------------------------------------------------ install
fn extract(zip_path: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::create_dir_all(dest)?;
    let mut archive = zip::ZipArchive::new(fs::File::open(zip_path)?).context("not a valid zip file")?;
    archive.extract(dest).context("extracting archive")?; // rejects unsafe paths
    Ok(())
}

fn find_dir(root: &Path, names: &[String], depth: usize) -> Option<PathBuf> {
    let mut level = vec![root.to_path_buf()];
    for _ in 0..=depth {
        let mut next = vec![];
        for d in &level {
            if names.iter().any(|n| d.join(n).exists()) {
                return Some(d.clone());
            }
            if let Ok(rd) = fs::read_dir(d) {
                next.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        }
        level = next;
    }
    None
}

/// Keep the version that was active before an install. Without a saved choice, the active
/// version is simply the newest one, so installing a newer PHP or MySQL would silently
/// switch to it (for MySQL that means a different, empty data folder).
pub fn pin_version(app: &App, tool: &str, before: Option<Version>, installed: &str) -> Result<()> {
    let key = format!("versions.{tool}");
    let saved = app.settings.str(&key);
    if !saved.is_empty() && app.versions(tool).iter().any(|v| v.name == saved) {
        return Ok(());
    }
    let keep = before.map_or_else(|| installed.to_string(), |v| v.name);
    app.settings.set(&key, json!(keep))
}

pub fn install(app: &App, item: &Package, log: Log) -> Result<PathBuf> {
    log(&format!("Looking up {}…", item.label));
    let r = resolve(app, item)?;
    if !safe_file_name(&r.folder) {
        return Err(package_err(format!("Refusing to install into an unsafe folder name: {:?}", r.folder)));
    }
    let dest_root = app.versions_root(&item.tool);
    let target = dest_root.join(&r.folder);
    if target.exists() {
        log(&format!("{} is already installed.", r.folder));
        return Ok(target);
    }
    let before = app.default_version(&item.tool);
    let mut name = r.filename.clone().unwrap_or_else(|| url_file(&r.url));
    if name.is_empty() || !name.contains('.') {
        name = format!("{}.zip", r.folder);
    }
    if !safe_file_name(&name) {
        return Err(package_err(format!("Refusing to download to an unsafe file name: {name:?}")));
    }
    let file = app.paths.tmp.join("downloads").join(&name);
    log(&format!("Downloading {}", r.url));
    let mut last = u64::MAX;
    let referer = item.source.get("page").and_then(Value::as_str);
    download(&r.url, &file, referer, &mut |done, total| {
        if let Some(step) = (done * 10).checked_div(total) {
            if step != last {
                last = step;
                log(&format!("  {}% of {} MB", step * 10, total / (1024 * 1024)));
            }
        }
    })?;
    for (expected, algo) in [(&r.sha256, "SHA-256"), (&r.sha512, "SHA-512")] {
        let Some(expected) = expected else { continue };
        let got = if algo == "SHA-256" { sha256_of(&file)? } else { sha512_of(&file)? };
        if !got.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&file);
            return Err(package_err(format!("{algo} mismatch for {name} (expected {expected}, got {got}). Download discarded.")));
        }
        log(&format!("  {algo} checksum verified"));
    }
    if name.ends_with(".phar") {
        fs::create_dir_all(&target)?;
        fs::rename(&file, target.join(&name))?;
        if item.tool == "composer" {
            fs::write(target.join("composer.bat"), "@echo off\r\nphp \"%~dp0composer.phar\" %*\r\n")?;
        }
        pin_version(app, &item.tool, before, &r.folder)?;
        log(&format!("Installed {}", r.folder));
        return Ok(target);
    }
    log("  extracting…");
    let ex = app.paths.tmp.join("extract").join(&r.folder);
    extract(&file, &ex)?;
    let cands = candidates(&item.tool);
    let Some(found) = find_dir(&ex, &cands, 3) else {
        let _ = fs::remove_dir_all(&ex);
        let _ = fs::remove_file(&file);
        return Err(package_err(format!("The archive doesn't contain {}.", cands.join(", "))));
    };
    fs::create_dir_all(&dest_root)?;
    fs::rename(&found, &target).with_context(|| format!("moving into {}", target.display()))?;
    let _ = fs::remove_dir_all(&ex);
    let _ = fs::remove_file(&file);
    if item.tool == "python" {
        // The NuGet build has pip but no pip.exe; Scripts\ is on PATH (and where pip puts
        // the commands of packages it installs).
        let scripts = target.join("Scripts");
        fs::create_dir_all(&scripts)?;
        for name in ["pip.cmd", "pip3.cmd"] {
            fs::write(scripts.join(name), "@\"%~dp0..\\python.exe\" -m pip %*\r\n")?;
        }
    }
    pin_version(app, &item.tool, before, &r.folder)?;
    log(&format!("Installed {} {} → {}", item.tool, r.folder, target.display()));
    Ok(target)
}
