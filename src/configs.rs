//! Renders every config file from the embedded templates on each start and reload.
//!
//! Generated files are disposable. To customize:
//!   * add lines to usr/php.ini, usr/my.ini, usr/redis.conf (appended),
//!   * drop *.conf files in usr/apache/ or usr/nginx/ (included last),
//!   * or put a template in usr/templates/ (same relative path) to replace the built-in one.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde_json::{json, Value};

use crate::assets;
use crate::core::App;
use crate::services::{mysql_data_dir, php_pool_ports};
use crate::sites::Site;
use crate::util::{fwd, read_text, write_if_changed};
use crate::versions::Version;

pub const APACHE_MODULES: &[&str] = &[
    // MPM/unixd only exist on Linux builds (used by tests); Windows has them built in.
    "mpm_prefork", "unixd",
    "access_compat", "actions", "alias", "allowmethods", "auth_basic", "authn_core", "authn_file",
    "authz_core", "authz_groupfile", "authz_host", "authz_user", "autoindex", "deflate", "dir", "env",
    "expires", "filter", "headers", "include", "lbmethod_byrequests", "log_config", "mime",
    "negotiation", "proxy", "proxy_balancer", "proxy_fcgi", "proxy_http", "proxy_wstunnel", "rewrite",
    "setenvif", "slotmem_shm", "socache_shmcb", "ssl", "vhost_alias",
];

pub const DEFAULT_EXTENSIONS: &[&str] = &[
    "bz2", "curl", "exif", "fileinfo", "gd", "gettext", "intl", "mbstring", "mysqli", "openssl",
    "pdo_mysql", "pdo_pgsql", "pdo_sqlite", "pgsql", "soap", "sockets", "sodium", "sqlite3", "xsl", "zip",
];

const ZEND_EXTENSIONS: &[&str] = &["opcache", "xdebug"];

/// (path, module name) of Apache's PHP module if the PHP build has one (thread-safe builds).
pub fn php_apache_module(app: &App, v: Option<&Version>) -> Option<(PathBuf, &'static str)> {
    let active;
    let v = match v {
        Some(v) => v,
        None => {
            active = app.active("php")?;
            &active
        }
    };
    [("php8apache2_4.dll", "php_module"), ("php7apache2_4.dll", "php7_module")]
        .iter()
        .map(|(dll, name)| (v.path.join(dll), *name))
        .find(|(p, _)| p.exists())
}

/// Where a PHP version's php.ini goes: next to php.exe for devstack's own versions (so `php` on
/// PATH finds it), under etc/php/<version>/ for Laragon's, whose folders devstack never writes to.
pub fn php_ini_dir(app: &App, v: &Version) -> PathBuf {
    if v.external {
        app.paths.etc.join("php").join(&v.name)
    } else {
        v.path.clone()
    }
}

pub fn php_available_extensions(v: &Version) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(v.path.join("ext"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    name.strip_prefix("php_").and_then(|n| n.strip_suffix(".dll")).map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

pub fn php_enabled_extensions(app: &App, v: &Version) -> Vec<String> {
    match app.settings.get(&format!("php.extensions.{}", v.series())) {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => DEFAULT_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
    }
}

fn php_extension_lines(app: &App, v: &Version) -> (Vec<String>, Vec<String>) {
    let available: HashSet<String> = php_available_extensions(v).into_iter().collect();
    let mut exts: Vec<String> = php_enabled_extensions(app, v)
        .into_iter()
        .filter(|e| available.contains(e) && !ZEND_EXTENSIONS.contains(&e.as_str()))
        .collect();
    // Older PHP builds load exif only after mbstring.
    exts.sort_by_key(|e| (e != "mbstring", e.clone()));
    let mut zend = vec![];
    if available.contains("opcache") {
        zend.push("opcache".to_string()); // PHP <= 8.4; 8.5 has OPcache built in
    }
    if app.settings.bool("php.xdebug") && available.contains("xdebug") {
        zend.push("xdebug".to_string());
    }
    (exts, zend)
}

fn environment(app: &App) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_keep_trailing_newline(true);
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    // {{ x | php }}: safe inside a single-quoted PHP string (a path with an apostrophe, …).
    env.add_filter("php", |v: String| v.replace('\\', "\\\\").replace('\'', "\\'"));
    let usr = app.paths.usr.join("templates");
    env.set_loader(move |name| {
        let p = usr.join(name);
        if p.is_file() {
            return fs::read_to_string(&p)
                .map(Some)
                .map_err(|e| minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, format!("{}: {e}", p.display())));
        }
        Ok(assets::template(name).map(str::to_string))
    });
    env
}

/// Ports for the templates, always as numbers: a hand-edited settings.json can't put text
/// (and with it extra config lines) where a port goes.
fn ports(app: &App) -> Value {
    let defaults = crate::settings::defaults();
    let mut out = serde_json::Map::new();
    if let Some(o) = app.settings.get("ports").as_object() {
        for (k, v) in o {
            if defaults["ports"].get(k).is_some() {
                out.insert(k.clone(), json!(app.settings.port(k)));
            } else if let Some(n) = v.as_u64().and_then(|n| u16::try_from(n).ok()) {
                out.insert(k.clone(), json!(n));
            }
        }
    }
    Value::Object(out)
}

/// Settings that end up on one config line, with line breaks and control characters removed.
fn one_line(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(s.chars().filter(|c| !c.is_control()).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, one_line(v))).collect()),
        other => other,
    }
}

/// This computer's name, if it is a plain host name: the catch-all site answers to it too, so
/// http://my-pc/ works from the network. A website can't claim that name for itself.
fn computer_name() -> String {
    let name = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default().to_lowercase();
    let plain = !name.is_empty() && name.len() <= 63 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if plain { name } else { String::new() }
}

fn pma_secret(app: &App) -> Result<String> {
    let sec = app.secrets.str("phpmyadmin_secret");
    if !sec.is_empty() {
        return Ok(sec);
    }
    let sec = crate::util::random_hex(32); // phpMyAdmin wants exactly 32 characters
    app.secrets.set("phpmyadmin_secret", json!(sec))?;
    Ok(sec)
}

pub fn context(app: &App, sites: &[Site]) -> Result<Value> {
    let p = &app.paths();
    let s = &app.settings;
    let dir = |tool: &str| app.active(tool).map(|v| fwd(&v.path)).unwrap_or_default();
    let php = app.active("php");
    let (exts, zend) = php.as_ref().map(|v| php_extension_lines(app, v)).unwrap_or_default();
    let module = php_apache_module(app, php.as_ref());
    let apache_modules: Vec<&str> = match app.active("apache") {
        Some(v) => APACHE_MODULES.iter().copied().filter(|m| v.path.join("modules").join(format!("mod_{m}.so")).exists()).collect(),
        None => vec![],
    };
    let cacert = p.ssl.join("cacert.pem");
    Ok(json!({
        "root": fwd(&p.root), "www": fwd(&p.www), "etc": fwd(&p.etc), "logs": fwd(&p.logs),
        "tmp": fwd(&p.tmp), "data": fwd(&p.data), "usr": fwd(&p.usr), "ssl_dir": fwd(&p.ssl),
        "dashboard_dir": fwd(p.etc.join("dashboard")),
        "ports": ports(app),
        "computer_name": computer_name(),
        // Web servers listen on 127.0.0.1 only, unless the user lets other devices in.
        // Listening everywhere would let anyone on the same Wi-Fi browse www/ (incl. .env files).
        "bind": if s.bool("lan_access") { "" } else { "127.0.0.1:" },
        "ssl": s.bool("ssl"),
        "localhost_cert": fwd(p.ssl.join("sites").join("localhost.crt")),
        "localhost_key": fwd(p.ssl.join("sites").join("localhost.key")),
        "sites": sites.iter().map(|x| json!({
            "name": x.name, "host": x.host, "aliases": x.aliases, "docroot": fwd(&x.docroot),
            "ssl": x.ssl, "cert": x.cert, "key": x.key,
            "proxy": x.proxy.map(|p| format!("http://127.0.0.1:{p}")).unwrap_or_default(),
        })).collect::<Vec<_>>(),
        "php": one_line(s.get("php")),
        // PHP 7.0/7.1 only understand "extension=php_curl.dll", not "extension=curl".
        "php_ext_dll": php.as_ref().is_some_and(|v| v.key() < vec![7, 2]),
        // serialize_precision = -1 exists since PHP 7.1.
        "php_modern_precision": php.as_ref().is_none_or(|v| v.key() >= vec![7, 1]),
        "php_ports": php_pool_ports(app),
        "hostname_template": s.str("hostname_template"),
        "cacert": if cacert.exists() { fwd(&cacert) } else { String::new() },
        "mailpit_smtp": s.port("mailpit_smtp"),
        "mailpit_ui": s.port("mailpit_ui"),
        "mysql_port": s.port("mysql"),
        "mysql_password": app.secrets.str("mysql_root_password"),
        "phpmyadmin_secret": pma_secret(app)?,
        "usr_php_ini": read_text(&p.usr.join("php.ini")),
        "usr_my_ini": read_text(&p.usr.join("my.ini")),
        "usr_redis_conf": read_text(&p.usr.join("redis.conf")),
        "apache_dir": dir("apache"), "nginx_dir": dir("nginx"), "php_dir": dir("php"),
        "php_ini_dir": php.as_ref().map(|v| fwd(php_ini_dir(app, v))).unwrap_or_default(),
        "mysql_dir": dir("mysql"), "phpmyadmin_dir": dir("phpmyadmin"),
        "apache_modules": apache_modules,
        "php_extensions": exts,
        "php_zend_extensions": zend,
        "php_apache_module": module.as_ref().map(|(p, _)| fwd(p)).unwrap_or_default(),
        "php_apache_module_name": module.as_ref().map(|(_, n)| *n).unwrap_or_default(),
        "mysql_data": app.active("mysql").map(|v| fwd(mysql_data_dir(app, &v))).unwrap_or_default(),
    }))
}

/// (service id, template, target file) for everything that should be generated.
pub fn outputs(app: &App) -> Vec<(&'static str, &'static str, PathBuf)> {
    let p = &app.paths();
    let mut out = vec![("dashboard", "dashboard/index.php.j2", p.etc.join("dashboard").join("index.php"))];
    if app.active("apache").is_some() {
        out.push(("apache", "apache/httpd.conf.j2", p.etc.join("apache").join("httpd.conf")));
        out.push(("apache", "apache/sites.conf.j2", p.etc.join("apache").join("sites.conf")));
    }
    if app.active("nginx").is_some() {
        out.push(("nginx", "nginx/nginx.conf.j2", p.etc.join("nginx").join("nginx.conf")));
        out.push(("nginx", "nginx/sites.conf.j2", p.etc.join("nginx").join("sites.conf")));
    }
    if let Some(php) = app.active("php") {
        out.push(("php", "php/php.ini.j2", php_ini_dir(app, &php).join("php.ini")));
    }
    if app.active("mysql").is_some() {
        out.push(("mysql", "mysql/my.ini.j2", p.etc.join("mysql").join("my.ini")));
        out.push(("mysql-client", "mysql/client.cnf.j2", p.etc.join("mysql").join("client.cnf")));
    }
    if app.active("redis").is_some() {
        out.push(("redis", "redis/redis.conf.j2", p.data.join("redis").join("redis.conf")));
    }
    if let Some(pma) = app.active("phpmyadmin") {
        out.push(("phpmyadmin", "phpmyadmin/config.inc.php.j2", pma.path.join("config.inc.php")));
    }
    out
}

/// Render everything. Returns the ids of services whose config changed.
pub fn generate(app: &App, sites: &[Site]) -> Result<HashSet<String>> {
    let ctx = context(app, sites)?;
    let env = environment(app);
    let mut changed = HashSet::new();
    for (sid, template, target) in outputs(app) {
        let text = env
            .get_template(template)
            .and_then(|t| t.render(&ctx))
            .with_context(|| format!("rendering {template}"))?;
        if write_if_changed(&target, &text)? {
            changed.insert(sid.to_string());
        }
    }
    if changed.contains("php") && php_apache_module(app, None).is_some() {
        changed.insert("apache".into()); // Apache embeds PHP, so php.ini changes need a restart
    }
    Ok(changed)
}
