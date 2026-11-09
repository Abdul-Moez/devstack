//! Every folder in www/ is a site: www/blog -> http://blog.test.
//!
//! Per-site overrides go in usr/sites.json, for example:
//! `{"blog": {"docroot": "web", "aliases": ["blog.local"], "ssl": false}, "old": {"disabled": true}}`

use std::fs;
use std::path::PathBuf;

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
        let host = o.get("host").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| host_for(&name, &template));
        if host.is_empty() {
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
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let ssl = o.get("ssl").and_then(Value::as_bool).unwrap_or(ssl_default) && ssl_default;
        sites.push(Site { name, host, path, docroot, aliases, ssl, cert: String::new(), key: String::new() });
    }
    sites
}
