//! The window's pages. Each draws from the state `GuiApp::refresh` caches and pushes `Action`s;
//! nothing here does slow work.

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
            } else {
                let current = r.active.clone().unwrap_or_default();
                egui::ComboBox::from_id_salt(("version", &r.id)).selected_text(&current).width(cols[1] - 22.0).show_ui(ui, |ui| {
                    for v in &r.versions {
                        if ui.selectable_label(*v == current, v).clicked() && *v != current {
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

fn site_row(ui: &mut Ui, s: &Site, sharing: Option<&ProcRec>, actions: &mut Vec<Action>) {
    let shared = sharing.filter(|r| r.site.as_deref() == Some(s.name.as_str()));
    // Both sides exist at once, so each collects its own clicks.
    let (mut left, mut right) = (vec![], vec![]);
    ui.add_space(10.0);
    egui::Sides::new().show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if theme::link(ui, s.host.clone(), theme::strong(15.0), BONE).on_hover_text(s.url()).clicked() {
                        left.push(Action::OpenUrl(s.url()));
                    }
                    ui.add_space(4.0);
                    if s.ssl {
                        theme::pill(ui, "HTTPS", ASH);
                    }
                    if let Some(p) = s.proxy {
                        theme::pill(ui, &format!("App server on port {p}"), SIGNAL);
                    }
                    for a in &s.aliases {
                        theme::pill(ui, a, SMOKE);
                    }
                });
                ui.label(RichText::new(fwd(&s.docroot)).font(theme::mono(12.0)).color(SMOKE));
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
            } else if theme::button(ui, Kind::Ghost, Some(icon::SHARE), "Share", true).on_hover_text("Get a public link through ngrok").clicked() {
                right.push(Action::Share(s.name.clone()));
            }
        },
    );
    actions.extend(left);
    actions.extend(right);
    ui.add_space(10.0);
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
        egui::Sides::new().show(
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
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.sites.is_empty() {
                ui.add_space(18.0);
                ui.label(RichText::new("No projects in www yet").font(theme::strong(15.0)).color(BONE));
                ui.label(theme::muted(format!("Create one with New project, or add a folder to {}.", paths.www.display())));
                ui.add_space(18.0);
            }
            for (i, s) in self.sites.iter().enumerate() {
                if i > 0 {
                    theme::hairline(ui);
                }
                site_row(ui, s, self.sharing.as_ref(), actions);
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(theme::faint("Docroot, aliases, HTTPS and app-server proxies per site are set in usr/sites.json."));
            if theme::link(ui, "Edit it", FontId::proportional(13.0), ASH).clicked() {
                actions.push(Action::Edit(paths.usr.join("sites.json")));
            }
        });
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
        theme::page_header(ui, "Install", "Versions install side by side. Installing never switches the active one.", |_| {});
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
                        egui::Sides::new().show(
                            ui,
                            |ui| {
                                ui.label(RichText::new(v).color(BONE));
                            },
                            |ui| {
                                let dir = self.app().versions_root(&selected).join(v);
                                if theme::button(ui, Kind::Ghost, Some(icon::FOLDER), "", true).on_hover_text("Open its folder").clicked() {
                                    actions.push(Action::OpenPath(dir));
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
                        egui::Sides::new().show(
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
        let row = self.rows.iter().find(|r| r.id == "php");
        let usr = self.app().paths.usr.clone();
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let desc = if php.thread_safe { "Thread-safe build: Apache loads it as a module" } else { "Runs as a pool of php-cgi processes" };
            theme::setting_row(ui, "Version", desc, |ui| {
                if let Some(r) = row {
                    let current = r.active.clone().unwrap_or_default();
                    egui::ComboBox::from_id_salt("php-version").selected_text(&current).width(240.0).show_ui(ui, |ui| {
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
                egui::ComboBox::from_id_salt("mysql-version").selected_text(&current).width(240.0).show_ui(ui, |ui| {
                    for v in &r.versions {
                        if ui.selectable_label(*v == current, v).clicked() && *v != current && !busy {
                            actions.push(Action::Version("mysql".into(), v.clone()));
                        }
                    }
                });
                theme::led(ui, if r.up() { Light::On } else { Light::Off });
            });
            for (title, desc, buttons) in [
                ("Create a database", "utf8mb4, ready for a new project", vec![(icon::ADD, "Create…", Action::CreateDb)]),
                ("Root password", "Used by phpMyAdmin and your apps", vec![(icon::COPY, "Copy", Action::CopyPw), (icon::LOCK, "Change…", Action::ChangePw)]),
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
            theme::setting_row(ui, "Other devices on your network", "Off: only this computer can open your sites", |ui| {
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
        if let Some(t) = theme::segmented(ui, &["General", "PHP", "Ports", "Backups and sharing"], prefs.tab, true) {
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
}

impl Prefs {
    pub fn load(app: &App) -> Self {
        let v = app.settings.snapshot();
        let token = app.secrets.str("ngrok_authtoken");
        Prefs { original: v.clone(), v, token_original: token.clone(), token, tab: 0, autostart_before: app.settings.bool("run_on_startup") }
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
                self.check(ui, "Other devices on your network", "Let phones and other computers open your sites", "lan_access");
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
