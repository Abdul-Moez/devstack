//! Quick app: create a project in www/ with one click, plus a database and a pretty URL.
//!
//! Recipes: the built-in list (assets/quickapps.json) plus usr/quickapps.json, where a recipe
//! with the same name replaces the built-in one. Types: blank, zip (download + extract),
//! composer (create-project), command (any shell command; {name} is replaced by the project
//! name), python (virtualenv + pip packages + setup commands, served through a proxy site).

use std::fs;
use std::path::Path;

use anyhow::{bail, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::assets;
use crate::core::App;
use crate::database::MySqlTools;
use crate::packages;
use crate::util::{command, exe, load_json, path_sep, random_hex, run_stream, valid_name};
use crate::Log;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub wordpress: bool,
    /// python: packages to pip install into the project's .venv.
    #[serde(default)]
    pub pip: Vec<String>,
    /// python: commands run in the project folder with the .venv active.
    #[serde(default)]
    pub commands: Vec<String>,
    /// python: the dev server command; {port} is replaced. Written to run.bat.
    #[serde(default)]
    pub run: Option<String>,
    /// python: also write a hello-world app.py (Flask).
    #[serde(default)]
    pub flask: bool,
    /// python: let config/settings.py accept the .test host (Django).
    #[serde(default)]
    pub django: bool,
}

pub fn recipes(app: &App) -> Vec<Recipe> {
    let mut list: Vec<Recipe> = serde_json::from_str(assets::QUICKAPPS_JSON).unwrap_or_default();
    let user = app.paths.usr.join("quickapps.json");
    if !user.exists() {
        let _ = fs::write(&user, "[]\n");
    }
    let extra: Vec<Recipe> = load_json(&user).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    for r in extra {
        match list.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&r.name)) {
            Some(x) => *x = r,
            None => list.push(r),
        }
    }
    list
}

/// php.exe and the composer.phar to run it with. Composer 2.3+ needs PHP 7.2.5, so with an
/// older PHP an installed Composer 2.2 LTS is used instead of the active Composer.
pub fn composer_cmd(app: &App) -> Result<(String, String)> {
    let php = app.active("php").ok_or_else(|| anyhow::anyhow!("PHP is not installed (Tools > Quick add)."))?;
    let active = app.active("composer").ok_or_else(|| anyhow::anyhow!("Composer is not installed (Tools > Quick add > Composer)."))?;
    let comp = if php.key() < vec![7, 2, 5] && active.key() >= vec![2, 3] {
        app.versions("composer").into_iter().find(|c| c.key() < vec![2, 3]).ok_or_else(|| {
            anyhow::anyhow!("PHP {} needs Composer 2.2 LTS: Tools > Quick add > Composer 2.2 LTS.", php.number())
        })?
    } else {
        active
    };
    Ok((php.path.join(exe("php")).display().to_string(), comp.exe.display().to_string()))
}

fn composer(app: &App, args: &[&str]) -> Result<std::process::Command> {
    let (php, phar) = composer_cmd(app)?;
    let mut c = command(php, [phar].into_iter().chain(args.iter().map(|s| s.to_string())));
    c.env_clear().envs(app.tool_env()).current_dir(app.paths().www);
    Ok(c)
}

fn shell(app: &App, line: &str) -> std::process::Command {
    let mut c = if cfg!(windows) { command("cmd.exe", ["/C", line]) } else { command("sh", ["-c", line]) };
    c.env_clear().envs(app.tool_env()).current_dir(app.paths().www);
    c
}

fn extract_zip(file: &Path, dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file)?)?;
    archive.extract(dest)?;
    Ok(())
}

pub fn create(app: &App, recipe: &Recipe, name: &str, log: Log) -> Result<String> {
    if !valid_name(name) {
        bail!("Use letters, numbers, dots, dashes or underscores for the project name.");
    }
    let www = app.paths().www;
    let target = www.join(name);
    if target.exists() {
        bail!("{} already exists.", target.display());
    }
    app.generate(log)?; // make sure php.ini (extensions, CA bundle) exists for the CLI
    log(&format!("Creating {} project '{name}'…", recipe.name));
    match recipe.kind.as_str() {
        "blank" => {
            fs::create_dir_all(&target)?;
            fs::write(target.join("index.php"), format!("<?php\necho 'Hello from {name}!';\n"))?;
        }
        "zip" => {
            let url = recipe.url.as_deref().unwrap_or_default();
            let file = app.paths.tmp.join("downloads").join(format!("quickapp-{name}.zip"));
            log(&format!("Downloading {url}"));
            packages::download(url, &file, None, &mut |_, _| {})?;
            let ex = app.paths.tmp.join("extract").join(format!("quickapp-{name}"));
            let _ = fs::remove_dir_all(&ex);
            extract_zip(&file, &ex)?;
            let entries: Vec<_> = fs::read_dir(&ex)?.flatten().map(|e| e.path()).collect();
            let src = if entries.len() == 1 && entries[0].is_dir() { entries[0].clone() } else { ex.clone() };
            fs::rename(&src, &target)?;
            let _ = fs::remove_dir_all(&ex);
            let _ = fs::remove_file(&file);
        }
        "composer" => {
            let pkg = recipe.package.as_deref().unwrap_or_default();
            run_stream(composer(app, &["create-project", pkg, name, "--no-interaction", "--prefer-dist"])?, log)?;
        }
        "command" => {
            let line = recipe.command.as_deref().unwrap_or_default().replace("{name}", name);
            if line.starts_with("laravel ") && !has_laravel(app) {
                log("Installing the Laravel installer (one time)…");
                run_stream(composer(app, &["global", "require", "laravel/installer", "--no-interaction"])?, log)?;
            }
            run_stream(shell(app, &line), log)?;
        }
        "python" => {
            if let Err(e) = python_project(app, recipe, name, &target, log) {
                let _ = fs::remove_dir_all(&target); // created by us a moment ago
                return Err(e);
            }
        }
        other => bail!("Unknown recipe type '{other}'."),
    }
    if app.settings.bool("quick_app.create_database") && app.svc("mysql").running(app) {
        match MySqlTools::new(app).create_database(name) {
            Ok(()) => log(&format!("Created database '{name}'")),
            Err(e) => log(&format!("Could not create database: {e}")),
        }
    }
    if recipe.wordpress {
        wp_config(app, &target, name)?;
    }
    app.reload(log)?;
    let url = app.sites().into_iter().find(|s| s.name == name).map(|s| s.url()).unwrap_or_else(|| format!("http://localhost/{name}"));
    if recipe.kind == "python" {
        start_dev_server(app, &target, log);
    }
    log(&format!("Ready: {url}"));
    Ok(url)
}

// ------------------------------------------------------------------ python projects
/// Command with devstack's tools on PATH, run in `dir`; with `venv`, that virtualenv first.
fn in_project(mut c: std::process::Command, app: &App, dir: &Path, venv: Option<&Path>) -> std::process::Command {
    let mut env = app.tool_env();
    // A PYTHONHOME/PYTHONPATH left over from another install would break the venv.
    env.retain(|k, _| !k.eq_ignore_ascii_case("PYTHONHOME") && !k.eq_ignore_ascii_case("PYTHONPATH"));
    if let Some(venv) = venv {
        let path = env.get("PATH").cloned().unwrap_or_default();
        env.insert("PATH".into(), format!("{}{}{path}", venv_bin(venv).display(), path_sep()));
        env.insert("VIRTUAL_ENV".into(), venv.display().to_string());
    }
    c.env_clear().envs(env).current_dir(dir);
    c
}

fn venv_bin(venv: &Path) -> std::path::PathBuf {
    venv.join(if cfg!(windows) { "Scripts" } else { "bin" })
}

fn python_project(app: &App, recipe: &Recipe, name: &str, target: &Path, log: Log) -> Result<()> {
    let py = app.active("python").ok_or_else(|| anyhow::anyhow!("Python is not installed (Tools > Quick add > Python)."))?;
    fs::create_dir_all(target)?;
    let venv = target.join(".venv");
    log(&format!("Creating a virtual environment with Python {}…", py.number()));
    run_stream(in_project(command(&py.exe, ["-m", "venv", ".venv"]), app, target, None), log)?;
    let vpy = venv_bin(&venv).join(exe("python"));
    if !recipe.pip.is_empty() {
        log(&format!("Installing {}…", recipe.pip.join(", ")));
        let mut args: Vec<String> = vec!["-m".into(), "pip".into(), "install".into(), "--disable-pip-version-check".into()];
        args.extend(recipe.pip.iter().cloned());
        run_stream(in_project(command(&vpy, args), app, target, Some(&venv)), log)?;
    }
    for line in &recipe.commands {
        let line = line.replace("{name}", name);
        let c = if cfg!(windows) { command("cmd.exe", ["/C", line.as_str()]) } else { command("sh", ["-c", line.as_str()]) };
        run_stream(in_project(c, app, target, Some(&venv)), log)?;
    }
    if recipe.flask {
        fs::write(target.join("app.py"), format!(
            "from flask import Flask\n\napp = Flask(__name__)\n\n\n@app.route(\"/\")\ndef index():\n    return \"Hello from {name}!\"\n"
        ))?;
    }
    let port = next_app_port(app)?;
    set_site_proxy(app, name, port)?;
    let host = app.sites().into_iter().find(|s| s.name == name).map(|s| s.host).unwrap_or_default();
    if recipe.django {
        django_hosts(&target.join("config").join("settings.py"), &host)?;
    }
    if let Some(run) = &recipe.run {
        fs::write(target.join("run.bat"), format!(
            "@echo off\r\nrem Dev server for https://{host} (devstack forwards it to port {port})\r\ncd /d \"%~dp0\"\r\ncall .venv\\Scripts\\activate.bat\r\n{}\r\n",
            run.replace("{port}", &port.to_string())
        ))?;
        log(&format!("Dev server: run.bat in the project folder (port {port}). https://{host} forwards to it."));
    }
    Ok(())
}

/// A free port for an app server: not a devstack port, not used by another proxied site, not
/// listening right now.
fn next_app_port(app: &App) -> Result<u16> {
    let mut used: std::collections::HashSet<u16> = crate::procs::listening().into_keys().collect();
    if let Some(ports) = app.settings.get("ports").as_object() {
        used.extend(ports.values().filter_map(Value::as_u64).filter_map(|p| u16::try_from(p).ok()));
    }
    if let Some(Value::Object(sites)) = load_json(&app.paths().usr.join("sites.json")) {
        used.extend(sites.values().filter_map(|o| crate::sites::proxy_port(o.get("proxy"))));
    }
    (8001..9000).find(|p| !used.contains(p)).ok_or_else(|| anyhow::anyhow!("No free port between 8001 and 8999."))
}

/// Add {"<name>": {"proxy": port}} to usr/sites.json, keeping everything else in it.
fn set_site_proxy(app: &App, name: &str, port: u16) -> Result<()> {
    let path = app.paths().usr.join("sites.json");
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut all: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| anyhow::anyhow!(
            "usr/sites.json isn't valid JSON ({e}); add \"{name}\": {{\"proxy\": {port}}} to it yourself."
        ))?
    };
    let Some(obj) = all.as_object_mut() else { bail!("usr/sites.json should contain a JSON object.") };
    let entry = obj.entry(name.to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry["proxy"] = json!(port);
    fs::write(&path, serde_json::to_string_pretty(&all)? + "\n")?;
    Ok(())
}

/// Django only answers for hosts in ALLOWED_HOSTS and checks the origin of HTTPS form posts.
fn django_hosts(settings: &Path, host: &str) -> Result<()> {
    let Ok(text) = fs::read_to_string(settings) else { return Ok(()) };
    let allowed = format!("ALLOWED_HOSTS = ['{host}', '.{host}', 'localhost', '127.0.0.1']\n\nCSRF_TRUSTED_ORIGINS = ['https://{host}', 'http://{host}']");
    fs::write(settings, text.replacen("ALLOWED_HOSTS = []", &allowed, 1))?;
    Ok(())
}

/// Open the project's run.bat in its own console window and wait (briefly) for the port.
fn start_dev_server(app: &App, target: &Path, log: Log) {
    let script = target.join("run.bat");
    if !script.exists() {
        return;
    }
    if let Err(e) = crate::winutil::open_console(&script, target, &app.tool_env()) {
        log(&format!("Couldn't start the dev server ({e:#}); run run.bat yourself."));
        return;
    }
    let port = app.sites().into_iter().find(|s| s.path == target).and_then(|s| s.proxy);
    if let Some(port) = port {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(20) && !crate::procs::port_open(port) {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
}

fn has_laravel(app: &App) -> bool {
    let bin = app.paths.usr.join("composer").join("vendor").join("bin");
    bin.join("laravel").exists() || bin.join("laravel.bat").exists()
}

fn php_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn wp_config(app: &App, target: &Path, db: &str) -> Result<()> {
    let sample = target.join("wp-config-sample.php");
    let Ok(mut text) = fs::read_to_string(&sample) else { return Ok(()) };
    text = text.replace("database_name_here", &php_str(db)).replace("username_here", "root");
    text = text.replace("password_here", &php_str(&app.secrets.str("mysql_root_password")));
    let re = Regex::new(r"define\(\s*'DB_HOST',\s*'localhost'\s*\);").unwrap();
    text = re.replace(&text, "define( 'DB_HOST', '127.0.0.1' );").into_owned();
    while text.contains("put your unique phrase here") {
        text = text.replacen("put your unique phrase here", &random_hex(64), 1);
    }
    fs::write(target.join("wp-config.php"), text)?;
    Ok(())
}
