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
    pub sha256: Option<String>,
    pub filename: Option<String>,
}

// ------------------------------------------------------------------ http
fn client() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| {
        Client::builder()
            .user_agent(UA)
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
pub fn catalogue(app: &App) -> Vec<Package> {
    let mut items: Vec<Package> = serde_json::from_str(assets::PACKAGES_JSON).unwrap_or_default();
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

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
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
            let rel = data.get(&series).ok_or_else(|| package_err(format!("PHP {series} is not on windows.php.net any more (end of life?).")))?;
            let build = app.settings.str("php.build");
            let build = if build.is_empty() { "ts".to_string() } else { build };
            let re = Regex::new(&format!(r"^{build}-vs\d+-x64$")).unwrap();
            let mut keys: Vec<&String> = rel.as_object().map(|o| o.keys().collect()).unwrap_or_default();
            keys.sort();
            keys.reverse();
            let key = keys.into_iter().find(|k| re.is_match(k)).ok_or_else(|| package_err(format!("No {build} x64 build listed for PHP {series}.")))?;
            let zip = &rel[key]["zip"];
            Ok(Resolved {
                url: format!("{base}{}", s(zip, "path")),
                folder: format!("php-{}-{build}", s(rel, "version")),
                sha256: zip.get("sha256").and_then(Value::as_str).map(str::to_string),
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
                        filename: Some(name.to_string()),
                    });
                }
            }
            let names: Vec<&str> = assets.iter().map(|a| s(a, "name")).collect();
            Err(package_err(format!("No asset matching {} in {repo} {tag}. Assets: {}. Fix the regex in usr/packages.json.", s(src, "asset"), names.join(", "))))
        }
        "composer" => {
            let v = http_json("https://getcomposer.org/versions")?;
            let stable = &v["stable"][0];
            let version = s(stable, "version");
            let sums = format!("https://getcomposer.org/download/{version}/composer.phar.sha256sum");
            Ok(Resolved {
                url: format!("https://getcomposer.org{}", s(stable, "path")),
                folder: format!("composer-{version}"),
                sha256: Some(published_sha256(&sums, "composer.phar")?),
                filename: Some("composer.phar".into()),
            })
        }
        "go" => {
            let rels = http_json("https://go.dev/dl/?mode=json")?;
            for r in rels.as_array().into_iter().flatten().filter(|r| r["stable"].as_bool().unwrap_or(false)) {
                for f in r["files"].as_array().into_iter().flatten() {
                    if s(f, "os") == "windows" && s(f, "arch") == "amd64" && s(f, "kind") == "archive" {
                        return Ok(Resolved {
                            url: format!("https://go.dev/dl/{}", s(f, "filename")),
                            folder: s(r, "version").replacen("go", "go-", 1),
                            sha256: f.get("sha256").and_then(Value::as_str).map(str::to_string),
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
                    filename: Some("phpMyAdmin-6.0-snapshot.zip".into()),
                });
            }
            let v = http_json("https://www.phpmyadmin.net/home_page/version.json")?;
            let ver = s(&v, "version");
            let file = format!("phpMyAdmin-{ver}-all-languages.zip");
            let url = format!("https://files.phpmyadmin.net/phpMyAdmin/{ver}/{file}");
            Ok(Resolved {
                sha256: Some(published_sha256(&format!("{url}.sha256"), &file)?),
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
            Ok(Resolved { url, folder: format!("{tool}-{version}"), sha256: None, filename: None })
        }
        "url" => {
            let url = s(src, "url").to_string();
            let folder = match s(src, "folder") {
                "" => format!("{tool}-{}", parse_version(&url_file(&url)).unwrap_or_else(|| "latest".into())),
                f => f.to_string(),
            };
            Ok(Resolved { sha256: src.get("sha256").and_then(Value::as_str).map(str::to_string), url, folder, filename: None })
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
    let before = app.active(&item.tool);
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
    if let Some(expected) = &r.sha256 {
        let got = sha256_of(&file)?;
        if !got.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&file);
            return Err(package_err(format!("Checksum mismatch for {name} (expected {expected}, got {got}). Download discarded.")));
        }
        log("  checksum verified");
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
    pin_version(app, &item.tool, before, &r.folder)?;
    log(&format!("Installed {} {} → {}", item.tool, r.folder, target.display()));
    Ok(target)
}
