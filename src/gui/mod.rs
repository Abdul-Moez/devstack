//! Tray icon + main window (eframe/egui). All real work happens in `core::App`; this module
//! wires the window and the tray to it and runs slow work on background threads.
//!
//! The window is a sidebar of pages (pages.rs), so nothing pops up over anything else; the
//! tray keeps the native right-click menu (menu.rs). The look lives in theme.rs.

pub mod menu;
mod pages;
mod theme;
mod tray;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui::{self, Color32, Margin, RichText, Stroke};
use serde_json::{json, Value};

use crate::core::App;
use crate::error::needs_force;
use crate::packages::Package;
use crate::quickapp::Recipe;
use crate::services::php_heal;
use crate::settings::ProcRec;
use crate::sites::Site;
use crate::{assets, configs, database, packages, quickapp, share, ssl, updates, winutil, Log, APP_NAME};
use menu::Action;
use theme::{icon, Kind};

// ------------------------------------------------------------------------- background jobs
pub enum Outcome {
    Nothing,
    StartFailed(Vec<String>),
    StopPending(Vec<String>, bool),
    OpenUrl(String),
    ShareUrl(String),
    Doctor(Vec<(bool, String)>),
    Recovered(Option<PathBuf>),
}

pub enum Msg {
    Log(String),
    Done { quiet: bool, result: std::result::Result<Outcome, (String, Option<String>)> },
    Action(Action),
    /// Projects whose index file sits in a subfolder: project name -> that folder.
    RootHints(HashMap<String, String>),
}

/// Shared by the window, the tray handlers and the ticker thread.
#[derive(Clone)]
pub struct Jobs {
    pub app: Arc<App>,
    tx: Sender<Msg>,
    pub ctx: egui::Context,
    busy: Arc<AtomicUsize>,
    /// Keys of jobs in progress that the window shows as such ("quickadd:<id>", "quickapp").
    pending: Arc<Mutex<HashSet<String>>>,
    pub hwnd: Arc<AtomicIsize>,
}

impl Jobs {
    pub fn log(&self, m: &str) {
        self.app.log_line(m);
        let _ = self.tx.send(Msg::Log(m.to_string()));
        self.ctx.request_repaint();
    }

    fn show(&self) {
        winutil::show_window(self.hwnd.load(Ordering::SeqCst));
        self.ctx.request_repaint();
    }

    fn is_pending(&self, key: &str) -> bool {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).contains(key)
    }

    /// Run `f` on a background thread. `busy` greys out Start/Stop/Reload while it runs.
    pub fn spawn(&self, busy: bool, quiet: bool, f: impl FnOnce(&App, Log) -> Result<Outcome> + Send + 'static) {
        if busy {
            self.busy.fetch_add(1, Ordering::SeqCst);
        }
        let me = self.clone();
        std::thread::spawn(move || {
            let log = |m: &str| me.log(m);
            let result = f(&me.app, &log).map_err(|e| (format!("{e:#}"), needs_force(&e).map(str::to_string)));
            if busy {
                me.busy.fetch_sub(1, Ordering::SeqCst);
            }
            let needs_attention = match &result {
                Err(_) => !quiet,
                Ok(Outcome::ShareUrl(_) | Outcome::Doctor(_)) | Ok(Outcome::Recovered(Some(_))) => true,
                Ok(Outcome::StopPending(p, _)) => !p.is_empty(),
                _ => false,
            };
            let _ = me.tx.send(Msg::Done { quiet, result });
            if needs_attention {
                me.show();
            }
            me.ctx.request_repaint();
        });
    }

    /// Like `spawn`, with `key` marked as in progress until the job ends.
    fn spawn_keyed(&self, key: String, f: impl FnOnce(&App, Log) -> Result<Outcome> + Send + 'static) {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone());
        let pending = self.pending.clone();
        self.spawn(false, false, move |app, log| {
            let r = f(app, log);
            pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
            r
        });
    }
}

/// Actions that don't need the window. Returns false if the window must handle it.
fn background(jobs: &Jobs, action: &Action) -> bool {
    let a = action.clone();
    match a {
        Action::StartAll => jobs.spawn(true, false, |app, log| Ok(Outcome::StartFailed(app.start_all(log)?))),
        Action::StopAll => jobs.spawn(true, false, |app, log| Ok(Outcome::StopPending(app.stop_all(log, false)?, false))),
        Action::Reload => jobs.spawn(true, false, |app, log| app.reload(log).map(|_| Outcome::Nothing)),
        Action::ToggleService(id) => jobs.spawn(true, false, move |app, log| {
            if app.svc(&id).running(app) { app.stop(&id, log, false)? } else { app.start(&id, log)? }
            Ok(Outcome::Nothing)
        }),
        Action::WebServer(w) => jobs.spawn(true, false, move |app, log| app.set_web_server(&w, log).map(|_| Outcome::Nothing)),
        Action::Ssl(on) => jobs.spawn(true, false, move |app, log| app.set_ssl(on, log).map(|_| Outcome::Nothing)),
        Action::Lan(on) => jobs.spawn(true, false, move |app, log| {
            app.settings.set("lan_access", json!(on))?;
            app.reload(log)?;
            log(if on { "Sites can now be opened from other devices on your network." } else { "Sites are reachable from this computer only." });
            Ok(Outcome::Nothing)
        }),
        Action::Version(tool, name) => jobs.spawn(true, false, move |app, log| app.switch_version(&tool, &name, log).map(|_| Outcome::Nothing)),
        Action::Project(name) => jobs.spawn(true, false, move |app, log| app.set_project(name.as_deref(), log).map(|_| Outcome::Nothing)),
        Action::SaveProject(name, docroot, versions) => {
            jobs.spawn(true, false, move |app, log| app.save_project_settings(&name, docroot.as_deref(), &versions, log).map(|_| Outcome::Nothing))
        }
        Action::Discover => jobs.spawn_keyed("discover".into(), |app, log| {
            if packages::discover(app, log)? == 0 {
                log("No new versions since the last check.");
            }
            Ok(Outcome::Nothing)
        }),
        Action::Ext(e, on) => jobs.spawn(true, false, move |app, log| app.toggle_extension(&e, on, log).map(|_| Outcome::Nothing)),
        Action::TrustCa => jobs.spawn(false, false, |app, log| {
            log(if ssl::trust(&app.paths)? { "Local CA trusted. Restart your browser if it still warns." } else { "The CA was not trusted (cancelled)." });
            Ok(Outcome::Nothing)
        }),
        Action::CheckUpdates => jobs.spawn(false, false, |app, log| {
            if updates::daily(app, log, true)?.is_empty() {
                log("Everything is up to date");
            }
            Ok(Outcome::Nothing)
        }),
        Action::UpdateCacert => jobs.spawn(false, false, |app, log| {
            if !updates::update_cacert(app, log)? {
                log("cacert.pem is current");
            }
            Ok(Outcome::Nothing)
        }),
        Action::BackupNow => jobs.spawn(false, false, |app, log| database::auto_backup(app, log, true).map(|_| Outcome::Nothing)),
        Action::PathAdd => jobs.spawn(false, false, |app, log| app.add_to_path(log).map(|_| Outcome::Nothing)),
        Action::PathRemove => jobs.spawn(false, false, |app, log| app.remove_from_path(log).map(|_| Outcome::Nothing)),
        Action::QuickAdd(id) => jobs.spawn_keyed(format!("quickadd:{id}"), move |app, log| {
            let pkg = packages::find(app, &id)?;
            packages::install(app, &pkg, log)?;
            app.generate(log)?;
            Ok(Outcome::Nothing)
        }),
        Action::Share(name) => jobs.spawn(false, false, move |app, log| {
            let site = app.sites().into_iter().find(|s| s.name == name).ok_or_else(|| anyhow::anyhow!("No site named {name}"))?;
            Ok(Outcome::ShareUrl(share::start(app, &site, log)?))
        }),
        Action::StopShare => jobs.spawn(false, false, |app, log| share::stop(app, log).map(|_| Outcome::Nothing)),
        Action::Doctor => jobs.spawn(false, false, |app, _| Ok(Outcome::Doctor(app.doctor()))),
        Action::Terminal => report(jobs, jobs.app.open_terminal()),
        Action::OpenUrl(u) => report(jobs, winutil::open_url(&u)),
        Action::OpenPath(p) => report(jobs, winutil::open_path(&p)),
        Action::Edit(p) => report(jobs, jobs.app.edit(&p)),
        Action::OpenWeb => report(jobs, winutil::open_url(&local_url(&jobs.app, "/"))),
        Action::OpenMail => report(jobs, winutil::open_url(&format!("http://127.0.0.1:{}/", jobs.app.settings.port("mailpit_ui")))),
        Action::OpenDatabase => report(jobs, open_database(&jobs.app)),
        Action::ShowWindow => jobs.show(),
        _ => return false,
    }
    true
}

fn report(jobs: &Jobs, r: Result<()>) {
    if let Err(e) = r {
        jobs.log(&format!("ERROR: {e:#}"));
    }
}

fn local_url(app: &App, path: &str) -> String {
    let port = app.settings.port("http");
    if port == 80 { format!("http://localhost{path}") } else { format!("http://localhost:{port}{path}") }
}

fn open_database(app: &App) -> Result<()> {
    for tool in ["heidisql", "dbeaver"] {
        if let Some(v) = app.active(tool) {
            return winutil::open_path(&v.exe);
        }
    }
    if app.active("phpmyadmin").is_some() {
        return winutil::open_url(&local_url(app, "/phpmyadmin/"));
    }
    anyhow::bail!("Add HeidiSQL, DBeaver or phpMyAdmin from the Install page.")
}

/// Called by the tray menu on the UI thread, even while the window is hidden.
pub(crate) fn tray_action(jobs: &Jobs, action: Action) {
    // Exit from the tray while the window is hidden: handing it to the window waits for a
    // frame that a hidden window may never get, so the process stayed alive. With nothing to
    // stop, leave right here (tray events arrive on the UI thread).
    if matches!(action, Action::Exit) {
        let app = &jobs.app;
        if !(app.settings.bool("stop_all_on_exit") && app.services().iter().any(|s| s.running(app))) {
            tray::remove();
            std::process::exit(0);
        }
    }
    if !background(jobs, &action) {
        jobs.show();
        let _ = jobs.tx.send(Msg::Action(action));
        jobs.ctx.request_repaint();
    }
}

// ------------------------------------------------------------------------- window state
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Overview,
    Sites,
    NewProject,
    Install,
    Php,
    Databases,
    Tools,
    Logs,
    Settings,
}

impl Page {
    /// Sidebar order; the inner arrays are visual groups.
    const GROUPS: [&'static [Page]; 3] = [
        &[Page::Overview, Page::Sites, Page::NewProject],
        &[Page::Install, Page::Php, Page::Databases],
        &[Page::Tools, Page::Logs, Page::Settings],
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Sites => "Sites",
            Page::NewProject => "New project",
            Page::Install => "Install",
            Page::Php => "PHP",
            Page::Databases => "Databases",
            Page::Tools => "Tools",
            Page::Logs => "Logs",
            Page::Settings => "Settings",
        }
    }

    fn icon(self) -> char {
        match self {
            Page::Overview => icon::HOME,
            Page::Sites => icon::GLOBE,
            Page::NewProject => icon::ADD,
            Page::Install => icon::DOWNLOAD,
            Page::Php => icon::CODE,
            Page::Databases => icon::DATABASE,
            Page::Tools => icon::TOOLS,
            Page::Logs => icon::LIST,
            Page::Settings => icon::SETTINGS,
        }
    }

    /// "install" or "install:php" (page plus a detail, e.g. the tool to show).
    fn parse(s: &str) -> Option<(Page, Option<String>)> {
        let (name, detail) = match s.split_once(':') {
            Some((n, d)) => (n, Some(d.to_string())),
            None => (s, None),
        };
        let page = match name.to_lowercase().as_str() {
            "overview" | "home" => Page::Overview,
            "sites" => Page::Sites,
            "new" | "newproject" | "quickapp" => Page::NewProject,
            "install" | "quickadd" => Page::Install,
            "php" => Page::Php,
            "databases" | "mysql" => Page::Databases,
            "tools" => Page::Tools,
            "logs" => Page::Logs,
            "settings" | "preferences" => Page::Settings,
            _ => return None,
        };
        Some((page, detail))
    }
}

struct Row {
    id: String,
    name: String,
    tool: String,
    status: String,
    ports: String,
    enabled: bool,
    installed: bool,
    versions: Vec<String>,
    active: Option<String>,
    /// The chosen project sets this tool's version.
    overridden: bool,
    /// Versions that are Laragon's (used in place).
    laragon: Vec<String>,
}

impl Row {
    fn up(&self) -> bool {
        self.status == "running" || self.status == "degraded"
    }
}

/// What the PHP page shows about the active PHP.
struct PhpInfo {
    version: String,
    thread_safe: bool,
    /// (enabled, name), sorted by name
    extensions: Vec<(bool, String)>,
}

enum InputThen {
    CreateDb,
    ChangePw,
    KnownPw,
}

enum ConfirmThen {
    ForceStop(String),
    ForceStopAll { then_exit: bool },
    Recover,
    Restore(PathBuf),
    UseLaragon(PathBuf),
}

enum Dialog {
    Input { title: String, prompt: String, value: String, password: bool, then: InputThen },
    Confirm { title: String, text: String, yes: String, then: ConfirmThen },
    Info { title: String, text: String },
}

struct ShareView {
    url: String,
    tex: egui::TextureHandle,
}

pub struct GuiApp {
    jobs: Jobs,
    rx: Receiver<Msg>,
    log: Vec<String>,
    page: Page,
    // cached by refresh()
    rows: Vec<Row>,
    sites: Vec<Site>,
    sharing: Option<ProcRec>,
    php: Option<PhpInfo>,
    catalogue: Vec<Package>,
    recipes: Vec<Recipe>,
    installed: HashMap<String, (Vec<String>, Option<String>)>,
    project: Option<String>,
    project_versions: BTreeMap<String, String>,
    /// Names of installed versions that are Laragon's.
    external: HashSet<String>,
    /// Sites page: the project whose versions are being edited, with the draft.
    editing: Option<pages::ProjectDraft>,
    /// Sites page: projects whose index file is in a subfolder (found off the UI thread).
    root_hints: HashMap<String, String>,
    hints_at: Option<Instant>,
    ssl_on: bool,
    lan_on: bool,
    web: String,
    last_refresh: Instant,
    // window behavior
    start_hidden: bool,
    first_frame: bool,
    exiting: bool,
    tray_hint_shown: bool,
    // overlays and page state
    dialog: Option<Dialog>,
    share: Option<ShareView>,
    prefs: Option<pages::Prefs>,
    new_name: String,
    new_recipe: Option<usize>,
    install_tool: String,
    logo: egui::TextureHandle,
}

impl GuiApp {
    fn new(cc: &eframe::CreationContext<'_>, app: Arc<App>, start_hidden: bool, page: Option<String>) -> Self {
        theme::install(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let hwnd = Arc::new(AtomicIsize::new(0));
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = cc.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    hwnd.store(w.hwnd.get(), Ordering::SeqCst);
                }
            }
        }
        let jobs = Jobs {
            app,
            tx,
            ctx: cc.egui_ctx.clone(),
            busy: Arc::new(AtomicUsize::new(0)),
            pending: Arc::new(Mutex::new(HashSet::new())),
            hwnd,
        };
        if cfg!(windows) {
            if let Err(e) = tray::create(jobs.clone()) {
                jobs.log(&format!("Couldn't create the tray icon: {e:#}"));
            }
        }
        start_ticker(jobs.clone());
        if jobs.app.settings.bool("start_all_on_launch") {
            background(&jobs, &Action::StartAll);
        }
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([assets::ICON_BRAND_SIZE as usize; 2], assets::ICON_BRAND),
            egui::TextureOptions::LINEAR,
        );
        let mut me = GuiApp {
            jobs,
            rx,
            log: vec![],
            page: Page::Overview,
            rows: vec![],
            sites: vec![],
            sharing: None,
            php: None,
            catalogue: vec![],
            recipes: vec![],
            installed: HashMap::new(),
            project: None,
            project_versions: BTreeMap::new(),
            external: HashSet::new(),
            editing: None,
            root_hints: HashMap::new(),
            hints_at: None,
            ssl_on: false,
            lan_on: false,
            web: String::new(),
            last_refresh: Instant::now(),
            start_hidden,
            first_frame: true,
            exiting: false,
            tray_hint_shown: false,
            dialog: None,
            share: None,
            prefs: None,
            new_name: String::new(),
            new_recipe: None,
            install_tool: String::new(),
            logo,
        };
        me.refresh();
        if let Some(p) = page {
            me.go(&p);
        }
        let app = me.jobs.app.clone();
        if !app.settings.bool("laragon.asked") && crate::laragon::root(&app).is_none() {
            if let Some(found) = crate::laragon::detect(&app) {
                me.dialog = Some(Dialog::Confirm {
                    title: "Laragon found".into(),
                    text: format!(
                        "devstack found Laragon at {}. Use its projects (www), its installed PHP, MySQL, Node.js and Python versions, and its databases?

They're used in place; devstack doesn't change Laragon's folders. Don't run Laragon and devstack at the same time. You can change this later in Settings > Laragon.",
                        found.display()
                    ),
                    yes: "Use Laragon's folders".into(),
                    then: ConfirmThen::UseLaragon(found),
                });
            }
        }
        me
    }

    fn app(&self) -> &App {
        &self.jobs.app
    }

    fn busy(&self) -> bool {
        self.jobs.busy.load(Ordering::SeqCst) > 0
    }

    fn push_log(&mut self, m: String) {
        let time = chrono::Local::now().format("%H:%M:%S");
        for line in m.lines() {
            self.log.push(format!("{time}  {line}"));
        }
        let n = self.log.len();
        if n > 3000 {
            self.log.drain(..n - 3000);
        }
    }

    /// Switch page; "install:php" also selects the tool, "settings:php" the tab.
    fn go(&mut self, target: &str) {
        let Some((page, detail)) = Page::parse(target) else { return };
        self.page = page;
        match (page, detail) {
            (Page::Install, Some(tool)) => self.install_tool = tool,
            (Page::Sites, Some(project)) => {
                if let Some(s) = self.sites.iter().find(|s| s.name == project) {
                    self.editing = Some(pages::ProjectDraft::new(s));
                }
            }
            (Page::Settings, Some(tab)) => {
                let app = self.jobs.app.clone();
                let prefs = self.prefs.get_or_insert_with(|| pages::Prefs::load(&app));
                prefs.tab = match tab.as_str() {
                    "php" => 1,
                    "ports" => 2,
                    "backups" | "sharing" => 3,
                    "laragon" => 4,
                    _ => 0,
                };
            }
            _ => {}
        }
    }

    fn refresh(&mut self) {
        let app = self.jobs.app.clone();
        // Pick up changes made with `ds` (or by hand) while the window is open.
        app.settings.refresh();
        app.secrets.refresh();
        self.project = app.project();
        self.project_versions = app.project_versions();
        let mut any_up = false;
        self.rows = app
            .services()
            .iter()
            .map(|s| {
                let status = s.status(&app);
                let installed = s.installed(&app);
                let ports = if installed && status != "in Apache" { s.ports(&app) } else { vec![] };
                let ports = match ports.len() {
                    0 => String::new(),
                    1 => ports[0].to_string(),
                    _ if s.id() == "php" => format!("{}–{}", ports[0], ports[ports.len() - 1]),
                    _ => ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", "),
                };
                let row = Row {
                    id: s.id().into(),
                    name: s.name().into(),
                    tool: s.tool().into(),
                    status,
                    ports,
                    enabled: s.enabled(&app),
                    installed,
                    versions: app.versions(s.tool()).into_iter().map(|v| v.name).collect(),
                    active: app.active(s.tool()).map(|v| v.name),
                    overridden: self.project_versions.contains_key(s.tool()),
                    laragon: app.versions(s.tool()).into_iter().filter(|v| v.external).map(|v| v.name).collect(),
                };
                any_up |= row.up();
                row
            })
            .collect();
        self.sites = app.sites();
        self.sharing = share::running(&app);
        self.ssl_on = app.settings.bool("ssl");
        self.lan_on = app.settings.bool("lan_access");
        self.web = app.settings.str("web_server");
        self.catalogue = packages::catalogue(&app);
        self.recipes = quickapp::recipes(&app);
        self.installed.clear();
        self.external.clear();
        for p in &self.catalogue {
            self.external.extend(app.versions(&p.tool).into_iter().filter(|v| v.external).map(|v| v.name));
            self.installed
                .entry(p.tool.clone())
                .or_insert_with(|| (app.versions(&p.tool).into_iter().map(|v| v.name).collect(), app.active(&p.tool).map(|v| v.name)));
        }
        self.php = app.active("php").map(|v| {
            let mut on = configs::php_enabled_extensions(&app, &v);
            if app.settings.bool("php.xdebug") {
                on.push("xdebug".into());
            }
            PhpInfo {
                version: v.name.clone(),
                thread_safe: configs::php_apache_module(&app, Some(&v)).is_some(),
                extensions: configs::php_available_extensions(&v)
                    .into_iter()
                    .filter(|e| e != "opcache")
                    .map(|e| (on.contains(&e), e))
                    .collect(),
            }
        });
        self.last_refresh = Instant::now();
        tray::set_running(any_up);
    }

    /// Look, off the UI thread, for projects whose web folder has no index file while one of
    /// their subfolders does (the Sites page offers to serve that folder).
    fn scan_root_hints(&mut self) {
        self.hints_at = Some(Instant::now());
        let jobs = self.jobs.clone();
        std::thread::spawn(move || {
            let hints: HashMap<String, String> = jobs
                .app
                .sites()
                .into_iter()
                .filter(|s| s.proxy.is_none() && s.docroot_rel.is_none() && !crate::sites::has_index(&s.docroot))
                .filter_map(|s| crate::sites::docroot_candidates(&s.path, 1).into_iter().next().map(|folder| (s.name, folder)))
                .collect();
            let _ = jobs.tx.send(Msg::RootHints(hints));
            jobs.ctx.request_repaint();
        });
    }

    fn hide(&mut self, ctx: &egui::Context) {
        let hwnd = self.jobs.hwnd.load(Ordering::SeqCst);
        if hwnd != 0 {
            winutil::hide_window(hwnd);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if !self.tray_hint_shown {
            self.jobs.log("devstack is still running in the tray. Right-click the icon for the menu.");
            self.tray_hint_shown = true;
        }
    }

    /// Leave for good. Asking eframe to close (ViewportCommand::Close) needs one more frame,
    /// which never comes when the window is hidden or idle, so the process stayed in Task
    /// Manager. Nothing is left to save at this point (services run detached, settings are
    /// written as they change), so end the process directly.
    fn quit(&mut self, _ctx: &egui::Context) {
        self.exiting = true;
        tray::remove(); // takes the icon out of the notification area now, not on hover
        std::process::exit(0);
    }

    fn exit(&mut self, ctx: &egui::Context) {
        let app = self.app();
        let anything = app.services().iter().any(|s| s.running(app));
        if app.settings.bool("stop_all_on_exit") && anything {
            self.jobs.log("Stopping services before exit…");
            self.jobs.spawn(true, false, |app, log| Ok(Outcome::StopPending(app.stop_all(log, false)?, true)));
        } else {
            self.quit(ctx);
        }
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Log(m) => self.push_log(m),
                Msg::Action(a) => self.dispatch(ctx, a),
                Msg::RootHints(hints) => self.root_hints = hints,
                Msg::Done { quiet, result } => {
                    self.refresh();
                    self.hints_at = None; // something changed: look again
                    match result {
                        Ok(o) => self.outcome(ctx, o),
                        Err((msg, force)) => {
                            if let Some(sid) = force {
                                self.dialog = Some(Dialog::Confirm {
                                    title: "Force stop?".into(),
                                    text: msg,
                                    yes: "Force stop".into(),
                                    then: ConfirmThen::ForceStop(sid),
                                });
                            } else {
                                self.push_log(format!("ERROR: {msg}"));
                                if !quiet {
                                    self.dialog = Some(Dialog::Info { title: "That didn't work".into(), text: msg });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn outcome(&mut self, ctx: &egui::Context, o: Outcome) {
        match o {
            Outcome::Nothing | Outcome::Recovered(None) => {}
            Outcome::StartFailed(f) if !f.is_empty() => {
                self.dialog = Some(Dialog::Info {
                    title: "Some services didn't start".into(),
                    text: format!("Failed: {}. The Logs page has the details.", f.join(", ")),
                });
            }
            Outcome::StartFailed(_) => {}
            Outcome::StopPending(p, then_exit) => {
                if p.is_empty() {
                    if then_exit {
                        self.quit(ctx);
                    }
                } else {
                    self.dialog = Some(Dialog::Confirm {
                        title: "Force stop?".into(),
                        text: "Some databases didn't shut down cleanly. Force them to stop? Unsaved data may be lost; they recover on the next start.".into(),
                        yes: "Force stop".into(),
                        then: ConfirmThen::ForceStopAll { then_exit },
                    });
                }
            }
            Outcome::OpenUrl(u) => {
                if self.app().settings.bool("quick_app.open_browser") {
                    let _ = winutil::open_url(&u);
                }
            }
            Outcome::ShareUrl(url) => {
                let tex = qr_texture(ctx, &url);
                self.share = Some(ShareView { url, tex });
            }
            Outcome::Doctor(results) => {
                let text = results.iter().map(|(ok, m)| format!("{}  {m}", if *ok { "ok" } else { "!!" })).collect::<Vec<_>>().join("\n");
                self.dialog = Some(Dialog::Info { title: "Setup check".into(), text });
            }
            Outcome::Recovered(Some(newest)) => {
                self.dialog = Some(Dialog::Confirm {
                    title: "Restore a backup?".into(),
                    text: format!("Restore the backup from {}?", newest.file_name().unwrap_or_default().to_string_lossy()),
                    yes: "Restore".into(),
                    then: ConfirmThen::Restore(newest),
                });
            }
        }
    }

    fn dispatch(&mut self, ctx: &egui::Context, action: Action) {
        if background(&self.jobs, &action) {
            return;
        }
        match action {
            Action::Exit => self.exit(ctx),
            Action::Page(target) => self.go(&target),
            Action::Preferences => self.go("settings"),
            Action::QuickApp(i) => {
                self.page = Page::NewProject;
                self.new_recipe = Some(i);
            }
            Action::CreateProject(i, name) => {
                let Some(recipe) = self.recipes.get(i).cloned() else { return };
                self.jobs.spawn_keyed("quickapp".into(), move |app, log| Ok(Outcome::OpenUrl(quickapp::create(app, &recipe, &name, log)?)));
            }
            Action::Enable(id, on) => {
                let app = self.jobs.app.clone();
                match app.find_svc(&id).and_then(|s| s.set_enabled(&app, on).map(|_| s.name())) {
                    Ok(name) => self.push_log(format!("{name} {}", if on { "is included in Start all" } else { "is left out of Start all" })),
                    Err(e) => self.push_log(format!("ERROR: {e:#}")),
                }
                self.refresh();
            }
            Action::CreateDb => {
                self.dialog = Some(Dialog::Input {
                    title: "Create a database".into(),
                    prompt: "Database name".into(),
                    value: String::new(),
                    password: false,
                    then: InputThen::CreateDb,
                });
            }
            Action::ChangePw => {
                self.dialog = Some(Dialog::Input {
                    title: "MySQL root password".into(),
                    prompt: "New password (leave empty for none)".into(),
                    value: String::new(),
                    password: true,
                    then: InputThen::ChangePw,
                });
            }
            Action::KnownPw => {
                self.dialog = Some(Dialog::Input {
                    title: "Existing root password".into(),
                    prompt: "The root password your databases already use".into(),
                    value: String::new(),
                    password: true,
                    then: InputThen::KnownPw,
                });
            }
            Action::CopyPw => {
                ctx.copy_text(self.app().secrets.str("mysql_root_password"));
                self.jobs.log("MySQL root password copied to the clipboard");
            }
            Action::RecoverMysql => {
                self.dialog = Some(Dialog::Confirm {
                    title: "Recover MySQL".into(),
                    text: "Move the current MySQL data folder to backup/ and start with a fresh one? Use this when MySQL won't start.".into(),
                    yes: "Move it and start fresh".into(),
                    then: ConfirmThen::Recover,
                });
            }
            _ => {}
        }
    }

    fn input_done(&mut self, then: InputThen, value: String) {
        let value = value.trim().to_string();
        match then {
            InputThen::CreateDb if !value.is_empty() => {
                self.jobs.spawn(false, false, move |app, log| {
                    database::MySqlTools::new(app).create_database(&value)?;
                    log(&format!("Created database {value}"));
                    Ok(Outcome::Nothing)
                });
            }
            InputThen::ChangePw => {
                self.jobs.spawn(false, false, move |app, log| database::MySqlTools::new(app).change_root_password(&value, log).map(|_| Outcome::Nothing));
            }
            InputThen::KnownPw => {
                self.jobs.spawn(false, false, move |app, log| database::MySqlTools::new(app).remember_root_password(&value, log).map(|_| Outcome::Nothing));
            }
            _ => {}
        }
    }

    fn confirm_done(&mut self, then: ConfirmThen) {
        match then {
            ConfirmThen::ForceStop(sid) => self.jobs.spawn(true, false, move |app, log| app.stop(&sid, log, true).map(|_| Outcome::Nothing)),
            ConfirmThen::ForceStopAll { then_exit } => {
                self.jobs.spawn(true, false, move |app, log| Ok(Outcome::StopPending({ app.stop_all(log, true)?; vec![] }, then_exit)))
            }
            ConfirmThen::Recover => self.jobs.spawn(true, false, |app, log| Ok(Outcome::Recovered(database::MySqlTools::new(app).recover(log)?))),
            ConfirmThen::Restore(p) => self.jobs.spawn(true, false, move |app, log| database::MySqlTools::new(app).restore(&p, log).map(|_| Outcome::Nothing)),
            ConfirmThen::UseLaragon(p) => self.jobs.spawn(true, false, move |app, log| crate::laragon::use_all(app, &p, log).map(|_| Outcome::Nothing)),
        }
    }

    // ---------------------------------------------------------------- drawing
    fn sidebar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(theme::GRAPHITE).inner_margin(Margin { left: 14, right: 14, top: 20, bottom: 16 });
        egui::SidePanel::left("nav").resizable(false).exact_width(226.0).frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.add(egui::Image::new((self.logo.id(), egui::vec2(36.0, 36.0))));
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.add_space(1.0);
                    ui.label(RichText::new("devstack").font(theme::display(21.0)).color(theme::BONE));
                    ui.add_space(-7.0);
                    ui.label(RichText::new(format!("version {}", crate::VERSION)).size(11.5).color(theme::SMOKE));
                });
            });
            ui.add_space(22.0);
            for (g, group) in Page::GROUPS.iter().enumerate() {
                if g > 0 {
                    ui.add_space(10.0);
                    let r = ui.available_rect_before_wrap();
                    ui.painter().hline((r.left() + 12.0)..=(r.right() - 12.0), r.top(), Stroke::new(1.0_f32, theme::STEEL));
                    ui.add_space(11.0);
                }
                for &p in *group {
                    if theme::nav_item(ui, p.icon(), p.title(), self.page == p).clicked() {
                        self.page = p;
                    }
                    ui.add_space(2.0);
                }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                let root = self.app().paths.root.clone();
                let tail = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if theme::link(ui, format!("Folder: {tail}"), egui::FontId::proportional(12.0), theme::SMOKE)
                        .on_hover_text(root.display().to_string())
                        .clicked()
                    {
                        let _ = winutil::open_path(&root);
                    }
                });
                let running = self.rows.iter().filter(|r| r.up()).count();
                ui.horizontal(|ui| {
                    ui.add_space(2.0);
                    theme::led(ui, if running > 0 { theme::Light::On } else { theme::Light::Off });
                    ui.label(RichText::new(match running {
                        0 => "Nothing running".to_string(),
                        1 => "1 service running".to_string(),
                        n => format!("{n} services running"),
                    }).color(theme::ASH));
                });
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new()
            .fill(theme::GRAPHITE)
            .stroke(Stroke::new(1.0_f32, theme::STEEL_HI))
            .corner_radius(14)
            .inner_margin(Margin::same(24))
            .shadow(egui::Shadow { offset: [0, 18], blur: 48, spread: 0, color: Color32::from_black_alpha(220) });
        let backdrop = Color32::from_black_alpha(170);

        let mut close = false;
        let mut input_result = None;
        let mut confirmed = false;
        if let Some(d) = &mut self.dialog {
            let modal = egui::Modal::new(egui::Id::new("dialog")).frame(frame).backdrop_color(backdrop).show(ctx, |ui| {
                ui.set_width(420.0);
                let title = match d {
                    Dialog::Input { title, .. } | Dialog::Confirm { title, .. } | Dialog::Info { title, .. } => title.clone(),
                };
                ui.label(RichText::new(title).font(theme::display(20.0)).color(theme::BONE));
                ui.add_space(8.0);
                match d {
                    Dialog::Input { prompt, value, password, .. } => {
                        ui.label(theme::muted(prompt.as_str()));
                        let r = ui.add(
                            egui::TextEdit::singleline(value)
                                .password(*password)
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(10, 7)),
                        );
                        r.request_focus();
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.add_space(14.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::button(ui, Kind::Primary, None, "OK", true).clicked() || enter {
                                input_result = Some(value.clone());
                                close = true;
                            }
                            if theme::button(ui, Kind::Ghost, None, "Cancel", true).clicked() {
                                close = true;
                            }
                        });
                    }
                    Dialog::Confirm { text, yes, .. } => {
                        ui.label(theme::muted(text.as_str()));
                        ui.add_space(14.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::button(ui, Kind::Primary, None, yes, true).clicked() {
                                confirmed = true;
                                close = true;
                            }
                            if theme::button(ui, Kind::Ghost, None, "Cancel", true).clicked() {
                                close = true;
                            }
                        });
                    }
                    Dialog::Info { text, .. } => {
                        egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                            ui.label(RichText::new(text.as_str()).font(theme::mono(12.5)).color(theme::ASH));
                        });
                        ui.add_space(14.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::button(ui, Kind::Primary, None, "OK", true).clicked() {
                                close = true;
                            }
                        });
                    }
                }
            });
            if modal.should_close() {
                close = true;
            }
        }
        if close {
            match self.dialog.take() {
                Some(Dialog::Input { then, .. }) => {
                    if let Some(v) = input_result {
                        self.input_done(then, v);
                    }
                }
                Some(Dialog::Confirm { then, .. }) => {
                    if confirmed {
                        self.confirm_done(then);
                    } else if let ConfirmThen::ForceStopAll { then_exit: true } = then {
                        self.quit(ctx);
                    } else if let ConfirmThen::UseLaragon(_) = then {
                        let _ = self.app().settings.set("laragon.asked", json!(true)); // don't ask again
                    }
                }
                _ => {}
            }
        }

        let mut close_share = false;
        let mut stop_share = false;
        if let Some(s) = &self.share {
            let modal = egui::Modal::new(egui::Id::new("share")).frame(frame).backdrop_color(backdrop).show(ctx, |ui| {
                ui.label(RichText::new("Shared on the internet").font(theme::display(20.0)).color(theme::BONE));
                ui.add_space(6.0);
                if theme::link(ui, s.url.clone(), theme::strong(14.0), theme::SIGNAL).clicked() {
                    let _ = winutil::open_url(&s.url);
                }
                ui.add_space(12.0);
                ui.add(egui::Image::new((s.tex.id(), s.tex.size_vec2())).corner_radius(8));
                ui.add_space(10.0);
                ui.label(theme::muted("Scan to open it on your phone. The link works while sharing is on."));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::button(ui, Kind::Secondary, Some(icon::COPY), "Copy link", true).clicked() {
                        ui.ctx().copy_text(s.url.clone());
                    }
                    if theme::button(ui, Kind::Secondary, Some(icon::STOP), "Stop sharing", true).clicked() {
                        stop_share = true;
                    }
                    if theme::button(ui, Kind::Ghost, None, "Close", true).clicked() {
                        close_share = true;
                    }
                });
            });
            if modal.should_close() {
                close_share = true;
            }
        }
        if stop_share {
            background(&self.jobs, &Action::StopShare);
        }
        if stop_share || close_share {
            self.share = None;
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            if self.start_hidden && tray::exists() {
                self.hide(ctx);
            }
        }
        self.drain(ctx);
        if self.last_refresh.elapsed() > Duration::from_secs(2) {
            self.refresh();
            tray::refresh_menu(&self.jobs);
        }
        if self.page == Page::Sites && self.hints_at.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
            self.scan_root_hints();
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.exiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.app().settings.bool("minimize_to_tray") && tray::exists() {
                self.hide(ctx);
            } else {
                self.exit(ctx);
            }
        }

        let mut actions = vec![];
        self.sidebar(ctx);
        let frame = egui::Frame::new().fill(theme::BLACK).inner_margin(Margin { left: 34, right: 34, top: 28, bottom: 22 });
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            if self.page == Page::Logs {
                self.page_logs(ui, &mut actions); // has its own scroll area
            } else {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // keep content clear of the scrollbar
                    egui::Frame::new().inner_margin(Margin { left: 0, right: 18, top: 0, bottom: 8 }).show(ui, |ui| match self.page {
                        Page::Overview => self.page_overview(ui, &mut actions),
                        Page::Sites => self.page_sites(ui, &mut actions),
                        Page::NewProject => self.page_new_project(ui, &mut actions),
                        Page::Install => self.page_install(ui, &mut actions),
                        Page::Php => self.page_php(ui, &mut actions),
                        Page::Databases => self.page_databases(ui, &mut actions),
                        Page::Tools => self.page_tools(ui, &mut actions),
                        Page::Settings => self.page_settings(ui, &mut actions),
                        Page::Logs => {}
                    });
                });
            }
        });
        self.dialogs(ctx);
        for a in actions {
            self.dispatch(ctx, a);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

fn qr_texture(ctx: &egui::Context, url: &str) -> egui::TextureHandle {
    let (w, colors) = match qrcode::QrCode::new(url.as_bytes()) {
        Ok(code) => (code.width(), code.to_colors()),
        Err(_) => (1, vec![qrcode::Color::Light]),
    };
    let (scale, border) = (6usize, 3usize);
    let size = (w + border * 2) * scale;
    let mut pixels = vec![255u8; size * size * 4];
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = ((y + border) * scale + dy) * size + (x + border) * scale + dx;
                        pixels[px * 4..px * 4 + 3].copy_from_slice(&[0, 0, 0]);
                    }
                }
            }
        }
    }
    ctx.load_texture("share-qr", egui::ColorImage::from_rgba_unmultiplied([size, size], &pixels), egui::TextureOptions::NEAREST)
}

/// Every 2 s: heal the PHP pool. Every 10 min: scheduled backups and daily update checks.
/// Runs on its own thread so it works while the window is hidden.
fn start_ticker(jobs: Jobs) {
    std::thread::spawn(move || {
        let mut ticks: u64 = 0;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            ticks += 1;
            let app = &jobs.app;
            if let Some(_g) = app.try_lock() {
                let log = |m: &str| jobs.log(m);
                let _ = php_heal(app, &log);
            }
            if ticks % 300 == 3 {
                let log = |m: &str| jobs.log(m);
                if let Err(e) = database::auto_backup(app, &log, false) {
                    jobs.log(&format!("Scheduled backup failed: {e:#}"));
                }
                let _ = updates::daily(app, &log, false);
            }
        }
    });
}

/// Settings keys the Settings page edits.
pub(crate) const PREF_KEYS: &[&str] = &[
    "hostname_template", "www_dir", "editor", "terminal", "lan_access", "start_all_on_launch", "run_on_startup", "minimize_to_tray",
    "stop_all_on_exit", "php.children", "php.memory_limit", "php.upload_max_filesize", "php.post_max_size",
    "php.max_execution_time", "php.timezone", "php.build", "php.auto_update", "php.auto_switch", "ports",
    "backup.enabled", "backup.interval_hours", "backup.keep", "quick_app.create_database", "quick_app.open_browser", "updates.cacert",
    "laragon_dir", "laragon.versions", "laragon.data", "laragon.allow_upgrade",
];

fn get_dotted(v: &Value, key: &str) -> Value {
    key.split('.').fold(v, |cur, k| cur.get(k).unwrap_or(&Value::Null)).clone()
}

// ------------------------------------------------------------------------- entry point
pub fn run(start_hidden: bool, page: Option<String>) -> i32 {
    winutil::keep_std_handles_private();
    let app = match App::new(None) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            let _ = rfd::MessageDialog::new().set_title(APP_NAME).set_description(format!("devstack couldn't start:\n{e:#}")).show();
            return 1;
        }
    };
    if !winutil::single_instance(&app.paths.root) {
        return 0;
    }
    let icon = egui::IconData { rgba: assets::ICON_BRAND.to_vec(), width: assets::ICON_BRAND_SIZE, height: assets::ICON_BRAND_SIZE };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1140.0, 760.0])
            .with_min_inner_size([940.0, 620.0])
            .with_icon(icon),
        ..Default::default()
    };
    let result = eframe::run_native(APP_NAME, options, Box::new(move |cc| Ok(Box::new(GuiApp::new(cc, app, start_hidden, page)))));
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = rfd::MessageDialog::new().set_title(APP_NAME).set_description(format!("devstack window error: {e}")).show();
            1
        }
    }
}
