//! Every folder in www/ is a site: www/blog -> http://blog.test.
//!
//! Per-site overrides go in usr/sites.json, for example:
//! `{"blog": {"docroot": "web", "aliases": ["blog.local"], "ssl": false}, "old": {"disabled": true}}`
//! `{"api": {"proxy": 8001}}` sends https://api.test to an app server on 127.0.0.1:8001
//! (Django, Flask, Node, ...) instead of serving files.

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::core::App;
use crate::util::load_json;

#[derive(Clone, Debug)]
pub struct Site {
    pub name: String,
    pub host: String,
    pub path: PathBuf,
    pub docroot: PathBuf,
    pub aliases: Vec<String>,
    pub ssl: bool,
    pub cert: String,
    pub key: String,
    /// Local port of an app server the web server forwards requests to.
    pub proxy: Option<u16>,
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
        let docroot = match o.get("docroot").and_then(Value::as_str) {
            Some(rel) => path.join(rel),
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
        sites.push(Site { name, host, path, docroot, aliases, ssl, cert: String::new(), key: String::new(), proxy });
    }
    sites
}
