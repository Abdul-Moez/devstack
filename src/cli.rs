//! Command line: `ds <command>`. Run `ds --help` for the list.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use crate::core::App;
use crate::error::needs_force;
use crate::{database, hosts, packages, quickapp, share, ssl, updates, versions};

#[derive(Parser)]
#[command(name = "ds", version, about = "devstack command line")]
struct Cli {
    /// Use another devstack folder (for development).
    #[arg(long, hide = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start all enabled services, or the ones named.
    Start { ids: Vec<String> },
    /// Stop everything, or the services named.
    Stop {
        ids: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    /// Restart one service.
    Restart { id: String },
    /// Regenerate configs, certificates and hosts; restart what changed.
    Reload,
    /// Show services.
    Status,
    /// List sites from www/.
    Sites,
    /// List installed versions.
    Versions { tool: Option<String> },
    /// Switch version: ds use php php-8.4.14-ts
    Use { tool: String, name: String },
    /// Choose the web server: apache | nginx | none
    Web { which: String },
    /// Enable a service (starts with Start All).
    Enable { id: String },
    /// Disable a service.
    Disable { id: String },
    /// List Quick add packages.
    Packages,
    /// Quick add: download and install packages.
    Add { ids: Vec<String> },
    /// List Quick app recipes.
    Recipes,
    /// Quick app: ds new Laravel myapp
    New { recipe: String, name: String },
    /// PHP extensions: ds ext | ds ext on gd | ds ext off xdebug
    Ext { action: Option<String>, name: Option<String> },
    /// MySQL tools.
    #[command(subcommand)]
    Db(DbCmd),
    /// Share a site through ngrok.
    Share { site: String },
    /// Stop sharing.
    Unshare,
    /// Add/remove devstack tools on your user PATH: add | remove
    Path { action: String },
    /// Local HTTPS: on | off | trust | untrust | status
    Ssl { action: String },
    /// Let other devices on your network open your sites: on | off (default off)
    Lan { action: String },
    /// Check for PHP updates and refresh cacert.pem now.
    Update,
    /// Check the setup.
    Doctor,
    /// Open a terminal with devstack tools on PATH.
    Terminal,
    /// Run devstack when Windows starts: on | off
    Autostart { action: String },
    /// First-time setup (used by the installer): download packages and enable their services.
    Setup {
        /// Quick add ids, comma separated (e.g. php-8.4,apache,mysql-8.4,mailpit)
        #[arg(long, value_delimiter = ',')]
        packages: Vec<String>,
        /// Wait for Enter before closing if something failed.
        #[arg(long)]
        pause_on_error: bool,
    },
    #[command(name = "_elevated", hide = true)]
    Elevated { task: String, arg: String },
    /// Show what Quick add entries resolve to (URL, folder, checksum) without downloading.
    /// No ids = every entry.
    #[command(hide = true)]
    Resolve { ids: Vec<String> },
}

#[derive(Subcommand)]
enum DbCmd {
    Create { name: String },
    List,
    Password { new: String },
    Backup,
    Recover,
}

fn log(msg: &str) {
    println!("{msg}");
    let _ = std::io::stdout().flush();
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Parse args and run. Returns the process exit code.
pub fn run(args: Vec<String>) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    let app = match App::new(cli.root.clone()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {e:#}");
            return 1;
        }
    };
    match dispatch(&app, cli.cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e:#}");
            1
        }
    }
}

fn dispatch(app: &App, cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Elevated { task, arg } => {
            return match task.as_str() {
                "hosts" => hosts::apply_staged(app, &PathBuf::from(arg)),
                _ => Ok(2),
            };
        }
        Cmd::Start { ids } => {
            if ids.is_empty() {
                return Ok(if app.start_all(&log)?.is_empty() { 0 } else { 1 });
            }
            for id in ids {
                app.start(&id, &log)?;
            }
        }
        Cmd::Stop { ids, force } => {
            if ids.is_empty() {
                let pending = app.stop_all(&log, force)?;
                if !pending.is_empty() && confirm("Force stop?") {
                    app.stop_all(&log, true)?;
                }
            } else {
                for id in ids {
                    if let Err(e) = app.stop(&id, &log, force) {
                        if needs_force(&e).is_some() {
                            log(&e.to_string());
                            if confirm("Force stop?") {
                                app.stop(&id, &log, true)?;
                            }
                        } else {
                            return Err(e);
                        }
                    }
                }
            }
        }
        Cmd::Restart { id } => app.restart(&id, &log)?,
        Cmd::Reload => app.reload(&log)?,
        Cmd::Status => {
            for s in app.services() {
                let v = s.version(app);
                let st = s.status(app);
                let ports = if s.installed(app) && st != "in Apache" { s.ports(app) } else { vec![] };
                let port = match ports.len() {
                    0 => String::new(),
                    1 => ports[0].to_string(),
                    2 => format!("{},{}", ports[0], ports[1]),
                    n => format!("{}-{}", ports[0], ports[n - 1]),
                };
                log(&format!("  [{}] {:<11} {:<14} {:<34} {port}", if s.enabled(app) { "on " } else { "off" }, s.name(), st,
                             v.map(|v| v.name).unwrap_or_else(|| "-".into())));
            }
            if let Some(r) = share::running(app) {
                log(&format!("  sharing {} at {}", r.site.unwrap_or_default(), r.url.unwrap_or_default()));
            }
        }
        Cmd::Sites => {
            for s in app.sites() {
                log(&format!("  {:<40} {}", s.url(), s.docroot.display()));
            }
        }
        Cmd::Versions { tool } => {
            let tools: Vec<String> = match tool {
                Some(t) => vec![t],
                None => versions::TOOLS.iter().map(|(t, _, _)| t.to_string()).collect(),
            };
            let single = tools.len() == 1;
            for t in tools {
                let vs = app.versions(&t);
                if vs.is_empty() && !single {
                    continue;
                }
                let active = app.active(&t);
                log(&format!("{t}:"));
                for v in &vs {
                    log(&format!("  {} {}", if active.as_ref() == Some(v) { "*" } else { " " }, v.name));
                }
                if vs.is_empty() {
                    log("    (none installed)");
                }
            }
        }
        Cmd::Use { tool, name } => app.switch_version(&tool, &name, &log)?,
        Cmd::Web { which } => {
            let w = match which.as_str() {
                "apache" | "nginx" => which.as_str(),
                "none" => "",
                _ => return Err(anyhow!("Choose apache, nginx or none.")),
            };
            app.set_web_server(w, &log)?;
        }
        Cmd::Enable { id } => {
            app.find_svc(&id)?.set_enabled(app, true)?;
            log(&format!("{id} enabled"));
        }
        Cmd::Disable { id } => {
            app.find_svc(&id)?.set_enabled(app, false)?;
            log(&format!("{id} disabled"));
        }
        Cmd::Packages => {
            for p in packages::catalogue(app) {
                log(&format!("  {:<20} {}", p.id, p.label));
            }
        }
        Cmd::Add { ids } => {
            for id in ids {
                packages::install(app, &packages::find(app, &id)?, &log)?;
            }
            app.generate(&log)?;
        }
        Cmd::Recipes => {
            for r in quickapp::recipes(app) {
                log(&format!("  {}", r.name));
            }
        }
        Cmd::New { recipe, name } => {
            let r = quickapp::recipes(app)
                .into_iter()
                .find(|r| r.name.eq_ignore_ascii_case(&recipe))
                .ok_or_else(|| anyhow!("No recipe named '{recipe}'. Run 'ds recipes'."))?;
            quickapp::create(app, &r, &name, &log)?;
        }
        Cmd::Ext { action, name } => {
            let v = app.active("php").ok_or_else(|| anyhow!("PHP is not installed."))?;
            match action.as_deref() {
                Some(a @ ("on" | "off")) => {
                    let n = name.ok_or_else(|| anyhow!("Which extension?"))?;
                    app.toggle_extension(&n, a == "on", &log)?;
                }
                Some(_) => return Err(anyhow!("Use: ds ext on <name> | ds ext off <name>")),
                None => {
                    let mut on = crate::configs::php_enabled_extensions(app, &v);
                    if app.settings.bool("php.xdebug") {
                        on.push("xdebug".into());
                    }
                    for e in crate::configs::php_available_extensions(&v) {
                        log(&format!("  [{}] {e}", if on.contains(&e) { "x" } else { " " }));
                    }
                }
            }
        }
        Cmd::Db(db) => {
            let t = database::MySqlTools::new(app);
            match db {
                DbCmd::Create { name } => {
                    t.create_database(&name)?;
                    log(&format!("Created {name}"));
                }
                DbCmd::List => {
                    for d in t.databases()? {
                        log(&format!("  {d}"));
                    }
                }
                DbCmd::Password { new } => t.change_root_password(&new, &log)?,
                DbCmd::Backup => database::auto_backup(app, &log, true)?,
                DbCmd::Recover => {
                    if confirm("Move the current MySQL data folder aside and start with an empty one?") {
                        if let Some(newest) = t.recover(&log)? {
                            if confirm(&format!("Restore the backup from {}?", newest.file_name().unwrap().to_string_lossy())) {
                                t.restore(&newest, &log)?;
                            }
                        }
                    }
                }
            }
        }
        Cmd::Share { site } => {
            let s = app.sites().into_iter().find(|s| s.name == site || s.host == site).ok_or_else(|| anyhow!("No site named {site}"))?;
            log(&share::start(app, &s, &log)?);
        }
        Cmd::Unshare => share::stop(app, &log)?,
        Cmd::Path { action } => match action.as_str() {
            "add" => app.add_to_path(&log)?,
            "remove" => app.remove_from_path(&log)?,
            _ => return Err(anyhow!("Use: ds path add | ds path remove")),
        },
        Cmd::Ssl { action } => match action.as_str() {
            "on" | "off" => app.set_ssl(action == "on", &log)?,
            "trust" => log(if ssl::trust(&app.paths)? { "Trusted" } else { "Not trusted (cancelled?)" }),
            "untrust" => {
                ssl::untrust(&app.paths)?;
            }
            "status" => log(&format!("HTTPS {}; CA trusted: {}", if app.settings.bool("ssl") { "on" } else { "off" },
                                    ssl::is_trusted(&app.paths).unwrap_or(false))),
            _ => return Err(anyhow!("Use: ds ssl on|off|trust|untrust|status")),
        },
        Cmd::Lan { action } => match action.as_str() {
            "on" | "off" => {
                app.settings.set("lan_access", serde_json::json!(action == "on"))?;
                app.reload(&log)?;
                log(if action == "on" {
                    "Sites are reachable from other devices on your network (Windows may ask to allow Apache/Nginx through the firewall)."
                } else {
                    "Sites are reachable from this computer only."
                });
            }
            _ => return Err(anyhow!("Use: ds lan on | ds lan off")),
        },
        Cmd::Update => {
            updates::daily(app, &log, true)?;
        }
        Cmd::Doctor => {
            let mut bad = 0;
            for (ok, msg) in app.doctor() {
                if !ok {
                    bad += 1;
                }
                log(&format!("  {} {msg}", if ok { "ok " } else { "!! " }));
            }
            return Ok(if bad > 0 { 1 } else { 0 });
        }
        Cmd::Terminal => app.open_terminal()?,
        Cmd::Autostart { action } => match action.as_str() {
            "on" | "off" => {
                app.set_autostart(action == "on")?;
                log(&format!("Run at Windows startup: {action}"));
            }
            _ => return Err(anyhow!("Use: ds autostart on | ds autostart off")),
        },
        Cmd::Setup { packages: ids, pause_on_error } => return setup(app, &ids, pause_on_error),
        Cmd::Resolve { ids } => {
            let all = packages::catalogue(app);
            let mut bad = 0;
            for p in all.iter().filter(|p| ids.is_empty() || ids.contains(&p.id)) {
                match packages::resolve(app, p) {
                    Ok(r) => {
                        let sum = match (&r.sha256, &r.sha512) {
                            (Some(_), _) => "sha256",
                            (_, Some(_)) => "sha512",
                            _ => "no checksum",
                        };
                        log(&format!("  ok  {:<18} {:<26} {sum:<11} {}", p.id, r.folder, r.url));
                    }
                    Err(e) => {
                        bad += 1;
                        log(&format!("  !!  {:<18} {e:#}", p.id));
                    }
                }
            }
            return Ok(i32::from(bad > 0));
        }
    }
    Ok(0)
}

/// Download the chosen packages, enable the services they belong to, pick the web server.
fn setup(app: &App, ids: &[String], pause_on_error: bool) -> Result<i32> {
    let mut failed = vec![];
    let mut tools = vec![];
    for id in ids.iter().filter(|i| !i.trim().is_empty()) {
        log(&format!("\n=== {id}"));
        match packages::find(app, id).and_then(|p| {
            packages::install(app, &p, &log)?;
            Ok(p.tool)
        }) {
            Ok(tool) => tools.push(tool),
            Err(e) => {
                log(&format!("FAILED: {e:#}"));
                failed.push(id.clone());
            }
        }
    }
    for s in app.services() {
        if tools.iter().any(|t| t == s.tool()) && !matches!(s.id(), "apache" | "nginx" | "php") {
            s.set_enabled(app, true)?;
        }
    }
    let has = |t: &str| tools.iter().any(|x| x == t);
    if has("nginx") && !has("apache") {
        app.settings.set("web_server", serde_json::json!("nginx"))?;
    } else if has("apache") {
        app.settings.set("web_server", serde_json::json!("apache"))?;
    }
    app.generate(&log)?;
    if failed.is_empty() {
        log("\nAll done. Open devstack and click Start All.");
        Ok(0)
    } else {
        log(&format!("\nThese couldn't be downloaded: {}. You can add them later from Menu > Tools > Quick add.", failed.join(", ")));
        if pause_on_error {
            confirm("Press Enter to close");
        }
        Ok(1)
    }
}
