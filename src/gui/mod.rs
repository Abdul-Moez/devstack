//! Tray icon + main window (eframe/egui). All real work happens in `core::App`; this module
//! wires buttons and menus to it and runs slow work on background threads.

pub mod menu;
mod tray;

use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use serde_json::{json, Value};

use crate::core::App;
use crate::error::needs_force;
use crate::services::php_heal;
use crate::{assets, database, packages, quickapp, share, ssl, updates, winutil, Log, APP_NAME};
use menu::{egui_menu, model, Action};

const GREEN: Color32 = Color32::from_rgb(47, 158, 87);
const GREY: Color32 = Color32::from_rgb(141, 148, 160);
const AMBER: Color32 = Color32::from_rgb(208, 138, 16);

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
}

/// Shared by the window, the tray handlers and the ticker thread.
#[derive(Clone)]
pub struct Jobs {
    pub app: Arc<App>,
    tx: Sender<Msg>,
    pub ctx: egui::Context,
    busy: Arc<AtomicUsize>,
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
        Action::Version(tool, name) => jobs.spawn(true, false, move |app, log| app.switch_version(&tool, &name, log).map(|_| Outcome::Nothing)),
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
        Action::QuickAdd(id) => jobs.spawn(false, false, move |app, log| {
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
    anyhow::bail!("Add HeidiSQL, DBeaver or phpMyAdmin from Menu > Tools > Quick add.")
}

/// Called by the tray menu on the UI thread, even while the window is hidden.
pub(crate) fn tray_action(jobs: &Jobs, action: Action) {
    if !background(jobs, &action) {
        jobs.show();
        let _ = jobs.tx.send(Msg::Action(action));
        jobs.ctx.request_repaint();
    }
}

// ------------------------------------------------------------------------- window state
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
}

enum InputThen {
    QuickApp(usize),
    CreateDb,
    ChangePw,
}

enum ConfirmThen {
    ForceStop(String),
    ForceStopAll { then_exit: bool },
    Recover,
    Restore(PathBuf),
}

enum Dialog {
    Input { title: String, prompt: String, value: String, password: bool, then: InputThen },
    Confirm { title: String, text: String, then: ConfirmThen },
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
    rows: Vec<Row>,
    ssl_on: bool,
    last_refresh: Instant,
    start_hidden: bool,
    first_frame: bool,
    exiting: bool,
    tray_hint_shown: bool,
    dialog: Option<Dialog>,
    share: Option<ShareView>,
    prefs: Option<Prefs>,
}

impl GuiApp {
    fn new(cc: &eframe::CreationContext<'_>, app: Arc<App>, start_hidden: bool) -> Self {
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
        let jobs = Jobs { app, tx, ctx: cc.egui_ctx.clone(), busy: Arc::new(AtomicUsize::new(0)), hwnd };
        if cfg!(windows) {
            if let Err(e) = tray::create(jobs.clone()) {
                jobs.log(&format!("Couldn't create the tray icon: {e:#}"));
            }
        }
        start_ticker(jobs.clone());
        if jobs.app.settings.bool("start_all_on_launch") {
            background(&jobs, &Action::StartAll);
        }
        let mut me = GuiApp {
            jobs,
            rx,
            log: vec![],
            rows: vec![],
            ssl_on: false,
            last_refresh: Instant::now(), // refresh() below runs right away
            start_hidden,
            first_frame: true,
            exiting: false,
            tray_hint_shown: false,
            dialog: None,
            share: None,
            prefs: None,
        };
        me.refresh();
        me
    }

    fn app(&self) -> &App {
        &self.jobs.app
    }

    fn busy(&self) -> bool {
        self.jobs.busy.load(Ordering::SeqCst) > 0
    }

    fn push_log(&mut self, m: String) {
        for line in m.lines() {
            self.log.push(line.to_string());
        }
        let n = self.log.len();
        if n > 3000 {
            self.log.drain(..n - 3000);
        }
    }

    fn refresh(&mut self) {
        let app = self.jobs.app.clone();
        // Pick up changes made with `ds` (or by hand) while the window is open.
        app.settings.refresh();
        app.secrets.refresh();
        let mut any_up = false;
        self.rows = app
            .services()
            .iter()
            .map(|s| {
                let status = s.status(&app);
                let up = status == "running" || status == "degraded";
                any_up |= up;
                let installed = s.installed(&app);
                let ports = if installed && status != "in Apache" { s.ports(&app) } else { vec![] };
                let ports = match ports.len() {
                    0 => String::new(),
                    1 => ports[0].to_string(),
                    _ if s.id() == "php" => format!("{}–{}", ports[0], ports[ports.len() - 1]),
                    _ => ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", "),
                };
                Row {
                    id: s.id().into(),
                    name: s.name().into(),
                    tool: s.tool().into(),
                    status,
                    ports,
                    enabled: s.enabled(&app),
                    installed,
                    versions: app.versions(s.tool()).into_iter().map(|v| v.name).collect(),
                    active: app.active(s.tool()).map(|v| v.name),
                }
            })
            .collect();
        self.ssl_on = app.settings.bool("ssl");
        self.last_refresh = Instant::now();
        tray::set_running(any_up);
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

    fn quit(&mut self, ctx: &egui::Context) {
        self.exiting = true;
        tray::remove();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                Msg::Done { quiet, result } => {
                    self.refresh();
                    match result {
                        Ok(o) => self.outcome(ctx, o),
                        Err((msg, force)) => {
                            if let Some(sid) = force {
                                self.dialog = Some(Dialog::Confirm { title: APP_NAME.into(), text: msg, then: ConfirmThen::ForceStop(sid) });
                            } else {
                                self.push_log(format!("ERROR: {msg}"));
                                if !quiet {
                                    self.dialog = Some(Dialog::Info { title: "Something went wrong".into(), text: msg });
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
            Outcome::Nothing => {}
            Outcome::StartFailed(f) if !f.is_empty() => {
                self.dialog = Some(Dialog::Info { title: "Some services didn't start".into(),
                                                  text: format!("Failed: {}. See the log below for details.", f.join(", ")) });
            }
            Outcome::StartFailed(_) => {}
            Outcome::StopPending(p, then_exit) => {
                if p.is_empty() {
                    if then_exit {
                        self.quit(ctx);
                    }
                } else {
                    self.dialog = Some(Dialog::Confirm {
                        title: APP_NAME.into(),
                        text: "Some databases didn't shut down cleanly. Force them to stop?".into(),
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
                let text = results.iter().map(|(ok, m)| format!("{}  {m}", if *ok { "OK " } else { "!! " })).collect::<Vec<_>>().join("\n");
                self.dialog = Some(Dialog::Info { title: "Doctor".into(), text });
            }
            Outcome::Recovered(Some(newest)) => {
                self.dialog = Some(Dialog::Confirm {
                    title: APP_NAME.into(),
                    text: format!("Restore the backup from {}?", newest.file_name().unwrap_or_default().to_string_lossy()),
                    then: ConfirmThen::Restore(newest),
                });
            }
            Outcome::Recovered(None) => {}
        }
    }

    fn dispatch(&mut self, ctx: &egui::Context, action: Action) {
        if background(&self.jobs, &action) {
            return;
        }
        match action {
            Action::Exit => self.exit(ctx),
            Action::QuickApp(i) => {
                let name = quickapp::recipes(self.app()).get(i).map(|r| r.name.clone()).unwrap_or_default();
                self.dialog = Some(Dialog::Input { title: "Quick app".into(), prompt: format!("Name for the new {name} project:"),
                                                   value: String::new(), password: false, then: InputThen::QuickApp(i) });
            }
            Action::CreateDb => {
                self.dialog = Some(Dialog::Input { title: "Create database".into(), prompt: "Database name:".into(),
                                                   value: String::new(), password: false, then: InputThen::CreateDb });
            }
            Action::ChangePw => {
                self.dialog = Some(Dialog::Input { title: "MySQL root password".into(), prompt: "New password (leave empty for none):".into(),
                                                   value: String::new(), password: true, then: InputThen::ChangePw });
            }
            Action::CopyPw => {
                ctx.copy_text(self.app().secrets.str("mysql_root_password"));
                self.jobs.log("MySQL root password copied to the clipboard");
            }
            Action::RecoverMysql => {
                self.dialog = Some(Dialog::Confirm {
                    title: "Recover MySQL".into(),
                    text: "Move the current MySQL data folder to backup/ and start with a fresh one? Use this when MySQL won't start.".into(),
                    then: ConfirmThen::Recover,
                });
            }
            Action::Preferences => self.prefs = Some(Prefs::load(self.app())),
            _ => {}
        }
    }

    fn input_done(&mut self, then: InputThen, value: String) {
        let value = value.trim().to_string();
        match then {
            InputThen::QuickApp(i) if !value.is_empty() => {
                let Some(recipe) = quickapp::recipes(self.app()).get(i).cloned() else { return };
                self.jobs.spawn(false, false, move |app, log| Ok(Outcome::OpenUrl(quickapp::create(app, &recipe, &value, log)?)));
            }
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
        }
    }

    // ---------------------------------------------------------------- drawing
    fn ui_top(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(APP_NAME).strong().size(16.0));
                ui.label(RichText::new(self.app().paths.root.display().to_string()).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("Menu", |ui| {
                        let nodes = model(self.app());
                        egui_menu(ui, &nodes, actions);
                    });
                    let enabled = !self.busy();
                    if ui.add_enabled(enabled, egui::Button::new("Reload")).clicked() {
                        actions.push(Action::Reload);
                    }
                    if ui.add_enabled(enabled, egui::Button::new("Stop All")).clicked() {
                        actions.push(Action::StopAll);
                    }
                    if ui.add_enabled(enabled, egui::Button::new(RichText::new("Start All").strong())).clicked() {
                        actions.push(Action::StartAll);
                    }
                    if self.busy() {
                        ui.spinner();
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn ui_services(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let apache_up = self.rows.iter().any(|r| r.id == "apache" && r.status == "running");
        let mut toggles: Vec<(String, bool)> = vec![];
        egui::Grid::new("services").num_columns(5).spacing([14.0, 8.0]).striped(true).show(ui, |ui| {
            for h in ["Service", "Version", "Port", "Status", ""] {
                ui.label(RichText::new(h).weak());
            }
            ui.end_row();
            for r in &self.rows {
                if r.id == "php" {
                    ui.label(&r.name).on_hover_text("Runs automatically when the web server needs it");
                } else {
                    let mut on = r.enabled;
                    if ui.checkbox(&mut on, &r.name).changed() {
                        toggles.push((r.id.clone(), on));
                    }
                }
                if r.versions.is_empty() {
                    ui.add_enabled(false, egui::Label::new("not installed: Menu > Tools > Quick add"));
                } else {
                    let current = r.active.clone().unwrap_or_default();
                    egui::ComboBox::from_id_salt(format!("ver-{}", r.id)).selected_text(&current).width(280.0).show_ui(ui, |ui| {
                        for v in &r.versions {
                            if ui.selectable_label(*v == current, v).clicked() && *v != current {
                                actions.push(Action::Version(r.tool.clone(), v.clone()));
                            }
                        }
                    });
                }
                ui.label(&r.ports);
                let color = match r.status.as_str() {
                    "running" => GREEN,
                    "degraded" => AMBER,
                    "in Apache" if apache_up => GREEN,
                    _ => GREY,
                };
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.5, color);
                    ui.label(RichText::new(&r.status).color(color));
                });
                let up = r.status == "running" || r.status == "degraded";
                let can = r.installed && (up || r.enabled) && r.id != "php" || (r.id == "php" && up);
                if ui.add_enabled(can && !self.busy(), egui::Button::new(if up { "Stop" } else { "Start" }).min_size(egui::vec2(64.0, 0.0))).clicked() {
                    actions.push(Action::ToggleService(r.id.clone()));
                }
                ui.end_row();
            }
        });
        for (id, on) in toggles {
            let app = self.jobs.app.clone();
            if let Err(e) = app.svc(&id).set_enabled(&app, on) {
                self.push_log(format!("ERROR: {e:#}"));
            }
            self.push_log(format!("{} {}", app.svc(&id).name(), if on { "enabled" } else { "disabled" }));
            self.refresh();
        }
    }

    fn ui_dialogs(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut input_result = None;
        let mut confirm_result = None;
        if let Some(d) = &mut self.dialog {
            let title = match d {
                Dialog::Input { title, .. } | Dialog::Confirm { title, .. } | Dialog::Info { title, .. } => title.clone(),
            };
            egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.set_min_width(360.0);
                match d {
                    Dialog::Input { prompt, value, password, .. } => {
                        ui.label(prompt.as_str());
                        let r = ui.add(egui::TextEdit::singleline(value).password(*password).desired_width(f32::INFINITY));
                        r.request_focus();
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() || enter {
                                input_result = Some(value.clone());
                                close = true;
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    }
                    Dialog::Confirm { text, .. } => {
                        ui.label(text.as_str());
                        ui.horizontal(|ui| {
                            if ui.button("Yes").clicked() {
                                confirm_result = Some(());
                                close = true;
                            }
                            if ui.button("No").clicked() {
                                close = true;
                            }
                        });
                    }
                    Dialog::Info { text, .. } => {
                        egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                            ui.label(RichText::new(text.as_str()).monospace());
                        });
                        if ui.button("OK").clicked() {
                            close = true;
                        }
                    }
                }
            });
        }
        if close {
            match self.dialog.take() {
                Some(Dialog::Input { then, .. }) => {
                    if let Some(v) = input_result {
                        self.input_done(then, v);
                    }
                }
                Some(Dialog::Confirm { then, .. }) => {
                    if confirm_result.is_some() {
                        self.confirm_done(then);
                    } else if let ConfirmThen::ForceStopAll { then_exit: true } = then {
                        self.quit(ctx);
                    }
                }
                _ => {}
            }
        }

        let mut close_share = false;
        let mut stop_share = false;
        if let Some(s) = &self.share {
            egui::Window::new("Share").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.hyperlink(&s.url);
                ui.add(egui::Image::new((s.tex.id(), s.tex.size_vec2())));
                ui.label("Scan to open on your phone. The link works while sharing is on.");
                ui.horizontal(|ui| {
                    if ui.button("Copy link").clicked() {
                        ui.ctx().copy_text(s.url.clone());
                    }
                    if ui.button("Stop sharing").clicked() {
                        stop_share = true;
                    }
                    if ui.button("Close").clicked() {
                        close_share = true;
                    }
                });
            });
        }
        if stop_share {
            background(&self.jobs, &Action::StopShare);
        }
        if stop_share || close_share {
            self.share = None;
        }

        let mut prefs_done = None;
        if let Some(p) = &mut self.prefs {
            prefs_done = p.ui(ctx);
        }
        if let Some(save) = prefs_done {
            let p = self.prefs.take().unwrap();
            if save {
                match p.save(self.app()) {
                    Ok(()) => {
                        self.jobs.log("Preferences saved");
                        background(&self.jobs, &Action::Reload);
                    }
                    Err(e) => self.push_log(format!("ERROR: {e:#}")),
                }
            }
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
        if ctx.input(|i| i.viewport().close_requested()) && !self.exiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.app().settings.bool("minimize_to_tray") && tray::exists() {
                self.hide(ctx);
            } else {
                self.exit(ctx);
            }
        }

        let mut actions = vec![];
        self.ui_top(ctx, &mut actions);
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_services(ui, &mut actions);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                for (label, a) in [("Web", Action::OpenWeb), ("Database", Action::OpenDatabase), ("Terminal", Action::Terminal),
                                   ("www", Action::OpenPath(self.app().paths().www)), ("Root", Action::OpenPath(self.app().paths.root.clone())),
                                   ("Mail", Action::OpenMail)] {
                    if ui.button(label).clicked() {
                        actions.push(a);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut on = self.ssl_on;
                    if ui.checkbox(&mut on, "HTTPS").on_hover_text("Serve every site over https:// too").changed() {
                        actions.push(Action::Ssl(on));
                    }
                });
            });
            ui.add_space(8.0);
            egui::Frame::default().fill(ui.visuals().extreme_bg_color).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                ui.set_min_height(ui.available_height());
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    for line in &self.log {
                        ui.label(RichText::new(line).monospace().size(12.5));
                    }
                });
            });
        });
        self.ui_dialogs(ctx);
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
    let (scale, border) = (6usize, 2usize);
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

// ------------------------------------------------------------------------- preferences
const PREF_KEYS: &[&str] = &[
    "hostname_template", "www_dir", "editor", "terminal", "lan_access", "start_all_on_launch", "run_on_startup", "minimize_to_tray",
    "stop_all_on_exit", "php.children", "php.memory_limit", "php.upload_max_filesize", "php.post_max_size",
    "php.max_execution_time", "php.timezone", "php.build", "php.auto_update", "php.auto_switch", "ports",
    "backup.enabled", "backup.interval_hours", "backup.keep", "quick_app.create_database", "quick_app.open_browser", "updates.cacert",
];

struct Prefs {
    v: Value,
    token: String,
    tab: usize,
    autostart_before: bool,
}

impl Prefs {
    fn load(app: &App) -> Self {
        Prefs { v: app.settings.snapshot(), token: app.secrets.str("ngrok_authtoken"), tab: 0, autostart_before: app.settings.bool("run_on_startup") }
    }

    fn get(&self, key: &str) -> Value {
        key.split('.').fold(&self.v, |cur, k| cur.get(k).unwrap_or(&Value::Null)).clone()
    }

    fn set(&mut self, key: &str, val: Value) {
        let parts: Vec<&str> = key.split('.').collect();
        let mut cur = &mut self.v;
        for k in &parts[..parts.len() - 1] {
            cur = &mut cur[*k];
        }
        cur[parts[parts.len() - 1]] = val;
    }

    fn text(&mut self, ui: &mut egui::Ui, label: &str, key: &str) {
        let mut s = self.get(key).as_str().map(str::to_string).unwrap_or_default();
        ui.label(label);
        if ui.add(egui::TextEdit::singleline(&mut s).desired_width(260.0)).changed() {
            self.set(key, json!(s));
        }
        ui.end_row();
    }

    fn check(&mut self, ui: &mut egui::Ui, label: &str, key: &str) {
        let mut b = self.get(key).as_bool().unwrap_or(false);
        ui.label("");
        if ui.checkbox(&mut b, label).changed() {
            self.set(key, json!(b));
        }
        ui.end_row();
    }

    fn num(&mut self, ui: &mut egui::Ui, label: &str, key: &str, lo: u64, hi: u64) {
        let mut n = self.get(key).as_u64().unwrap_or(lo);
        ui.label(label);
        if ui.add(egui::DragValue::new(&mut n).range(lo..=hi)).changed() {
            self.set(key, json!(n));
        }
        ui.end_row();
    }

    fn choice(&mut self, ui: &mut egui::Ui, label: &str, key: &str, options: &[&str]) {
        let cur = self.get(key).as_str().unwrap_or("").to_string();
        ui.label(label);
        egui::ComboBox::from_id_salt(key).selected_text(&cur).show_ui(ui, |ui| {
            for o in options {
                if ui.selectable_label(cur == *o, *o).clicked() {
                    self.set(key, json!(o));
                }
            }
        });
        ui.end_row();
    }

    /// Some(true) = save, Some(false) = cancel, None = still open.
    fn ui(&mut self, ctx: &egui::Context) -> Option<bool> {
        let mut result = None;
        egui::Window::new("Preferences").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (i, t) in ["General", "PHP", "Ports", "Data & sharing"].iter().enumerate() {
                    ui.selectable_value(&mut self.tab, i, *t);
                }
            });
            ui.separator();
            egui::Grid::new("prefs").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| match self.tab {
                0 => {
                    self.text(ui, "Host name template", "hostname_template");
                    self.text(ui, "Projects folder (empty = www)", "www_dir");
                    ui.label("");
                    if ui.button("Browse…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            self.set("www_dir", json!(d.display().to_string()));
                        }
                    }
                    ui.end_row();
                    self.text(ui, "Editor (empty = Notepad)", "editor");
                    self.choice(ui, "Terminal", "terminal", &["cmd", "powershell", "pwsh", "wt"]);
                    self.check(ui, "Let other devices on my network open my sites", "lan_access");
                    self.check(ui, "Start all services when devstack opens", "start_all_on_launch");
                    self.check(ui, "Run devstack when Windows starts", "run_on_startup");
                    self.check(ui, "Closing the window keeps devstack in the tray", "minimize_to_tray");
                    self.check(ui, "Stop all services when exiting", "stop_all_on_exit");
                }
                1 => {
                    self.num(ui, "PHP processes in pool (Nginx)", "php.children", 4, 64);
                    self.text(ui, "memory_limit", "php.memory_limit");
                    self.text(ui, "upload_max_filesize", "php.upload_max_filesize");
                    self.text(ui, "post_max_size", "php.post_max_size");
                    self.num(ui, "max_execution_time (s)", "php.max_execution_time", 0, 86400);
                    self.text(ui, "date.timezone", "php.timezone");
                    self.choice(ui, "Quick add build", "php.build", &["ts", "nts"]);
                    self.check(ui, "Automatically add new PHP patch releases", "php.auto_update");
                    self.check(ui, "…and switch to them", "php.auto_switch");
                }
                2 => {
                    let keys: Vec<String> = self.get("ports").as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                    for k in keys {
                        self.num(ui, &k.replace('_', " "), &format!("ports.{k}"), 1, 65535);
                    }
                }
                _ => {
                    self.check(ui, "Back up databases automatically", "backup.enabled");
                    self.num(ui, "Every (hours)", "backup.interval_hours", 1, 168);
                    self.num(ui, "Keep last", "backup.keep", 1, 100);
                    self.check(ui, "Quick app creates a database", "quick_app.create_database");
                    self.check(ui, "Quick app opens the browser when done", "quick_app.open_browser");
                    self.check(ui, "Keep cacert.pem up to date", "updates.cacert");
                    ui.label("ngrok authtoken");
                    ui.add(egui::TextEdit::singleline(&mut self.token).password(true).desired_width(260.0));
                    ui.end_row();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
            });
        });
        result
    }

    fn save(&self, app: &App) -> Result<()> {
        app.settings.refresh(); // keep settings changed elsewhere that aren't on this dialog
        for k in PREF_KEYS {
            app.settings.put(k, self.get(k));
        }
        app.settings.save()?;
        app.secrets.set("ngrok_authtoken", json!(self.token.trim()))?;
        app.paths().ensure()?;
        let on = app.settings.bool("run_on_startup");
        if on != self.autostart_before {
            app.set_autostart(on)?;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------------- entry point
pub fn run(start_hidden: bool) -> i32 {
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
    let icon = egui::IconData { rgba: assets::ICON_GREEN.to_vec(), width: assets::ICON_SIZE, height: assets::ICON_SIZE };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([700.0, 480.0])
            .with_icon(icon),
        ..Default::default()
    };
    let result = eframe::run_native(APP_NAME, options, Box::new(move |cc| Ok(Box::new(GuiApp::new(cc, app, start_hidden)))));
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = rfd::MessageDialog::new().set_title(APP_NAME).set_description(format!("devstack window error: {e}")).show();
            1
        }
    }
}
