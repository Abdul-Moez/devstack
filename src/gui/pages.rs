//! The window's pages. Each draws from the state `GuiApp::refresh` caches and pushes `Action`s;
//! nothing here does slow work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use eframe::egui::{self, Align, Color32, FontId, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Ui};
use serde_json::{json, Value};

use super::menu::{tool_label, Action};
use super::theme::{self, icon, Kind, Light, ASH, BONE, LED_ERR, LED_ON, LED_WARN, SIGNAL, SMOKE};
use super::{get_dotted, GuiApp, Row, PREF_KEYS};
use crate::core::App;
use crate::quickapp::Recipe;
use crate::settings::ProcRec;
use crate::sites::Site;
use crate::util::{fwd, valid_name};

/// "PHP 7.4.33", "MariaDB 11.4.13", "Node.js 20.20.2" for a project's tool + version folder.
fn version_label(tool: &str, folder: &str) -> String {
    // "php-7.4.33-ts" -> 7.4.33; "node-v22" (no dots) -> 22
    let number = crate::util::parse_version(folder).unwrap_or_else(|| {
        let digits: String = folder.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
        if digits.is_empty() { folder.to_string() } else { digits }
    });
    let name = match tool {
        "php" => "PHP",
        "mysql" if folder.contains("mariadb") => "MariaDB",
        "mysql" => "MySQL",
        "postgresql" => "PostgreSQL",
        "nodejs" => "Node.js",
        "python" => "Python",
        other => other,
    };
    format!("{name} {number}")
}

fn line_color(line: &str) -> Color32 {
    if line.contains("ERROR") {
        LED_ERR
    } else if line.contains("WARNING") || line.contains("FAILED") {
        LED_WARN
    } else {
        ASH
    }
}

fn service_cols(w: f32) -> [f32; 6] {
    [0.22, 0.28, 0.13, 0.14, 0.09, 0.14].map(|f| (w * f).floor())
}

fn service_row(ui: &mut Ui, r: &Row, cols: &[f32; 6], busy: bool, apache_up: bool, actions: &mut Vec<Action>) {
    let h = 48.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        theme::cell(ui, cols[0], h, |ui| {
            let light = match r.status.as_str() {
                "running" => Light::On,
                "degraded" => Light::Warn,
                "in Apache" if apache_up => Light::On,
                _ => Light::Off,
            };
            theme::led(ui, light);
            ui.add_space(10.0);
            ui.label(RichText::new(&r.name).font(theme::strong(14.5)).color(if r.installed { BONE } else { ASH }));
        });
        theme::cell(ui, cols[1], h, |ui| {
            if r.versions.is_empty() {
                if theme::link(ui, "Install…", FontId::proportional(13.5), ASH).clicked() {
                    actions.push(Action::Page(format!("install:{}", r.tool)));
                }
            } else if r.overridden {
                // The chosen project decides; the dropdown would only change the default.
                ui.label(RichText::new(r.active.clone().unwrap_or_default()).color(BONE));
                ui.add_space(8.0);
                theme::pill(ui, "project", ASH)
                    .on_hover_text("Set by the chosen project. Change it on the Sites page, or choose Default versions.");
            } else {
                let current = r.active.clone().unwrap_or_default();
                egui::ComboBox::from_id_salt(("version", &r.id)).selected_text(&current).width(cols[1] - 22.0).truncate().show_ui(ui, |ui| {
                    for v in &r.versions {
                        let text = if r.laragon.contains(v) { format!("{v}  (Laragon)") } else { v.clone() };
                        if ui.selectable_label(*v == current, text).clicked() && *v != current {
                            actions.push(Action::Version(r.tool.clone(), v.clone()));
                        }
                    }
                });
            }
        });
        theme::cell(ui, cols[2], h, |ui| {
            ui.label(RichText::new(&r.ports).font(theme::mono(12.5)).color(ASH));
        });
        theme::cell(ui, cols[3], h, |ui| {
            let (text, color) = match r.status.as_str() {
                "running" => ("Running", LED_ON),
                "degraded" => ("Partly running", LED_WARN),
                "stopped" => ("Stopped", ASH),
                "in Apache" => ("Inside Apache", if apache_up { LED_ON } else { ASH }),
                _ => ("Not installed", SMOKE),
            };
            ui.label(RichText::new(text).color(color));
        });
        theme::cell(ui, cols[4], h, |ui| {
            if r.id == "php" {
                ui.label(theme::faint("Auto")).on_hover_text("PHP runs whenever the web server needs it");
            } else if theme::toggle(ui, r.enabled, !busy).on_hover_text("Include in Start all").clicked() {
                actions.push(Action::Enable(r.id.clone(), !r.enabled));
            }
        });
        theme::cell(ui, cols[5], h, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Nothing to start until it's installed; PHP only shows Stop while its pool runs.
                let up = r.up();
                let shown = if r.id == "php" { up } else { r.installed };
                if shown {
                    let (glyph, label) = if up { (icon::STOP, "Stop") } else { (icon::PLAY, "Start") };
                    if theme::button(ui, Kind::Secondary, Some(glyph), label, (up || r.enabled) && !busy).clicked() {
                        actions.push(Action::ToggleService(r.id.clone()));
                    }
                }
            });
        });
    });
}

/// What a site's row needs to know besides the site itself.
struct SiteState<'a> {
    sharing: Option<&'a ProcRec>,
    /// A subfolder that holds the index file when the web folder has none.
    hint: Option<&'a String>,
    chosen: bool,
    editing: bool,
    busy: bool,
}

/// The folder a site is served from, as short as it can be said: relative to the projects
/// folder ("shop/another_folder") when it is inside it.
fn served_from(s: &Site) -> String {
    let full = fwd(&s.docroot);
    let project = fwd(&s.path);
    match (full.strip_prefix(&project), project.rsplit('/').next()) {
        (Some(rest), Some(name)) => format!("www/{name}{rest}"),
        _ => full,
    }
}

/// One site. Returns true when its "Settings" button was clicked (opens/closes the editor).
fn site_row(ui: &mut Ui, s: &Site, st: &SiteState, actions: &mut Vec<Action>) -> bool {
    let shared = st.sharing.filter(|r| r.site.as_deref() == Some(s.name.as_str()));
    // Both sides exist at once, so each collects its own clicks.
    let (mut left, mut right) = (vec![], vec![]);
    let mut toggle_editor = false;
    ui.add_space(10.0);
    theme::sides(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    if theme::link(ui, s.host.clone(), theme::strong(15.0), BONE).on_hover_text(s.url()).clicked() {
                        left.push(Action::OpenUrl(s.url()));
                    }
                    ui.add_space(4.0);
                    if st.chosen {
                        theme::pill(ui, "Chosen project", LED_ON);
                    }
                    for tech in s.stack() {
                        theme::pill(ui, tech, SMOKE);
                    }
                    if s.ssl {
                        theme::pill(ui, "HTTPS", SMOKE);
                    }
                    if let Some(p) = s.proxy {
                        theme::pill(ui, &format!("App server on port {p}"), SIGNAL);
                    }
                    for a in &s.aliases {
                        theme::pill(ui, a, SMOKE);
                    }
                });
                ui.add(egui::Label::new(RichText::new(served_from(s)).font(theme::mono(12.0)).color(SMOKE)).truncate())
                    .on_hover_text(fwd(&s.docroot));
                if let Some(folder) = st.hint {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("No index file here, but there is one in {folder}.")).size(12.5).color(LED_WARN));
                        if theme::link(ui, "Serve that folder", FontId::proportional(12.5), BONE).clicked() {
                            left.push(Action::SaveProject(s.name.clone(), Some(folder.clone()), s.versions.clone()));
                        }
                    });
                }
                if !s.versions.is_empty() {
                    let uses: Vec<String> = s.versions.iter().map(|(t, v)| version_label(t, v)).collect();
                    ui.label(RichText::new(format!("Uses {}", uses.join(", "))).size(12.5).color(ASH));
                }
                if let Some(r) = shared {
                    let url = r.url.clone().unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Shared at").color(LED_ON));
                        if theme::link(ui, url.clone(), FontId::proportional(13.0), BONE).clicked() {
                            left.push(Action::OpenUrl(url.clone()));
                        }
                    });
                }
            });
        },
        |ui| {
            if theme::button(ui, Kind::Ghost, Some(icon::OPEN), "", true).on_hover_text("Open in the browser").clicked() {
                right.push(Action::OpenUrl(s.url()));
            }
            if theme::button(ui, Kind::Ghost, Some(icon::FOLDER), "", true).on_hover_text("Open the project folder").clicked() {
                right.push(Action::OpenPath(s.path.clone()));
            }
            if shared.is_some() {
                if theme::button(ui, Kind::Secondary, Some(icon::STOP), "Stop sharing", true).clicked() {
                    right.push(Action::StopShare);
                }
            } else if theme::button(ui, Kind::Ghost, Some(icon::SHARE), "", true).on_hover_text("Share: get a public link through ngrok").clicked() {
                right.push(Action::Share(s.name.clone()));
            }
            if theme::button(ui, if st.editing { Kind::Secondary } else { Kind::Ghost }, Some(icon::SETTINGS), "Settings", true)
                .on_hover_text("The web folder, and the PHP, MySQL, Node.js and Python versions this project uses")
                .clicked()
            {
                toggle_editor = true;
            }
            if st.chosen {
                if theme::button(ui, Kind::Secondary, None, "Use defaults", !st.busy).on_hover_text("Go back to the default versions").clicked() {
                    right.push(Action::Project(None));
                }
            } else if theme::button(ui, Kind::Secondary, Some(icon::PLAY), "Use this project", !st.busy)
                .on_hover_text("Switch to this project's versions (running services restart with them)")
                .clicked()
            {
                right.push(Action::Project(Some(s.name.clone())));
            }
        },
    );
    actions.extend(left);
    actions.extend(right);
    ui.add_space(10.0);
    toggle_editor
}

/// The Sites page's open editor: one project's web folder and versions, not saved yet.
pub(super) struct ProjectDraft {
    pub name: String,
    path: PathBuf,
    /// None = automatic; "." = the project folder; else a folder inside the project.
    docroot: Option<String>,
    versions: BTreeMap<String, String>,
    /// Subfolders that hold an index file, offered in the web folder list.
    candidates: Vec<String>,
    /// The project has a public folder (which is what Automatic then serves).
    has_public: bool,
    error: Option<String>,
}

impl ProjectDraft {
    pub fn new(s: &Site) -> Self {
        let mut candidates = crate::sites::docroot_candidates(&s.path, 12);
        if let Some(d) = s.docroot_rel.as_ref().filter(|d| d.as_str() != "." && !candidates.contains(*d)) {
            candidates.insert(0, d.clone());
        }
        ProjectDraft {
            name: s.name.clone(),
            path: s.path.clone(),
            docroot: s.docroot_rel.clone(),
            versions: s.versions.clone(),
            candidates,
            has_public: s.path.join("public").is_dir(),
            error: None,
        }
    }

    /// A folder picked in the folder dialog: it has to be inside the project.
    fn pick(&mut self, picked: &Path) {
        let base = self.path.to_string_lossy().into_owned();
        let full = picked.to_string_lossy().into_owned();
        // Windows paths don't care about case, and the dialog may spell the drive differently.
        let inside = full.len() >= base.len()
            && full.is_char_boundary(base.len())
            && full[..base.len()].eq_ignore_ascii_case(&base)
            && full[base.len()..].chars().next().is_none_or(|c| c == '\\' || c == '/');
        if !inside {
            self.error = Some("Choose a folder inside the project.".into());
            return;
        }
        let rel = full[base.len()..].trim_matches(['\\', '/']).replace('\\', "/");
        if rel.is_empty() {
            self.docroot = if self.has_public { Some(".".into()) } else { None };
            self.error = None;
            return;
        }
        match crate::sites::clean_docroot(&rel) {
            Some(clean) => {
                if !self.candidates.contains(&clean) {
                    self.candidates.push(clean.clone());
                }
                self.docroot = Some(clean);
                self.error = None;
            }
            None => self.error = Some("That folder's name can't be used for a web folder.".into()),
        }
    }
}

fn recipe_hint(r: &Recipe) -> String {
    match r.kind.as_str() {
        "blank" => "An empty folder with index.php".into(),
        "zip" if r.wordpress => "Latest WordPress, wp-config.php filled in".into(),
        "zip" => "Downloads and unpacks a starter".into(),
        "composer" => format!("composer create-project {}", r.package.as_deref().unwrap_or("")),
        "command" => r.command.clone().unwrap_or_default().replace("{name}", "<name>"),
        "python" => format!("Virtualenv with {}", r.pip.join(", ")),
        other => other.to_string(),
    }
}

/// Tools a recipe needs, as (tool, label).
fn recipe_needs(r: &Recipe) -> &'static [(&'static str, &'static str)] {
    match r.kind.as_str() {
        "composer" | "command" => &[("php", "PHP"), ("composer", "Composer")],
        "python" => &[("python", "Python")],
        _ => &[],
    }
}

fn recipe_tile(ui: &mut Ui, width: f32, r: &Recipe, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 56.0), Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    let fill = if selected { theme::RAISED } else if hovered { Color32::from_rgb(19, 19, 22) } else { theme::GRAPHITE };
    let stroke = Stroke::new(1.0_f32, if selected { SIGNAL } else if hovered { theme::STEEL_HI } else { theme::STEEL });
    p.rect(rect, 10, fill, stroke, StrokeKind::Inside);
    let left = rect.left() + 16.0;
    p.text(egui::pos2(left, rect.top() + 11.0), egui::Align2::LEFT_TOP, &r.name, theme::strong(14.0), BONE);
    let hint = recipe_hint(r);
    let max_chars = ((width - 60.0) / 6.6) as usize;
    let hint = if hint.chars().count() > max_chars { format!("{}…", hint.chars().take(max_chars.saturating_sub(1)).collect::<String>()) } else { hint };
    p.text(egui::pos2(left, rect.top() + 31.0), egui::Align2::LEFT_TOP, hint, FontId::proportional(12.5), SMOKE);
    let dot = egui::pos2(rect.right() - 20.0, rect.center().y);
    if selected {
        p.circle_filled(dot, 6.0, SIGNAL);
        p.circle_filled(dot, 2.5, theme::ON_SIGNAL);
    } else {
        p.circle_stroke(dot, 6.0, Stroke::new(1.0_f32, theme::STEEL_HI));
    }
    if resp.has_focus() {
        p.rect_stroke(rect.expand(2.0), 12, Stroke::new(1.5_f32, SIGNAL), StrokeKind::Outside);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl GuiApp {
    // ------------------------------------------------------------------ Overview
    pub(super) fn page_overview(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let running: Vec<&str> = self.rows.iter().filter(|r| r.up()).map(|r| r.name.as_str()).collect();
        let subtitle = if running.is_empty() { "Everything is stopped.".to_string() } else { format!("Running: {}", running.join(", ")) };
        theme::page_header(ui, "Overview", &subtitle, |ui| {
            if theme::button(ui, Kind::Primary, Some(icon::PLAY), "Start all", !busy).clicked() {
                actions.push(Action::StartAll);
            }
            if theme::button(ui, Kind::Secondary, Some(icon::STOP), "Stop all", !busy).clicked() {
                actions.push(Action::StopAll);
            }
            if theme::button(ui, Kind::Ghost, Some(icon::REFRESH), "Reload", !busy)
                .on_hover_text("Regenerate configs and restart only what changed")
                .clicked()
            {
                actions.push(Action::Reload);
            }
            if busy {
                ui.spinner();
            }
        });

        // Which versions Start all uses: a project's, or the defaults.
        theme::panel().inner_margin(Margin::symmetric(18, 12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Project").font(theme::strong(14.0)).color(BONE));
                ui.add_space(4.0);
                let current = self.project.clone().unwrap_or_else(|| "Default versions".into());
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt("project").selected_text(&current).width(230.0).truncate().show_ui(ui, |ui| {
                        if ui.selectable_label(self.project.is_none(), "Default versions").clicked() && self.project.is_some() {
                            actions.push(Action::Project(None));
                        }
                        for s in &self.sites {
                            let is = self.project.as_deref() == Some(s.name.as_str());
                            let label = if s.versions.is_empty() { format!("{} (default versions)", s.name) } else { s.name.clone() };
                            if ui.selectable_label(is, label).clicked() && !is {
                                actions.push(Action::Project(Some(s.name.clone())));
                            }
                        }
                    });
                });
                ui.add_space(6.0);
                if self.project_versions.is_empty() {
                    let text = if self.project.is_some() { "This project uses the default versions." } else { "Start all uses each tool's default version." };
                    ui.label(theme::muted(text));
                } else {
                    for (tool, v) in &self.project_versions {
                        theme::pill(ui, &version_label(tool, v), ASH);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::link(ui, "Set versions per project", FontId::proportional(13.0), ASH).clicked() {
                        actions.push(Action::Page("sites".into()));
                    }
                });
            });
        });
        ui.add_space(14.0);

        let apache_up = self.rows.iter().any(|r| r.id == "apache" && r.status == "running");
        theme::panel().show(ui, |ui| {
            let cols = service_cols(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (i, h) in ["Service", "Version", "Port", "Status", "Start all", ""].into_iter().enumerate() {
                    theme::cell(ui, cols[i], 30.0, |ui| {
                        if i == 0 {
                            ui.add_space(30.0);
                        }
                        ui.label(RichText::new(h).size(12.0).color(SMOKE));
                    });
                }
            });
            for r in &self.rows {
                theme::hairline(ui);
                service_row(ui, r, &cols, busy, apache_up, actions);
            }
        });

        theme::section(ui, "Open");
        let www = self.app().paths().www;
        let root = self.app().paths.root.clone();
        ui.horizontal_wrapped(|ui| {
            for (glyph, label, a) in [
                (icon::OPEN, "localhost", Action::OpenWeb),
                (icon::DATABASE, "Database", Action::OpenDatabase),
                (icon::TERMINAL, "Terminal", Action::Terminal),
                (icon::MAIL, "Mail inbox", Action::OpenMail),
                (icon::FOLDER, "www folder", Action::OpenPath(www)),
                (icon::FOLDER, "devstack folder", Action::OpenPath(root)),
            ] {
                if theme::button(ui, Kind::Secondary, Some(glyph), label, true).clicked() {
                    actions.push(a);
                }
            }
        });

        ui.add_space(22.0);
        theme::sides(
            ui,
            |ui| {
                ui.label(RichText::new("Activity").font(theme::display(17.0)).color(BONE));
            },
            |ui| {
                if theme::link(ui, "All logs", FontId::proportional(13.0), ASH).clicked() {
                    actions.push(Action::Page("logs".into()));
                }
            },
        );
        ui.add_space(6.0);
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_space(8.0);
            if self.log.is_empty() {
                ui.label(theme::faint("Starts, stops and installs show up here."));
            } else {
                ui.spacing_mut().item_spacing.y = 3.0;
                for line in self.log.iter().skip(self.log.len().saturating_sub(8)) {
                    ui.label(RichText::new(line).font(theme::mono(12.5)).color(line_color(line)));
                }
            }
            ui.add_space(8.0);
        });
    }

    // ------------------------------------------------------------------ Sites
    pub(super) fn page_sites(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let paths = self.app().paths();
        let scheme = if self.ssl_on { "https" } else { "http" };
        let template = self.app().settings.str("hostname_template");
        let example = if template.is_empty() { "name.test".to_string() } else { template.replace("{name}", "name") };
        theme::page_header(ui, "Sites", &format!("Every folder in www opens at {scheme}://{example}"), |ui| {
            if theme::button(ui, Kind::Primary, Some(icon::ADD), "New project", true).clicked() {
                actions.push(Action::Page("new".into()));
            }
            if theme::button(ui, Kind::Secondary, Some(icon::FOLDER), "Open www", true).clicked() {
                actions.push(Action::OpenPath(paths.www.clone()));
            }
        });
        let busy = self.busy();
        let sites = self.sites.clone();
        let mut toggled: Option<&Site> = None;
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if sites.is_empty() {
                ui.add_space(18.0);
                ui.label(RichText::new("No projects in www yet").font(theme::strong(15.0)).color(BONE));
                ui.label(theme::muted(format!("Create one with New project, or add a folder to {}.", paths.www.display())));
                ui.add_space(18.0);
            }
            for (i, s) in sites.iter().enumerate() {
                if i > 0 {
                    theme::hairline(ui);
                }
                let editing = self.editing.as_ref().is_some_and(|d| d.name == s.name);
                let state = SiteState {
                    sharing: self.sharing.as_ref(),
                    hint: self.root_hints.get(&s.name),
                    chosen: self.project.as_deref() == Some(s.name.as_str()),
                    editing,
                    busy,
                };
                if site_row(ui, s, &state, actions) {
                    toggled = Some(s);
                }
                if editing {
                    self.project_editor(ui, actions);
                    ui.add_space(12.0);
                }
            }
        });
        if let Some(s) = toggled {
            let same = self.editing.as_ref().is_some_and(|d| d.name == s.name);
            self.editing = if same { None } else { Some(ProjectDraft::new(s)) };
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(theme::faint("Aliases, another host name and HTTPS per site are set in usr/sites.json."));
            if theme::link(ui, "Edit it", FontId::proportional(13.0), ASH).clicked() {
                actions.push(Action::Edit(paths.usr.join("sites.json")));
            }
        });
    }

    /// The open project's settings: its web folder, and one version dropdown per tool
    /// ("Default" or an installed version).
    fn project_editor(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let external = self.external.clone();
        let lists: Vec<(&str, Vec<String>, Option<String>)> = crate::core::PROJECT_TOOLS
            .iter()
            .map(|t| {
                let (installed, _) = self.installed.get(*t).cloned().unwrap_or_default();
                (*t, installed, self.app().default_version(t).map(|v| v.name))
            })
            .collect();
        let Some(draft) = self.editing.as_mut() else { return };
        let (mut close, mut pick) = (false, false);
        egui::Frame::new().fill(theme::RAISED).stroke(Stroke::new(1.0_f32, theme::STEEL)).corner_radius(10).inner_margin(Margin::same(16)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("Settings for {}", draft.name)).font(theme::strong(14.5)).color(BONE));

            let auto_text = format!("Automatic ({})", if draft.has_public { "public" } else { "the project folder" });
            let current = match draft.docroot.as_deref() {
                None => auto_text.clone(),
                Some(".") => "The project folder".to_string(),
                Some(d) => d.to_string(),
            };
            let candidates = draft.candidates.clone();
            let has_public = draft.has_public;
            let docroot = &mut draft.docroot;
            theme::setting_row(ui, "Web folder", "The folder the site is served from: the one that holds index.php", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::FOLDER), "Choose…", true).clicked() {
                    pick = true;
                }
                egui::ComboBox::from_id_salt("project-docroot").selected_text(&current).width(260.0).truncate().show_ui(ui, |ui| {
                    if ui.selectable_label(docroot.is_none(), &auto_text).clicked() {
                        *docroot = None;
                    }
                    if has_public && ui.selectable_label(docroot.as_deref() == Some("."), "The project folder").clicked() {
                        *docroot = Some(".".into());
                    }
                    for c in &candidates {
                        if ui.selectable_label(docroot.as_ref() == Some(c), c).clicked() {
                            *docroot = Some(c.clone());
                        }
                    }
                });
            });
            if let Some(e) = &draft.error {
                ui.label(RichText::new(e).size(12.5).color(LED_ERR));
                ui.add_space(6.0);
            }
            theme::hairline(ui);
            ui.add_space(10.0);
            ui.label(RichText::new("Versions").font(theme::strong(14.0)).color(BONE));
            ui.add_space(-4.0);
            ui.label(RichText::new("Choosing this project switches to these. Tools left on Default use the default version.").size(12.5).color(ASH));
            ui.add_space(4.0);
            let versions = &mut draft.versions;
            for (tool, installed, default) in &lists {
                ui.add_space(4.0);
                theme::sides(
                    ui,
                    |ui| {
                        ui.label(RichText::new(tool_label(tool)).color(BONE));
                    },
                    |ui| {
                        if installed.is_empty() {
                            if theme::link(ui, "Install", FontId::proportional(13.0), ASH).clicked() {
                                actions.push(Action::Page(format!("install:{tool}")));
                            }
                            ui.label(theme::faint("None installed."));
                            return;
                        }
                        let default_text = format!("Default ({})", default.clone().unwrap_or_default());
                        let current = versions.get(*tool).cloned().unwrap_or_else(|| default_text.clone());
                        egui::ComboBox::from_id_salt(("project-version", *tool)).selected_text(&current).width(260.0).truncate().show_ui(ui, |ui| {
                            if ui.selectable_label(!versions.contains_key(*tool), &default_text).clicked() {
                                versions.remove(*tool);
                            }
                            for v in installed {
                                let text = if external.contains(v) { format!("{v}  (Laragon)") } else { v.clone() };
                                if ui.selectable_label(versions.get(*tool) == Some(v), text).clicked() {
                                    versions.insert(tool.to_string(), v.clone());
                                }
                            }
                        });
                    },
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if theme::button(ui, Kind::Primary, Some(icon::CHECK), "Save", true).clicked() {
                    actions.push(Action::SaveProject(draft.name.clone(), draft.docroot.clone(), draft.versions.clone()));
                    close = true;
                }
                if theme::button(ui, Kind::Ghost, None, "Cancel", true).clicked() {
                    close = true;
                }
            });
        });
        if pick {
            if let Some(d) = rfd::FileDialog::new().set_directory(&draft.path).pick_folder() {
                draft.pick(&d);
            }
        }
        if close {
            self.editing = None;
        }
    }

    // ------------------------------------------------------------------ New project
    pub(super) fn page_new_project(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        theme::page_header(ui, "New project", "Creates a folder in www with everything set up, then opens it in the browser", |_| {});
        let paths = self.app().paths();
        let template = self.app().settings.str("hostname_template");
        let template = if template.is_empty() { "{name}.test".to_string() } else { template };

        ui.label(RichText::new("Project name").font(theme::strong(14.0)).color(BONE));
        ui.add(
            egui::TextEdit::singleline(&mut self.new_name)
                .hint_text("my-project")
                .font(FontId::proportional(15.0))
                .desired_width(360.0)
                .margin(Margin::symmetric(12, 8)),
        );
        let name = self.new_name.trim().to_string();
        let name_ok = valid_name(&name) && !paths.www.join(&name).exists();
        let (hint, color) = if name.is_empty() {
            ("Letters, numbers, dots, dashes and underscores.".to_string(), SMOKE)
        } else if !valid_name(&name) {
            ("Use letters, numbers, dots, dashes or underscores (no spaces).".to_string(), LED_ERR)
        } else if !name_ok {
            (format!("www/{name} already exists."), LED_ERR)
        } else {
            let scheme = if self.ssl_on { "https" } else { "http" };
            (format!("Opens at {scheme}://{}", crate::sites::host_for(&name, &template)), ASH)
        };
        ui.label(RichText::new(hint).size(12.5).color(color));

        type Filter = fn(&Recipe) -> bool;
        let groups: [(&str, Filter); 2] = [("PHP and WordPress", |r| r.kind != "python"), ("Python", |r| r.kind == "python")];
        let mut pick = None;
        for (title, filter) in groups {
            let items: Vec<(usize, &Recipe)> = self.recipes.iter().enumerate().filter(|(_, r)| filter(r)).collect();
            if items.is_empty() {
                continue;
            }
            theme::section(ui, title);
            let gap = 10.0;
            let width = ((ui.available_width() - gap) / 2.0).floor();
            for pair in items.chunks(2) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (i, r) in pair {
                        if recipe_tile(ui, width, r, self.new_recipe == Some(*i)).clicked() {
                            pick = Some(*i);
                        }
                    }
                });
                ui.add_space(2.0);
            }
        }
        if pick.is_some() {
            self.new_recipe = pick;
        }

        ui.add_space(20.0);
        let recipe = self.new_recipe.and_then(|i| self.recipes.get(i));
        let missing: Vec<(&str, &str)> = recipe
            .map(|r| recipe_needs(r).iter().copied().filter(|(t, _)| self.installed.get(*t).is_none_or(|(v, _)| v.is_empty())).collect())
            .unwrap_or_default();
        let creating = self.jobs.is_pending("quickapp");
        ui.horizontal(|ui| {
            let ready = name_ok && recipe.is_some() && missing.is_empty() && !creating;
            let label = if creating { "Creating…" } else { "Create project" };
            if theme::button(ui, Kind::Primary, Some(icon::ADD), label, ready).clicked() {
                if let Some(i) = self.new_recipe {
                    actions.push(Action::CreateProject(i, name.clone()));
                }
            }
            if creating {
                ui.spinner();
            }
            if recipe.is_none() {
                ui.label(theme::faint("Pick a starting point above."));
            } else if let Some((tool, _)) = missing.first() {
                let names: Vec<&str> = missing.iter().map(|(_, l)| *l).collect();
                ui.label(RichText::new(format!("Needs {} first.", names.join(" and "))).color(LED_WARN));
                if theme::link(ui, "Install", FontId::proportional(14.0), SIGNAL).clicked() {
                    actions.push(Action::Page(format!("install:{tool}")));
                }
            }
        });
        if creating {
            ui.add_space(12.0);
            theme::panel().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.y = 3.0;
                for line in self.log.iter().skip(self.log.len().saturating_sub(10)) {
                    ui.label(RichText::new(line).font(theme::mono(12.0)).color(line_color(line)));
                }
                ui.add_space(6.0);
            });
        }
    }

    // ------------------------------------------------------------------ Install
    pub(super) fn page_install(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let checking = self.jobs.is_pending("discover");
        theme::page_header(ui, "Install", "Versions install side by side. Installing never switches the active one.", |ui| {
            let label = if checking { "Checking…" } else { "Check for new versions" };
            if theme::button(ui, Kind::Secondary, Some(icon::REFRESH), label, !checking)
                .on_hover_text("Looks for new PHP, Python, Node.js, MariaDB, PostgreSQL and MongoDB series and new Laravel versions (also done once a day)")
                .clicked()
            {
                actions.push(Action::Discover);
            }
            if checking {
                ui.spinner();
            }
        });
        let mut tools: Vec<String> = vec![];
        for p in &self.catalogue {
            if !tools.contains(&p.tool) {
                tools.push(p.tool.clone());
            }
        }
        if !tools.contains(&self.install_tool) {
            self.install_tool = tools.first().cloned().unwrap_or_default();
        }
        let selected = self.install_tool.clone();
        let busy = self.busy();
        let mut pick = None;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(200.0);
                for t in &tools {
                    let n = self.installed.get(t).map_or(0, |(v, _)| v.len());
                    let count = if n > 0 { n.to_string() } else { String::new() };
                    if theme::list_item(ui, &tool_label(t), &count, *t == selected).clicked() {
                        pick = Some(t.clone());
                    }
                }
            });
            ui.add_space(18.0);
            ui.vertical(|ui| {
                let (versions, active) = self.installed.get(&selected).cloned().unwrap_or_default();
                theme::panel().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add_space(12.0);
                    ui.label(RichText::new(tool_label(&selected)).font(theme::display(21.0)).color(BONE));
                    ui.add_space(10.0);
                    ui.label(RichText::new("Installed").font(theme::strong(13.0)).color(ASH));
                    if versions.is_empty() {
                        ui.add_space(4.0);
                        ui.label(theme::faint("Nothing installed yet."));
                    }
                    for v in &versions {
                        ui.add_space(2.0);
                        theme::sides(
                            ui,
                            |ui| {
                                ui.label(RichText::new(v).color(BONE));
                            },
                            |ui| {
                                let dir = self.app().versions_root(&selected).join(v);
                                if theme::button(ui, Kind::Ghost, Some(icon::FOLDER), "", true).on_hover_text("Open its folder").clicked() {
                                    actions.push(Action::OpenPath(dir));
                                }
                                if self.external.contains(v) {
                                    theme::pill(ui, "Laragon", SMOKE).on_hover_text("Laragon's copy, used in place");
                                }
                                if active.as_deref() == Some(v.as_str()) {
                                    theme::pill(ui, "Active", LED_ON);
                                } else if theme::button(ui, Kind::Secondary, None, "Use this one", !busy).clicked() {
                                    actions.push(Action::Version(selected.clone(), v.clone()));
                                }
                            },
                        );
                    }
                    ui.add_space(16.0);
                    theme::hairline(ui);
                    ui.add_space(10.0);
                    ui.label(RichText::new("Available").font(theme::strong(13.0)).color(ASH));
                    for p in self.catalogue.iter().filter(|p| p.tool == selected) {
                        ui.add_space(2.0);
                        theme::sides(
                            ui,
                            |ui| {
                                ui.label(RichText::new(&p.label).color(BONE));
                            },
                            |ui| {
                                if self.jobs.is_pending(&format!("quickadd:{}", p.id)) {
                                    ui.label(theme::muted("Installing…"));
                                    ui.spinner();
                                } else if theme::button(ui, Kind::Secondary, Some(icon::DOWNLOAD), "Install", true).clicked() {
                                    actions.push(Action::QuickAdd(p.id.clone()));
                                }
                            },
                        );
                    }
                    ui.add_space(12.0);
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(theme::faint("Your own entries go in usr/packages.json."));
                    if theme::link(ui, "Edit it", FontId::proportional(13.0), ASH).clicked() {
                        actions.push(Action::Edit(self.app().paths.usr.join("packages.json")));
                    }
                });
            });
        });
        if let Some(t) = pick {
            self.install_tool = t;
        }
    }

    // ------------------------------------------------------------------ PHP
    pub(super) fn page_php(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let subtitle = match &self.php {
            Some(p) => format!("Active: {}", p.version),
            None => "PHP is not installed yet".to_string(),
        };
        theme::page_header(ui, "PHP", &subtitle, |ui| {
            if theme::button(ui, Kind::Secondary, Some(icon::DOWNLOAD), "Install a version", true).clicked() {
                actions.push(Action::Page("install:php".into()));
            }
        });
        let Some(php) = &self.php else { return };
        if self.web == "apache" && !php.thread_safe {
            ui.label(
                RichText::new("With Apache, this non-thread-safe build can't serve .htaccess pretty URLs (Laravel, WordPress). Switch to Nginx in Tools, or use a thread-safe build.")
                    .size(12.5)
                    .color(LED_WARN),
            );
            ui.add_space(10.0);
        }
        let row = self.rows.iter().find(|r| r.id == "php");
        let usr = self.app().paths.usr.clone();
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let desc = if php.thread_safe { "Thread-safe build: Apache loads it as a module" } else { "Runs as a pool of php-cgi processes" };
            theme::setting_row(ui, "Version", desc, |ui| {
                if let Some(r) = row {
                    let current = r.active.clone().unwrap_or_default();
                    egui::ComboBox::from_id_salt("php-version").selected_text(&current).width(240.0).truncate().show_ui(ui, |ui| {
                        for v in &r.versions {
                            if ui.selectable_label(*v == current, v).clicked() && *v != current && !busy {
                                actions.push(Action::Version("php".into(), v.clone()));
                            }
                        }
                    });
                }
            });
            theme::hairline(ui);
            theme::setting_row(ui, "php.ini additions", "Your own settings, added to the end of the generated php.ini", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::EDIT), "Edit", true).clicked() {
                    actions.push(Action::Edit(usr.join("php.ini")));
                }
            });
            theme::hairline(ui);
            theme::setting_row(ui, "Limits and timezone", "memory_limit, upload size, max_execution_time, date.timezone", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::SETTINGS), "Open in Settings", true).clicked() {
                    actions.push(Action::Page("settings:php".into()));
                }
            });
            theme::hairline(ui);
            theme::setting_row(ui, "Updates", "Adds new patch releases of the PHP versions you have", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::REFRESH), "Check now", true).clicked() {
                    actions.push(Action::CheckUpdates);
                }
            });
        });

        theme::section(ui, "Extensions");
        ui.label(theme::muted("Click to turn one on or off. The web server restarts to apply it."));
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            if php.extensions.is_empty() {
                ui.label(theme::faint("No extensions found in this PHP's ext folder."));
            }
            for (on, name) in &php.extensions {
                if theme::chip(ui, name, *on, !busy).clicked() {
                    actions.push(Action::Ext(name.clone(), !*on));
                }
            }
        });
    }

    // ------------------------------------------------------------------ Databases
    pub(super) fn page_databases(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let paths = self.app().paths();
        theme::page_header(ui, "Databases", "MySQL or MariaDB tools and backups", |ui| {
            if theme::button(ui, Kind::Secondary, Some(icon::FOLDER), "Open backups", true).clicked() {
                actions.push(Action::OpenPath(paths.backup.clone()));
            }
        });
        let mysql = self.rows.iter().find(|r| r.id == "mysql");
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let Some(r) = mysql.filter(|r| r.installed) else {
                ui.add_space(16.0);
                ui.label(RichText::new("MySQL or MariaDB isn't installed").font(theme::strong(15.0)).color(BONE));
                ui.horizontal(|ui| {
                    ui.label(theme::muted("Add one on the Install page."));
                    if theme::link(ui, "Install", FontId::proportional(14.0), SIGNAL).clicked() {
                        actions.push(Action::Page("install:mysql".into()));
                    }
                });
                ui.add_space(16.0);
                return;
            };
            let status = if r.up() { "Running" } else { "Stopped. Start it on the Overview page to use the tools below." };
            theme::setting_row(ui, "Version", status, |ui| {
                let current = r.active.clone().unwrap_or_default();
                egui::ComboBox::from_id_salt("mysql-version").selected_text(&current).width(240.0).truncate().show_ui(ui, |ui| {
                    for v in &r.versions {
                        if ui.selectable_label(*v == current, v).clicked() && *v != current && !busy {
                            actions.push(Action::Version("mysql".into(), v.clone()));
                        }
                    }
                });
                theme::led(ui, if r.up() { Light::On } else { Light::Off });
            });
            if let Some(v) = self.app().active("mysql") {
                let dd = crate::services::mysql_data_dir(self.app(), &v);
                let laragon = crate::laragon::owns(self.app(), &dd);
                theme::hairline(ui);
                let desc = if laragon { "Laragon's folder for this version, used in place" } else { "devstack's own folder for this version" };
                theme::setting_row(ui, "Data folder", desc, |ui| {
                    if theme::button(ui, Kind::Ghost, Some(icon::FOLDER), "", true).on_hover_text("Open it").clicked() {
                        actions.push(Action::OpenPath(dd.clone()));
                    }
                    let shown = fwd(&dd);
                    ui.add(egui::Label::new(RichText::new(&shown).font(theme::mono(12.0)).color(ASH)).truncate()).on_hover_text(&shown);
                });
            }
            for (title, desc, buttons) in [
                ("Create a database", "utf8mb4, ready for a new project", vec![(icon::ADD, "Create…", Action::CreateDb)]),
                ("Root password", "Used by phpMyAdmin and your apps. If the databases already have one (set in Laragon), enter it.", vec![(icon::COPY, "Copy", Action::CopyPw), (icon::LOCK, "Change…", Action::ChangePw), (icon::EDIT, "Enter existing…", Action::KnownPw)]),
                ("Back up now", "Dumps every database into the backup folder", vec![(icon::DOWNLOAD, "Back up", Action::BackupNow)]),
                ("Browse the data", "HeidiSQL, DBeaver or phpMyAdmin, whichever is installed", vec![(icon::OPEN, "Open", Action::OpenDatabase)]),
                ("my.ini additions", "Your own [mysqld] settings", vec![(icon::EDIT, "Edit", Action::Edit(paths.usr.join("my.ini")))]),
                ("Recover", "Moves a broken data folder aside and starts fresh", vec![(icon::WARNING, "Recover…", Action::RecoverMysql)]),
            ] {
                theme::hairline(ui);
                theme::setting_row(ui, title, desc, |ui| {
                    for (glyph, label, a) in buttons.into_iter().rev() {
                        if theme::button(ui, Kind::Secondary, Some(glyph), label, true).clicked() {
                            actions.push(a);
                        }
                    }
                });
            }
        });
        theme::section(ui, "Automatic backups");
        let s = &self.app().settings;
        let text = if s.bool("backup.enabled") {
            format!("Running databases are backed up every {} hours; the last {} backups are kept.", s.u64("backup.interval_hours", 8), s.u64("backup.keep", 5))
        } else {
            "Automatic backups are off.".to_string()
        };
        ui.horizontal(|ui| {
            ui.label(theme::muted(text));
            if theme::link(ui, "Change", FontId::proportional(14.0), SIGNAL).clicked() {
                actions.push(Action::Page("settings:backups".into()));
            }
        });
    }

    // ------------------------------------------------------------------ Tools
    pub(super) fn page_tools(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let paths = self.app().paths();
        theme::page_header(ui, "Tools", "Web server, HTTPS, PATH and maintenance", |_| {});
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::setting_row(ui, "Web server", "Serves the sites in www", |ui| {
                let selected = match self.web.as_str() {
                    "apache" => 0,
                    "nginx" => 1,
                    _ => 2,
                };
                if let Some(i) = theme::segmented(ui, &["Apache", "Nginx", "None"], selected, !busy) {
                    actions.push(Action::WebServer(["apache", "nginx", ""][i].into()));
                }
            });
            theme::hairline(ui);
            theme::setting_row(ui, "HTTPS for all sites", "Each site gets a certificate from the local CA", |ui| {
                if theme::toggle(ui, self.ssl_on, !busy).clicked() {
                    actions.push(Action::Ssl(!self.ssl_on));
                }
            });
            theme::hairline(ui);
            theme::setting_row(ui, "Trust the local CA", "So browsers open https://name.test without a warning", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::LOCK), "Trust", true).clicked() {
                    actions.push(Action::TrustCa);
                }
            });
            theme::hairline(ui);
            let lan_desc = if self.lan_on {
                "On: anyone on your network can browse everything in www at this computer's IP address"
            } else {
                "Off: only this computer can open your sites"
            };
            theme::setting_row(ui, "Other devices on your network", lan_desc, |ui| {
                if theme::toggle(ui, self.lan_on, !busy).clicked() {
                    actions.push(Action::Lan(!self.lan_on));
                }
            });
        });

        theme::section(ui, "Command line");
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::setting_row(ui, "PATH", "php, composer, node, python, mysql and more in any terminal", |ui| {
                if theme::button(ui, Kind::Secondary, None, "Remove", true).clicked() {
                    actions.push(Action::PathRemove);
                }
                if theme::button(ui, Kind::Secondary, Some(icon::ADD), "Add", true).clicked() {
                    actions.push(Action::PathAdd);
                }
            });
            theme::hairline(ui);
            theme::setting_row(ui, "Terminal", "Opens in www with every tool on PATH", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::TERMINAL), "Open", true).clicked() {
                    actions.push(Action::Terminal);
                }
            });
        });

        theme::section(ui, "Maintenance");
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::setting_row(ui, "Check the setup", "Ports, runtimes, certificates and tokens", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::DIAGNOSTIC), "Run checks", true).clicked() {
                    actions.push(Action::Doctor);
                }
            });
            theme::hairline(ui);
            theme::setting_row(ui, "CA bundle", "cacert.pem, used by PHP, curl and Composer", |ui| {
                if theme::button(ui, Kind::Secondary, Some(icon::REFRESH), "Update", true).clicked() {
                    actions.push(Action::UpdateCacert);
                }
            });
            theme::hairline(ui);
            ui.add_space(10.0);
            ui.label(RichText::new("Config folders").font(theme::strong(14.0)).color(BONE));
            ui.add_space(-4.0);
            ui.label(RichText::new("Generated configs are rebuilt on every start; your own additions go in usr.").size(12.5).color(ASH));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for (label, path) in [
                    ("Generated configs", paths.etc.clone()),
                    ("Logs", paths.logs.clone()),
                    ("Apache additions", paths.usr.join("apache")),
                    ("Nginx additions", paths.usr.join("nginx")),
                ] {
                    if theme::button(ui, Kind::Secondary, Some(icon::FOLDER), label, true).clicked() {
                        actions.push(Action::OpenPath(path));
                    }
                }
            });
            ui.add_space(10.0);
        });
    }

    // ------------------------------------------------------------------ Logs
    pub(super) fn page_logs(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let logs = self.app().paths.logs.clone();
        let mut clear = false;
        let has_lines = !self.log.is_empty();
        theme::page_header(ui, "Logs", "This session's activity. Each service also writes its own log in the logs folder.", |ui| {
            if theme::button(ui, Kind::Secondary, Some(icon::FOLDER), "Logs folder", true).clicked() {
                actions.push(Action::OpenPath(logs));
            }
            if theme::button(ui, Kind::Ghost, Some(icon::COPY), "Copy", has_lines).clicked() {
                ui.ctx().copy_text(self.log.join("\n"));
            }
            if theme::button(ui, Kind::Ghost, None, "Clear", has_lines).clicked() {
                clear = true;
            }
        });
        if clear {
            self.log.clear();
        }
        theme::panel().inner_margin(Margin::same(14)).show(ui, |ui| {
            if self.log.is_empty() {
                ui.set_width(ui.available_width());
                ui.label(theme::faint("Nothing yet. Starting, stopping and installing things writes here."));
                return;
            }
            ui.spacing_mut().item_spacing.y = 2.0;
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show_rows(ui, row_h, self.log.len(), |ui, range| {
                for line in &self.log[range] {
                    ui.label(RichText::new(line).font(theme::mono(12.5)).color(line_color(line)));
                }
            });
        });
    }

    // ------------------------------------------------------------------ Settings
    pub(super) fn page_settings(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let app = self.jobs.app.clone();
        let prefs = self.prefs.get_or_insert_with(|| Prefs::load(&app));
        let dirty = prefs.dirty();
        let (mut save, mut discard) = (false, false);
        theme::page_header(ui, "Settings", "Saved in settings.json next to devstack.exe", |ui| {
            if theme::button(ui, Kind::Primary, Some(icon::CHECK), "Save changes", dirty).clicked() {
                save = true;
            }
            if theme::button(ui, Kind::Ghost, None, "Discard", dirty).clicked() {
                discard = true;
            }
        });
        if let Some(t) = theme::segmented(ui, &["General", "PHP", "Ports", "Backups and sharing", "Laragon"], prefs.tab, true) {
            prefs.tab = t;
        }
        ui.add_space(14.0);
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            prefs.form(ui);
        });
        if save {
            match prefs.save(&app) {
                Ok(()) => {
                    self.prefs = None;
                    self.jobs.log("Settings saved");
                    actions.push(Action::Reload);
                }
                Err(e) => self.push_log(format!("ERROR: {e:#}")),
            }
        } else if discard {
            self.prefs = None;
        }
    }
}

// ------------------------------------------------------------------------- settings form
pub(super) struct Prefs {
    v: Value,
    original: Value,
    token: String,
    token_original: String,
    pub tab: usize,
    autostart_before: bool,
    /// A Laragon folder found on this computer (for the Laragon tab).
    laragon_found: Option<std::path::PathBuf>,
}

impl Prefs {
    pub fn load(app: &App) -> Self {
        let v = app.settings.snapshot();
        let token = app.secrets.str("ngrok_authtoken");
        Prefs {
            original: v.clone(),
            v,
            token_original: token.clone(),
            token,
            tab: 0,
            autostart_before: app.settings.bool("run_on_startup"),
            laragon_found: crate::laragon::detect(app),
        }
    }

    fn dirty(&self) -> bool {
        PREF_KEYS.iter().any(|k| get_dotted(&self.v, k) != get_dotted(&self.original, k)) || self.token.trim() != self.token_original
    }

    fn get(&self, key: &str) -> Value {
        get_dotted(&self.v, key)
    }

    fn set(&mut self, key: &str, val: Value) {
        let parts: Vec<&str> = key.split('.').collect();
        let mut cur = &mut self.v;
        for k in &parts[..parts.len() - 1] {
            cur = &mut cur[*k];
        }
        cur[parts[parts.len() - 1]] = val;
    }

    fn text(&mut self, ui: &mut Ui, title: &str, desc: &str, key: &str) {
        let mut s = self.get(key).as_str().map(str::to_string).unwrap_or_default();
        theme::setting_row(ui, title, desc, |ui| {
            if ui.add(egui::TextEdit::singleline(&mut s).desired_width(280.0).margin(Margin::symmetric(10, 6))).changed() {
                self.set(key, json!(s));
            }
        });
    }

    fn check(&mut self, ui: &mut Ui, title: &str, desc: &str, key: &str) {
        let on = self.get(key).as_bool().unwrap_or(false);
        theme::setting_row(ui, title, desc, |ui| {
            if theme::toggle(ui, on, true).clicked() {
                self.set(key, json!(!on));
            }
        });
    }

    fn num(&mut self, ui: &mut Ui, title: &str, desc: &str, key: &str, lo: u64, hi: u64) {
        let mut n = self.get(key).as_u64().unwrap_or(lo);
        theme::setting_row(ui, title, desc, |ui| {
            if ui.add(egui::DragValue::new(&mut n).range(lo..=hi).speed(0.5)).changed() {
                self.set(key, json!(n));
            }
        });
    }

    fn choice(&mut self, ui: &mut Ui, title: &str, desc: &str, key: &str, options: &[&str]) {
        let cur = self.get(key).as_str().unwrap_or("").to_string();
        let selected = options.iter().position(|o| *o == cur).unwrap_or(0);
        theme::setting_row(ui, title, desc, |ui| {
            if let Some(i) = theme::segmented(ui, options, selected, true) {
                self.set(key, json!(options[i]));
            }
        });
    }

    fn form(&mut self, ui: &mut Ui) {
        match self.tab {
            0 => {
                self.text(ui, "Host name", "{name} is the folder name, e.g. {name}.test", "hostname_template");
                theme::hairline(ui);
                let www = self.get("www_dir").as_str().unwrap_or("").to_string();
                let mut browse = false;
                theme::setting_row(ui, "Projects folder", if www.is_empty() { "www next to devstack.exe" } else { &www }, |ui| {
                    if !www.is_empty() && theme::button(ui, Kind::Ghost, None, "Use default", true).clicked() {
                        browse = false;
                        self.set("www_dir", json!(""));
                    }
                    if theme::button(ui, Kind::Secondary, Some(icon::FOLDER), "Choose…", true).clicked() {
                        browse = true;
                    }
                });
                if browse {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.set("www_dir", json!(d.display().to_string()));
                    }
                }
                theme::hairline(ui);
                self.text(ui, "Editor", "Opens config files; empty means Notepad", "editor");
                theme::hairline(ui);
                self.choice(ui, "Terminal", "Used by the Terminal button", "terminal", &["cmd", "powershell", "pwsh", "wt"]);
                theme::hairline(ui);
                self.check(ui, "Other devices on your network", "Lets anyone on your network browse everything in www at this computer's IP address", "lan_access");
                theme::hairline(ui);
                self.check(ui, "Start everything when devstack opens", "", "start_all_on_launch");
                theme::hairline(ui);
                self.check(ui, "Run when Windows starts", "Starts hidden in the tray", "run_on_startup");
                theme::hairline(ui);
                self.check(ui, "Keep running in the tray", "Closing the window hides it instead of quitting", "minimize_to_tray");
                theme::hairline(ui);
                self.check(ui, "Stop services on exit", "", "stop_all_on_exit");
            }
            1 => {
                self.text(ui, "memory_limit", "", "php.memory_limit");
                theme::hairline(ui);
                self.text(ui, "upload_max_filesize", "", "php.upload_max_filesize");
                theme::hairline(ui);
                self.text(ui, "post_max_size", "", "php.post_max_size");
                theme::hairline(ui);
                self.num(ui, "max_execution_time", "Seconds; 0 means no limit", "php.max_execution_time", 0, 86400);
                theme::hairline(ui);
                self.text(ui, "date.timezone", "For example Asia/Karachi or Europe/London", "php.timezone");
                theme::hairline(ui);
                self.choice(ui, "Build for new installs", "ts works inside Apache; nts runs as php-cgi", "php.build", &["ts", "nts"]);
                theme::hairline(ui);
                self.num(ui, "php-cgi processes", "Used with Nginx; each handles one request at a time", "php.children", 4, 64);
                theme::hairline(ui);
                self.check(ui, "Add new patch releases", "Checked once a day", "php.auto_update");
                theme::hairline(ui);
                self.check(ui, "Switch to them too", "Otherwise the new patch waits in the Version list", "php.auto_switch");
            }
            2 => {
                let keys: Vec<String> = self.get("ports").as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        theme::hairline(ui);
                    }
                    let title = k.replace('_', " ");
                    self.num(ui, &title, "", &format!("ports.{k}"), 1, 65535);
                }
            }
            4 => self.laragon_form(ui),
            _ => {
                self.check(ui, "Back up databases automatically", "Only databases that are running", "backup.enabled");
                theme::hairline(ui);
                self.num(ui, "Every", "Hours between backups", "backup.interval_hours", 1, 168);
                theme::hairline(ui);
                self.num(ui, "Keep", "Number of backups to keep", "backup.keep", 1, 100);
                theme::hairline(ui);
                self.check(ui, "New project creates a database", "Named after the project, when MySQL is running", "quick_app.create_database");
                theme::hairline(ui);
                self.check(ui, "New project opens the browser", "", "quick_app.open_browser");
                theme::hairline(ui);
                self.check(ui, "Keep cacert.pem up to date", "The CA bundle PHP and Composer use", "updates.cacert");
                theme::hairline(ui);
                let mut token = self.token.clone();
                theme::setting_row(ui, "ngrok authtoken", "For Share; free at dashboard.ngrok.com", |ui| {
                    if ui.add(egui::TextEdit::singleline(&mut token).password(true).desired_width(280.0).margin(Margin::symmetric(10, 6))).changed() {
                        self.token = token.clone();
                    }
                });
            }
        }
    }

    fn laragon_form(&mut self, ui: &mut Ui) {
        let dir = self.get("laragon_dir").as_str().unwrap_or("").to_string();
        let set = !dir.is_empty() && crate::laragon::looks_like_laragon(std::path::Path::new(&dir));
        let mut pick = false;
        let found = self.laragon_found.clone().filter(|f| f.display().to_string() != dir);
        let desc = if set {
            dir.clone()
        } else if dir.is_empty() {
            "Not set".to_string()
        } else {
            format!("{dir} (no laragon.exe there)")
        };
        theme::setting_row(ui, "Laragon folder", &desc, |ui| {
            if theme::button(ui, Kind::Secondary, Some(icon::FOLDER), "Choose…", true).clicked() {
                pick = true;
            }
            if let Some(f) = &found {
                if theme::button(ui, Kind::Secondary, None, &format!("Use {}", f.display()), true).clicked() {
                    self.set("laragon_dir", json!(f.display().to_string()));
                }
            }
        });
        if pick {
            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                self.set("laragon_dir", json!(d.display().to_string()));
            }
        }
        let www = std::path::Path::new(&dir).join("www").display().to_string();
        let uses_www = set && self.get("www_dir").as_str() == Some(www.as_str());
        theme::hairline(ui);
        let www_desc = format!("Projects folder: {}", if set { www.as_str() } else { "Laragon's www" });
        theme::setting_row(ui, "Use its projects", &www_desc, |ui| {
            if theme::toggle(ui, uses_www, set).clicked() {
                self.set("www_dir", json!(if uses_www { String::new() } else { www.clone() }));
            }
        });
        for (title, desc, key) in [
            (
                "Use its installed versions",
                "Laragon's PHP, MySQL/MariaDB, Node.js and Python appear next to devstack's, used in place. devstack keeps its own settings for them.",
                "laragon.versions",
            ),
            (
                "Use its databases",
                "Each MySQL/MariaDB version opens Laragon's data folder for that version (mysql-8 holds 8.0). Versions Laragon has no folder for get one in devstack's own data folder.",
                "laragon.data",
            ),
            (
                "Let other versions upgrade its databases",
                "Off: only Laragon's own MySQL/MariaDB builds open Laragon's databases. A newer build upgrades the files, and Laragon's older build may then refuse to open them.",
                "laragon.allow_upgrade",
            ),
        ] {
            theme::hairline(ui);
            let on = self.get(key).as_bool().unwrap_or(false);
            theme::setting_row(ui, title, desc, |ui| {
                if theme::toggle(ui, on, set).clicked() {
                    self.set(key, json!(!on));
                }
            });
        }
        ui.add_space(6.0);
        ui.label(RichText::new("Don't run Laragon and devstack at the same time: they share ports and database folders.").size(12.5).color(LED_WARN));
        ui.add_space(8.0);
    }

    fn save(&self, app: &App) -> Result<()> {
        app.settings.refresh(); // keep settings changed elsewhere that aren't on this page
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
