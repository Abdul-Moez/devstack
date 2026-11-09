//! Quick app: create a project in www/ with one click, plus a database and a pretty URL.
//!
//! Recipes come from usr/quickapps.json (created from the built-in list on first use; edit
//! freely). Types: blank, zip (download + extract), composer (create-project), command (any
//! shell command; {name} is replaced by the project name).

use std::fs;
use std::path::Path;

use anyhow::{bail, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::assets;
use crate::core::App;
use crate::database::MySqlTools;
use crate::packages;
use crate::util::{command, exe, load_json, random_hex, run_stream, valid_name};
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
}

pub fn recipes(app: &App) -> Vec<Recipe> {
    let user = app.paths.usr.join("quickapps.json");
    if !user.exists() {
        let _ = fs::write(&user, assets::QUICKAPPS_JSON);
    }
    load_json(&user)
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| serde_json::from_str(assets::QUICKAPPS_JSON).unwrap_or_default())
}

fn composer_cmd(app: &App) -> Result<(String, String)> {
    let php = app.active("php").ok_or_else(|| anyhow::anyhow!("PHP is not installed (Tools > Quick add)."))?;
    let comp = app.active("composer").ok_or_else(|| anyhow::anyhow!("Composer is not installed (Tools > Quick add > Composer)."))?;
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
    log(&format!("Ready: {url}"));
    Ok(url)
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
