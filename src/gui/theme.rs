//! The look. A black canvas with graphite panels; Signal orange is spent only on actions and
//! selection, green only on "running". The status lights are the one bold element; everything
//! else stays quiet. Fonts and icons come from Windows itself (Bahnschrift for headings and
//! navigation, Segoe UI for text, Consolas for ports and logs, Segoe Fluent Icons); other
//! systems fall back to egui's built-in fonts and text-only buttons.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontData, FontDefinitions, FontFamily, FontId, Margin, Response,
    RichText, Sense, Shadow, Stroke, StrokeKind, TextStyle, Theme, Ui, Vec2,
};

// ------------------------------------------------------------------ palette
pub const BLACK: Color32 = Color32::from_rgb(0, 0, 0); // canvas
pub const GRAPHITE: Color32 = Color32::from_rgb(14, 14, 16); // sidebar, panels
pub const RAISED: Color32 = Color32::from_rgb(24, 24, 27); // inputs, selected rows
pub const HOVER: Color32 = Color32::from_rgb(32, 32, 36);
pub const PRESSED: Color32 = Color32::from_rgb(40, 40, 45);
pub const STEEL: Color32 = Color32::from_rgb(38, 38, 43); // hairlines
pub const STEEL_HI: Color32 = Color32::from_rgb(60, 60, 67); // strong borders
pub const BONE: Color32 = Color32::from_rgb(237, 237, 237); // text
pub const ASH: Color32 = Color32::from_rgb(161, 161, 170); // secondary text (8.9:1 on black)
pub const SMOKE: Color32 = Color32::from_rgb(130, 130, 140); // tertiary text (5.5:1 on black)
pub const SIGNAL: Color32 = Color32::from_rgb(255, 138, 61); // primary action, selection
pub const SIGNAL_HI: Color32 = Color32::from_rgb(255, 160, 99);
pub const SIGNAL_DOWN: Color32 = Color32::from_rgb(230, 118, 44);
pub const ON_SIGNAL: Color32 = Color32::from_rgb(26, 12, 2); // text on Signal (8.5:1)
pub const LED_ON: Color32 = Color32::from_rgb(74, 222, 128); // running
pub const LED_WARN: Color32 = Color32::from_rgb(250, 204, 21); // degraded
pub const LED_ERR: Color32 = Color32::from_rgb(248, 113, 113); // errors
pub const TOGGLE_ON: Color32 = Color32::from_rgb(212, 212, 216);

/// Segoe Fluent Icons / Segoe MDL2 Assets code points (same in both fonts).
pub mod icon {
    pub const HOME: char = '\u{E80F}';
    pub const GLOBE: char = '\u{E774}';
    pub const ADD: char = '\u{E710}';
    pub const DOWNLOAD: char = '\u{E896}';
    pub const CODE: char = '\u{E943}';
    pub const DATABASE: char = '\u{EE94}';
    pub const TOOLS: char = '\u{E90F}';
    pub const LIST: char = '\u{E8FD}';
    pub const SETTINGS: char = '\u{E713}';
    pub const PLAY: char = '\u{E768}';
    pub const STOP: char = '\u{E71A}';
    pub const REFRESH: char = '\u{E72C}';
    pub const OPEN: char = '\u{E8A7}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const TERMINAL: char = '\u{E756}';
    pub const MAIL: char = '\u{E715}';
    pub const SHARE: char = '\u{E72D}';
    pub const COPY: char = '\u{E8C8}';
    pub const CHECK: char = '\u{E73E}';
    pub const WARNING: char = '\u{E7BA}';
    pub const EDIT: char = '\u{E70F}';
    pub const LOCK: char = '\u{E72E}';
    pub const DIAGNOSTIC: char = '\u{E9D9}';
}

static ICONS: AtomicBool = AtomicBool::new(false);

pub fn icons_available() -> bool {
    ICONS.load(Ordering::Relaxed)
}

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("strong".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

// ------------------------------------------------------------------ setup
fn load_font(fonts: &mut FontDefinitions, dir: &Path, name: &str, files: &[&str]) -> bool {
    for f in files {
        if let Ok(bytes) = std::fs::read(dir.join(f)) {
            fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
            return true;
        }
    }
    false
}

/// Fonts, colors and spacing. Always dark, whatever the Windows theme.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = Path::new(&std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    let has_ui = load_font(&mut fonts, &dir, "ui", &["segoeui.ttf"]);
    let has_display = load_font(&mut fonts, &dir, "display", &["bahnschrift.ttf"]);
    let has_strong = load_font(&mut fonts, &dir, "strong", &["seguisb.ttf"]);
    let has_mono = load_font(&mut fonts, &dir, "mono", &["CascadiaMono.ttf", "consola.ttf"]);
    let has_icons = load_font(&mut fonts, &dir, "icons", &["SegoeIcons.ttf", "segmdl2.ttf"]);
    if has_ui {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    if has_mono {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    }
    let proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (family, present) in [("display", has_display), ("strong", has_strong), ("icons", has_icons)] {
        let mut list = if present { vec![family.to_string()] } else { vec![] };
        list.extend(proportional.iter().cloned());
        fonts.families.insert(FontFamily::Name(family.into()), list);
    }
    ICONS.store(has_icons, Ordering::Relaxed);
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(Theme::Dark, style);
}

fn style(s: &mut egui::Style) {
    use egui::style::{ScrollStyle, WidgetVisuals};
    let widget = |bg: Color32, border: Color32, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0_f32, border),
        corner_radius: CornerRadius::same(8),
        fg_stroke: Stroke::new(1.0_f32, fg),
        expansion: 0.0,
    };
    let v = &mut s.visuals;
    v.dark_mode = true;
    v.panel_fill = BLACK;
    v.window_fill = GRAPHITE;
    v.window_stroke = Stroke::new(1.0_f32, STEEL_HI);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow { offset: [0, 16], blur: 40, spread: 0, color: Color32::from_black_alpha(200) };
    v.popup_shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(180) };
    v.extreme_bg_color = RAISED; // text fields
    v.faint_bg_color = GRAPHITE;
    v.code_bg_color = RAISED;
    v.hyperlink_color = SIGNAL;
    v.warn_fg_color = LED_WARN;
    v.error_fg_color = LED_ERR;
    v.selection.bg_fill = SIGNAL.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, SIGNAL);
    v.text_cursor.stroke = Stroke::new(2.0_f32, SIGNAL);
    v.striped = false;
    v.widgets.noninteractive = widget(GRAPHITE, STEEL, ASH);
    v.widgets.inactive = widget(RAISED, STEEL, BONE);
    v.widgets.hovered = widget(HOVER, STEEL_HI, BONE);
    v.widgets.active = widget(PRESSED, SIGNAL, BONE);
    v.widgets.open = widget(RAISED, STEEL_HI, BONE);

    s.spacing.item_spacing = egui::vec2(8.0, 8.0);
    s.spacing.button_padding = egui::vec2(12.0, 6.0);
    s.spacing.interact_size = egui::vec2(36.0, 30.0);
    s.spacing.window_margin = Margin::same(22);
    s.spacing.menu_margin = Margin::same(6);
    s.spacing.combo_height = 320.0;
    s.spacing.scroll = ScrollStyle::thin();
    s.interaction.selectable_labels = false;
    s.text_styles = [
        (TextStyle::Heading, display(24.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Monospace, mono(12.5)),
    ]
    .into();
}

// ------------------------------------------------------------------ widgets
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Signal fill: the one main action of a page.
    Primary,
    /// Raised fill with a hairline border.
    Secondary,
    /// No fill until hovered.
    Ghost,
}

/// A button with an optional icon, painted by hand so every state uses the palette.
pub fn button(ui: &mut Ui, kind: Kind, glyph: Option<char>, label: &str, enabled: bool) -> Response {
    let fg = match (kind, enabled) {
        (_, false) => SMOKE,
        (Kind::Primary, true) => ON_SIGNAL,
        _ => BONE,
    };
    let icon_fg = if enabled && kind != Kind::Primary { ASH } else { fg };
    let text = (!label.is_empty()).then(|| ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(14.0), fg));
    let glyph = glyph.filter(|_| icons_available()).map(|g| ui.painter().layout_no_wrap(g.to_string(), icon_font(14.0), icon_fg));
    let pad = if text.is_some() { 14.0 } else { 9.0 };
    let gap = if text.is_some() && glyph.is_some() { 8.0 } else { 0.0 };
    let width = pad * 2.0 + gap + glyph.as_ref().map_or(0.0, |g| g.size().x) + text.as_ref().map_or(0.0, |t| t.size().x);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width.max(32.0), 32.0), sense);
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let down = enabled && resp.is_pointer_button_down_on();
        let (fill, stroke) = match kind {
            Kind::Primary if !enabled => (RAISED, Stroke::new(1.0_f32, STEEL)),
            Kind::Primary => (if down { SIGNAL_DOWN } else if hovered { SIGNAL_HI } else { SIGNAL }, Stroke::NONE),
            Kind::Secondary => (
                if down { PRESSED } else if hovered { HOVER } else { RAISED },
                Stroke::new(1.0_f32, if hovered { STEEL_HI } else { STEEL }),
            ),
            Kind::Ghost => (if down { PRESSED } else if hovered { RAISED } else { Color32::TRANSPARENT }, Stroke::NONE),
        };
        let painter = ui.painter();
        painter.rect(rect, 8, fill, stroke, StrokeKind::Inside);
        if resp.has_focus() {
            painter.rect_stroke(rect.expand(2.0), 10, Stroke::new(1.5_f32, SIGNAL), StrokeKind::Outside);
        }
        let mut x = rect.left() + pad;
        if let Some(g) = glyph {
            let w = g.size().x;
            painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, icon_fg);
            x += w + gap;
        }
        if let Some(t) = text {
            painter.galley(egui::pos2(x, rect.center().y - t.size().y / 2.0), t, fg);
        }
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// An on/off switch. Returns the response; `clicked()` means the user flipped it.
pub fn toggle(ui: &mut Ui, on: bool, enabled: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(38.0, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        // On is a light track with a black knob: clear at a glance without spending the
        // Signal color, which stays reserved for actions.
        let t = ui.ctx().animate_bool(resp.id, on);
        let track = RAISED.lerp_to_gamma(TOGGLE_ON, t);
        let border = if on { Color32::TRANSPARENT } else if enabled && resp.hovered() { STEEL_HI } else { STEEL };
        let painter = ui.painter();
        painter.rect(rect, 11, if enabled { track } else { RAISED }, Stroke::new(1.0_f32, border), StrokeKind::Inside);
        let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
        let knob = if !enabled { STEEL_HI } else if on { BLACK } else { SMOKE };
        painter.circle_filled(egui::pos2(x, rect.center().y), 7.0, knob);
        if resp.has_focus() {
            painter.rect_stroke(rect.expand(2.0), 13, Stroke::new(1.5_f32, SIGNAL), StrokeKind::Outside);
        }
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Light {
    On,
    Warn,
    Off,
}

/// A status light: lit ones glow, unlit ones are a dark lens with a rim.
pub fn led(ui: &mut Ui, light: Light) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::hover());
    let c = rect.center();
    let p = ui.painter();
    match light {
        Light::Off => {
            p.circle_filled(c, 4.2, Color32::from_rgb(26, 26, 29));
            p.circle_stroke(c, 4.2, Stroke::new(1.0_f32, STEEL_HI));
        }
        Light::On | Light::Warn => {
            let col = if light == Light::On { LED_ON } else { LED_WARN };
            for (r, a) in [(9.5, 0.07), (7.5, 0.14), (5.8, 0.26)] {
                p.circle_filled(c, r, col.gamma_multiply(a));
            }
            p.circle_filled(c, 4.2, col);
            p.circle_filled(c + egui::vec2(-1.3, -1.3), 1.3, Color32::from_white_alpha(110));
        }
    }
    resp
}

/// A small rounded label, e.g. "HTTPS" or "Active".
pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(12.0), color);
    let size = galley.size() + egui::vec2(16.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect(rect, 99, color.gamma_multiply(0.10), Stroke::new(1.0_f32, color.gamma_multiply(0.45)), StrokeKind::Inside);
    p.galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// An on/off chip (PHP extensions): on = Signal outline with a check mark.
pub fn chip(ui: &mut Ui, label: &str, on: bool, enabled: bool) -> Response {
    let font = FontId::proportional(13.0);
    let fg = if !enabled { SMOKE } else if on { BONE } else { SMOKE };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let check = (on && icons_available()).then(|| ui.painter().layout_no_wrap(icon::CHECK.to_string(), icon_font(11.0), SIGNAL));
    let extra = check.as_ref().map_or(0.0, |c| c.size().x + 6.0);
    let size = egui::vec2(galley.size().x + extra + 22.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let hovered = enabled && resp.hovered();
    // On: lit outline and a Signal check mark. Off: just an outline.
    let (fill, stroke) = if on {
        (if hovered { PRESSED } else { HOVER }, Stroke::new(1.0_f32, if hovered { ASH } else { STEEL_HI }))
    } else {
        (if hovered { RAISED } else { Color32::TRANSPARENT }, Stroke::new(1.0_f32, if hovered { STEEL_HI } else { STEEL }))
    };
    let p = ui.painter();
    p.rect(rect, 99, fill, stroke, StrokeKind::Inside);
    let mut x = rect.left() + 11.0;
    if let Some(c) = check {
        let w = c.size().x;
        p.galley(egui::pos2(x, rect.center().y - c.size().y / 2.0), c, SIGNAL);
        x += w + 6.0;
    }
    p.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    if resp.has_focus() {
        p.rect_stroke(rect.expand(2.0), 99, Stroke::new(1.5_f32, SIGNAL), StrokeKind::Outside);
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A selectable row in a side list, with an optional count on the right.
pub fn list_item(ui: &mut Ui, label: &str, detail: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 8, RAISED);
    } else if hovered {
        p.rect_filled(rect, 8, Color32::from_rgb(20, 20, 23));
    }
    let fg = if selected || hovered { BONE } else { ASH };
    p.text(egui::pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(14.0), fg);
    if !detail.is_empty() {
        p.text(egui::pos2(rect.right() - 12.0, rect.center().y), Align2::RIGHT_CENTER, detail, mono(12.0), if selected { SIGNAL } else { SMOKE });
    }
    if resp.has_focus() {
        p.rect_stroke(rect, 8, Stroke::new(1.5_f32, SIGNAL), StrokeKind::Inside);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Text that behaves like a link (Signal on hover).
pub fn link(ui: &mut Ui, text: impl Into<String>, font: FontId, color: Color32) -> Response {
    let text = text.into();
    let resp = ui.add(egui::Label::new(RichText::new(text.clone()).font(font.clone()).color(color)).sense(Sense::click()));
    if resp.hovered() {
        let r = resp.rect;
        ui.painter().hline(r.x_range(), r.bottom() - 1.0, Stroke::new(1.0_f32, SIGNAL));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// One navigation entry in the sidebar.
pub fn nav_item(ui: &mut Ui, glyph: char, label: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 9, RAISED);
    } else if hovered {
        p.rect_filled(rect, 9, Color32::from_rgb(20, 20, 23));
    }
    let fg = if selected || hovered { BONE } else { ASH };
    let mut x = rect.left() + 12.0;
    if icons_available() {
        let g = p.layout_no_wrap(glyph.to_string(), icon_font(15.0), if selected { SIGNAL } else { fg });
        p.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, fg);
        x += 28.0;
    }
    p.text(egui::pos2(x, rect.center().y), Align2::LEFT_CENTER, label, display(15.0), fg);
    if resp.has_focus() {
        p.rect_stroke(rect, 9, Stroke::new(1.5_f32, SIGNAL), StrokeKind::Inside);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A row of mutually exclusive options; returns the index clicked.
pub fn segmented(ui: &mut Ui, options: &[&str], selected: usize, enabled: bool) -> Option<usize> {
    let mut clicked = None;
    let font = FontId::proportional(13.5);
    let widths: Vec<f32> = options.iter().map(|o| ui.painter().layout_no_wrap((*o).to_owned(), font.clone(), BONE).size().x + 24.0).collect();
    let gap = 2.0;
    let inner = egui::vec2(widths.iter().sum::<f32>() + gap * (widths.len().saturating_sub(1)) as f32, 26.0);
    egui::Frame::new().fill(RAISED).stroke(Stroke::new(1.0_f32, STEEL)).corner_radius(9).inner_margin(Margin::same(3)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = gap;
        // Exactly as wide as the options, and always left to right, even inside a
        // right-aligned area (Sides lays those out right to left).
        ui.allocate_ui_with_layout(inner, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            for (i, opt) in options.iter().enumerate() {
                let size = egui::vec2(widths[i], 26.0);
                let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
                let on = i == selected;
                let p = ui.painter();
                if on {
                    p.rect_filled(rect, 7, HOVER);
                } else if enabled && resp.hovered() {
                    p.rect_filled(rect, 7, Color32::from_rgb(28, 28, 32));
                }
                let color = if on { BONE } else if enabled { ASH } else { SMOKE };
                p.text(rect.center(), Align2::CENTER_CENTER, *opt, FontId::proportional(13.5), color);
                if on {
                    p.hline((rect.center().x - 8.0)..=(rect.center().x + 8.0), rect.bottom() - 3.0, Stroke::new(2.0_f32, SIGNAL));
                }
                if enabled && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !on {
                    clicked = Some(i);
                }
            }
        });
    });
    clicked
}

/// Page title and one line of context, with actions on the right.
pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str, actions: impl FnOnce(&mut Ui)) {
    egui::Sides::new().show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).font(display(26.0)).color(BONE));
                ui.add_space(-2.0);
                ui.label(RichText::new(subtitle).color(ASH));
            });
        },
        actions,
    );
    ui.add_space(18.0);
}

/// A section heading inside a page.
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(22.0);
    ui.label(RichText::new(title).font(display(17.0)).color(BONE));
    ui.add_space(6.0);
}

/// The panel that groups rows: graphite with a hairline border.
pub fn panel() -> egui::Frame {
    egui::Frame::new().fill(GRAPHITE).stroke(Stroke::new(1.0_f32, STEEL)).corner_radius(12).inner_margin(Margin::symmetric(18, 8))
}

/// A hairline between rows inside a panel.
pub fn hairline(ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0_f32, STEEL));
    ui.add_space(1.0);
}

/// Label + description on the left, a control on the right.
pub fn setting_row(ui: &mut Ui, title: &str, desc: &str, control: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    egui::Sides::new().show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).font(strong(14.0)).color(BONE));
                if !desc.is_empty() {
                    ui.add_space(-4.0);
                    ui.label(RichText::new(desc).size(12.5).color(ASH));
                }
            });
        },
        control,
    );
    ui.add_space(10.0);
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(ASH)
}

pub fn faint(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(SMOKE)
}

/// Width-limited column used by row layouts.
pub fn cell<R>(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(Vec2::new(width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(Vec2::new(width, height));
        add(ui)
    })
    .inner
}
