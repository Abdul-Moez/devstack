//! One menu definition, rendered both as the window's "Menu" button (egui) and as the tray
//! icon's right-click menu (native).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::configs::{php_available_extensions, php_enabled_extensions};
use crate::core::App;
use crate::{packages, quickapp, share};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    ShowWindow,
    StartAll,
    StopAll,
    Reload,
    Exit,
    ToggleService(String),
    OpenUrl(String),
    OpenPath(PathBuf),
    Edit(PathBuf),
    OpenWeb,
    OpenDatabase,
    OpenMail,
    Share(String),
    StopShare,
    QuickApp(usize),
    QuickAdd(String),
    WebServer(String),
    Ssl(bool),
    TrustCa,
    Version(String, String),
    Ext(String, bool),
    CheckUpdates,
    UpdateCacert,
    CreateDb,
    ChangePw,
    CopyPw,
    BackupNow,
    RecoverMysql,
    PathAdd,
    PathRemove,
    Terminal,
    Doctor,
    Preferences,
}

impl Action {
    pub fn id(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub fn from_id(id: &str) -> Option<Action> {
        serde_json::from_str(id).ok()
    }
}

pub enum Node {
    Item(String, Action),
    Disabled(String),
    Check(String, bool, Action),
    Sub(String, Vec<Node>),
    Sep,
}

fn item(label: impl Into<String>, a: Action) -> Node {
    Node::Item(label.into(), a)
}

const OTHER_VERSION_TOOLS: &[&str] = &["apache", "nginx", "postgresql", "mongodb", "redis", "memcached", "mailpit",
                                       "nodejs", "python", "go", "composer", "git", "phpmyadmin"];

fn version_menu(app: &App, tool: &str) -> Vec<Node> {
    let versions = app.versions(tool);
    if versions.is_empty() {
        return vec![Node::Disabled("(not installed)".into())];
    }
    let active = app.active(tool).map(|v| v.name);
    let mut out: Vec<Node> = versions
        .iter()
        .map(|v| Node::Check(v.name.clone(), Some(&v.name) == active.as_ref(), Action::Version(tool.into(), v.name.clone())))
        .collect();
    out.push(Node::Sep);
    out.push(item("Open folder", Action::OpenPath(app.versions_root(tool))));
    out
}

fn tool_label(tool: &str) -> String {
    match tool {
        "php" => "PHP".into(),
        "mysql" => "MySQL / MariaDB".into(),
        "postgresql" => "PostgreSQL".into(),
        "mongodb" => "MongoDB".into(),
        "nodejs" => "Node.js".into(),
        "phpmyadmin" => "phpMyAdmin".into(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
    }
}

/// Quick add grouped by tool (in catalogue order); tools with a single entry stay top-level.
pub fn quick_add_menu(catalogue: &[packages::Package]) -> Vec<Node> {
    let mut tools: Vec<&str> = vec![];
    for pk in catalogue {
        if !tools.contains(&pk.tool.as_str()) {
            tools.push(&pk.tool);
        }
    }
    let mut out = vec![];
    for tool in tools {
        let mut entries: Vec<Node> =
            catalogue.iter().filter(|pk| pk.tool == tool).map(|pk| item(pk.label.clone(), Action::QuickAdd(pk.id.clone()))).collect();
        if entries.len() == 1 {
            out.append(&mut entries);
        } else {
            out.push(Node::Sub(tool_label(tool), entries));
        }
    }
    out
}

pub fn model(app: &App) -> Vec<Node> {
    let p = app.paths();
    let sites = app.sites();
    let web = app.settings.str("web_server");

    let mut www: Vec<Node> = sites.iter().map(|s| item(s.host.clone(), Action::OpenUrl(s.url()))).collect();
    if sites.is_empty() {
        www.push(Node::Disabled("(no projects yet)".into()));
    }
    let mut share_menu: Vec<Node> = sites.iter().map(|s| item(s.host.clone(), Action::Share(s.name.clone()))).collect();
    if let Some(r) = share::running(app) {
        share_menu.push(Node::Sep);
        share_menu.push(item(format!("Stop sharing {}", r.site.unwrap_or_default()), Action::StopShare));
    }
    www.extend([Node::Sep, Node::Sub("Share".into(), share_menu), item("Open www folder", Action::OpenPath(p.www.clone())),
                item("Site overrides (usr/sites.json)", Action::Edit(p.usr.join("sites.json")))]);

    let mut qa: Vec<Node> = quickapp::recipes(app).iter().enumerate().map(|(i, r)| item(r.name.clone(), Action::QuickApp(i))).collect();
    qa.extend([Node::Sep, item("Edit recipes (usr/quickapps.json)", Action::Edit(p.usr.join("quickapps.json")))]);

    let ssl_on = app.settings.bool("ssl");
    let webm = vec![
        Node::Check("Apache".into(), web == "apache", Action::WebServer("apache".into())),
        Node::Check("Nginx".into(), web == "nginx", Action::WebServer("nginx".into())),
        Node::Check("None".into(), web.is_empty(), Action::WebServer(String::new())),
        Node::Sep,
        Node::Check("HTTPS for all sites".into(), ssl_on, Action::Ssl(!ssl_on)),
        item("Trust local CA (no browser warnings)", Action::TrustCa),
        Node::Sep,
        item("Apache extra config (usr/apache)", Action::OpenPath(p.usr.join("apache"))),
        item("Nginx extra config (usr/nginx)", Action::OpenPath(p.usr.join("nginx"))),
        item("Generated configs (etc)", Action::OpenPath(p.etc.clone())),
        item("Logs", Action::OpenPath(p.logs.clone())),
    ];

    let mut exts = vec![];
    match app.active("php") {
        Some(v) => {
            let mut on = php_enabled_extensions(app, &v);
            if app.settings.bool("php.xdebug") {
                on.push("xdebug".into());
            }
            for e in php_available_extensions(&v).into_iter().filter(|e| e != "opcache") {
                let checked = on.contains(&e);
                exts.push(Node::Check(e.clone(), checked, Action::Ext(e, !checked)));
            }
            if exts.is_empty() {
                exts.push(Node::Disabled("(no extensions found)".into()));
            }
        }
        None => exts.push(Node::Disabled("(PHP not installed)".into())),
    }
    let php = vec![
        Node::Sub("Version".into(), version_menu(app, "php")),
        Node::Sub("Extensions".into(), exts),
        item("php.ini additions (usr/php.ini)", Action::Edit(p.usr.join("php.ini"))),
        item("Check for PHP updates", Action::CheckUpdates),
    ];

    let port = app.settings.port("http");
    let base = if port == 80 { "http://localhost".to_string() } else { format!("http://localhost:{port}") };
    let mysql = vec![
        Node::Sub("Version".into(), version_menu(app, "mysql")),
        item("phpMyAdmin", Action::OpenUrl(format!("{base}/phpmyadmin/"))),
        item("Create database…", Action::CreateDb),
        item("Change root password…", Action::ChangePw),
        item("Copy root password", Action::CopyPw),
        item("Back up all databases now", Action::BackupNow),
        item("Open backup folder", Action::OpenPath(p.backup.clone())),
        item("Recover data folder…", Action::RecoverMysql),
        item("my.ini additions (usr/my.ini)", Action::Edit(p.usr.join("my.ini"))),
    ];

    let mut others: Vec<Node> = OTHER_VERSION_TOOLS
        .iter()
        .filter(|t| !app.versions(t).is_empty())
        .map(|t| Node::Sub(t.to_string(), version_menu(app, t)))
        .collect();
    if others.is_empty() {
        others.push(Node::Disabled("(nothing else installed)".into()));
    }

    let mut qadd = quick_add_menu(&packages::catalogue(app));
    qadd.extend([Node::Sep, item("Edit package list (usr/packages.json)", Action::Edit(p.usr.join("packages.json")))]);
    let tools = vec![
        Node::Sub("Quick add".into(), qadd),
        Node::Sub("PATH".into(), vec![item("Add devstack to PATH", Action::PathAdd), item("Remove devstack from PATH", Action::PathRemove)]),
        item("Terminal", Action::Terminal),
        item("Update cacert.pem now", Action::UpdateCacert),
        item("Doctor (check setup)", Action::Doctor),
    ];

    vec![
        item("Show window", Action::ShowWindow),
        Node::Sep,
        item("Start All", Action::StartAll),
        item("Stop All", Action::StopAll),
        item("Reload", Action::Reload),
        Node::Sep,
        Node::Sub("www".into(), www),
        Node::Sub("Quick app".into(), qa),
        Node::Sub("Web server".into(), webm),
        Node::Sub("PHP".into(), php),
        Node::Sub("MySQL".into(), mysql),
        Node::Sub("Other versions".into(), others),
        item("Mailpit inbox", Action::OpenMail),
        Node::Sub("Tools".into(), tools),
        Node::Sep,
        item("Preferences…", Action::Preferences),
        item("Open root folder", Action::OpenPath(p.root.clone())),
        Node::Sep,
        item("Exit", Action::Exit),
    ]
}

/// Render as egui menu entries; clicked actions are pushed to `out`.
pub fn egui_menu(ui: &mut eframe::egui::Ui, nodes: &[Node], out: &mut Vec<Action>) {
    for n in nodes {
        match n {
            Node::Item(label, a) => {
                if ui.button(label).clicked() {
                    out.push(a.clone());
                    ui.close_menu();
                }
            }
            Node::Disabled(label) => {
                ui.add_enabled(false, eframe::egui::Button::new(label));
            }
            Node::Check(label, checked, a) => {
                let mut v = *checked;
                if ui.checkbox(&mut v, label).clicked() {
                    out.push(a.clone());
                    ui.close_menu();
                }
            }
            Node::Sub(label, kids) => {
                ui.menu_button(label, |ui| egui_menu(ui, kids, out));
            }
            Node::Sep => {
                ui.separator();
            }
        }
    }
}

/// Count entries (used by the tests and the smoke check).
pub fn count(nodes: &[Node]) -> usize {
    nodes.iter().map(|n| match n {
        Node::Sub(_, kids) => 1 + count(kids),
        Node::Sep => 0,
        _ => 1,
    }).sum()
}
