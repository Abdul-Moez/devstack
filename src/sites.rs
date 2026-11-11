//! Every folder in www/ is a site: www/blog -> http://blog.test.
//!
//! Per-site overrides go in usr/sites.json, for example:
//! `{"blog": {"docroot": "web", "aliases": ["blog.local"], "ssl": false}, "old": {"disabled": true}}`
//! `{"api": {"proxy": 8001}}` sends https://api.test to an app server on 127.0.0.1:8001
//! (Django, Flask, Node, ...) instead of serving files.
//! `{"shop": {"versions": {"php": "php-7.4.33-ts", "mysql": "mysql-5.7.44"}}}` makes shop a
//! project with its own versions: choosing it switches to them (see `App::set_project`).
//!
//! The web folder (docroot) is `public/` if the project has one, else the project folder;
//! `"docroot": "another_folder"` picks another folder inside the project ("." = the project
//! folder itself).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::{json, Value};

use crate::core::{App, PROJECT_TOOLS};
use crate::util::{load_json, safe_file_name, write_if_changed};

#[derive(Clone, Debug)]
pub struct Site {
    pub name: String,
    pub host: String,
    pub path: PathBuf,
    pub docroot: PathBuf,
    /// The web folder chosen in usr/sites.json, relative to the project ("a/b", or "." for
    /// the project folder). None = automatic.
    pub docroot_rel: Option<String>,
    pub aliases: Vec<String>,
    pub ssl: bool,
    pub cert: String,
    pub key: String,
    /// Local port of an app server the web server forwards requests to.
    pub proxy: Option<u16>,
    /// Versions this project uses (tool -> version folder), e.g. php -> php-7.4.33-ts.
    pub versions: BTreeMap<String, String>,
}

/// A proxy target from usr/sites.json: only a port number (8001 or "8001") is accepted, so
/// nothing but 127.0.0.1:<port> can end up in the web server config.
pub fn proxy_port(v: Option<&Value>) -> Option<u16> {
    let n = match v? {
        Value::Number(n) => n.as_u64()?,
        Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    u16::try_from(n).ok().filter(|p| *p != 0)
}

impl Site {
    pub fn url(&self) -> String {
        format!("{}://{}", if self.ssl { "https" } else { "http" }, self.host)
    }

    /// What the project is built with, judged from its files.
    pub fn stack(&self) -> Vec<&'static str> {
        let has = |f: &str| self.path.join(f).exists();
        let mut out = vec![];
        if has("composer.json") || has("index.php") || has("artisan") || has("wp-config.php") || has("public/index.php") {
            out.push("PHP");
        }
        if has("package.json") {
            out.push("Node");
        }
        if has("manage.py") || has("requirements.txt") || has("pyproject.toml") || has(".venv") || has("app.py") {
            out.push("Python");
        }
        out
    }
}

/// A web folder from usr/sites.json, normalized to "a/b" (or "." for the project folder).
/// It has to stay inside the project: absolute paths, drive letters and ".." are refused, and
/// so are characters that could end the quoted path in an Apache or Nginx config.
pub fn clean_docroot(rel: &str) -> Option<String> {
    let rel = rel.trim().replace('\\', "/");
    if rel == "." || rel == "./" {
        return Some(".".into());
    }
    let rel = rel.trim_matches('/');
    let ok = !rel.is_empty()
        && rel.len() <= 200
        && rel.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(' ')
                && !part.chars().any(|c| c.is_control() || matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?' | '$' | ';' | '{' | '}'))
        });
    ok.then(|| rel.to_string())
}

fn join_rel(base: &Path, rel: &str) -> PathBuf {
    if rel == "." {
        return base.to_path_buf();
    }
    rel.split('/').fold(base.to_path_buf(), |p, part| p.join(part))
}

/// True if the folder has a file a web server would serve by default.
pub fn has_index(dir: &Path) -> bool {
    ["index.php", "index.html", "index.htm"].iter().any(|f| dir.join(f).is_file())
}

/// Folders inside a project (up to two levels down) that hold an index file, as paths relative
/// to the project, nearest first. Dependency and hidden folders are skipped.
pub fn docroot_candidates(project: &Path, limit: usize) -> Vec<String> {
    const SKIP: &[&str] = &["vendor", "node_modules", "storage", "cache", "tests", "test", "docs"];
    let mut out = vec![];
    let mut level: Vec<(PathBuf, String)> = vec![(project.to_path_buf(), String::new())];
    let mut visited = 0;
    for _ in 0..2 {
        let mut next = vec![];
        for (dir, rel) in &level {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            let mut names: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.') && !SKIP.contains(&n.to_lowercase().as_str()))
                .collect();
            names.sort_by_key(|n| n.to_lowercase());
            for name in names {
                visited += 1;
                if visited > 300 {
                    return out;
                }
                let path = dir.join(&name);
                let rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                if has_index(&path) && clean_docroot(&rel).is_some() {
                    out.push(rel.clone());
                    if out.len() >= limit {
                        return out;
                    }
                }
                next.push((path, rel));
            }
        }
        level = next;
    }
    out
}

/// Set the folder inside a project that the web server serves (`None` = automatic).
pub fn set_docroot(app: &App, name: &str, rel: Option<&str>) -> Result<()> {
    let rel = match rel.map(str::trim).filter(|r| !r.is_empty()) {
        None => None,
        Some(r) => {
            let clean = clean_docroot(r).ok_or_else(|| anyhow!("The web folder has to be a folder inside the project (no drive letters or \"..\")."))?;
            let site = app.sites().into_iter().find(|s| s.name == name).ok_or_else(|| anyhow!("No project called {name} in www."))?;
            if !join_rel(&site.path, &clean).is_dir() {
                return Err(anyhow!("{name} has no folder called {clean}."));
            }
            Some(clean)
        }
    };
    update_override(app, name, |e| match &rel {
        Some(r) => {
            e.insert("docroot".into(), json!(r));
        }
        None => {
            e.remove("docroot");
        }
    })
}

/// Versions from a usr/sites.json entry: only known tools and plain folder names.
fn parse_versions(v: Option<&Value>) -> BTreeMap<String, String> {
    v.and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter(|(tool, _)| PROJECT_TOOLS.contains(&tool.as_str()))
                .filter_map(|(tool, name)| name.as_str().filter(|n| safe_file_name(n)).map(|n| (tool.clone(), n.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Change one site's entry in usr/sites.json, keeping everything else in the file. An entry
/// left empty is removed.
pub fn update_override(app: &App, name: &str, change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<()> {
    let path = app.paths().usr.join("sites.json");
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut all: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| anyhow!("usr/sites.json isn't valid JSON ({e}); fix it first."))?
    };
    let obj = all.as_object_mut().ok_or_else(|| anyhow!("usr/sites.json should contain a JSON object."))?;
    let entry = obj.entry(name.to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    change(entry.as_object_mut().expect("just made it an object"));
    if entry.as_object().is_some_and(serde_json::Map::is_empty) {
        obj.remove(name);
    }
    write_if_changed(&path, &(serde_json::to_string_pretty(&all)? + "\n"))?;
    Ok(())
}

/// Save a project's versions (an empty map makes it use the defaults again).
pub fn set_versions(app: &App, name: &str, versions: &BTreeMap<String, String>) -> Result<()> {
    for (tool, v) in versions {
        if !PROJECT_TOOLS.contains(&tool.as_str()) || !safe_file_name(v) {
            return Err(anyhow!("{tool} = {v} isn't a version devstack can use."));
        }
    }
    update_override(app, name, |e| {
        if versions.is_empty() {
            e.remove("versions");
        } else {
            e.insert("versions".into(), json!(versions));
        }
    })
}

pub fn host_for(name: &str, template: &str) -> String {
    let re = Regex::new(r"[^a-z0-9-]+").unwrap();
    let label = re.replace_all(&name.to_lowercase(), "-").trim_matches('-').to_string();
    if label.is_empty() {
        String::new()
    } else {
        template.replace("{name}", &label)
    }
}

/// A plain DNS host name (lowercase labels, no wildcard, no spaces). Host names end up in the
/// hosts file (written with admin rights) and in Apache/Nginx configs, so anything else from
/// usr/sites.json or the host name template is ignored.
pub fn valid_host(h: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    h.len() <= 253
        && RE
            .get_or_init(|| Regex::new(r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$").unwrap())
            .is_match(h)
}

pub fn discover(app: &App) -> Vec<Site> {
    let www = app.paths().www;
    let Ok(rd) = fs::read_dir(&www) else { return vec![] };
    let template = app.settings.str("hostname_template");
    let template = if template.is_empty() { "{name}.test".to_string() } else { template };
    let ssl_default = app.settings.bool("ssl");
    let overrides = load_json(&app.paths().usr.join("sites.json")).unwrap_or(Value::Null);
    let mut dirs: Vec<_> = rd.flatten().filter(|e| e.path().is_dir()).collect();
    dirs.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    let mut sites = vec![];
    for d in dirs {
        let name = d.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let o = overrides.get(&name).cloned().unwrap_or(Value::Null);
        if o.get("disabled").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let host = o.get("host").and_then(Value::as_str).map(str::to_lowercase).unwrap_or_else(|| host_for(&name, &template));
        if !valid_host(&host) {
            continue;
        }
        let path = d.path();
        // An override that isn't a folder inside the project (or doesn't exist) is ignored.
        let docroot_rel = o.get("docroot").and_then(Value::as_str).and_then(clean_docroot).filter(|rel| join_rel(&path, rel).is_dir());
        let docroot = match &docroot_rel {
            Some(rel) => join_rel(&path, rel),
            None if path.join("public").is_dir() => path.join("public"),
            None => path.clone(),
        };
        let aliases = o
            .get("aliases")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_lowercase)).filter(|x| valid_host(x)).collect())
            .unwrap_or_default();
        let ssl = o.get("ssl").and_then(Value::as_bool).unwrap_or(ssl_default) && ssl_default;
        let proxy = proxy_port(o.get("proxy"));
        let versions = parse_versions(o.get("versions"));
        sites.push(Site { name, host, path, docroot, docroot_rel, aliases, ssl, cert: String::new(), key: String::new(), proxy, versions });
    }
    sites
}
