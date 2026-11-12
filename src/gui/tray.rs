//! The tray icon. Created only on Windows (on Linux the tray needs a GTK loop; the window
//! works without it). The menu is built when it is asked for (right click), so it always
//! lists current sites and versions, and actions run even while the window is in the tray.
//!
//! The menu is a plain Win32 popup (`winutil::popup_menu`), not a tray-icon/muda menu: muda
//! numbers every item it ever creates from one counter, and a click comes back through
//! WM_COMMAND, which carries 16 bits. Rebuilt every 2 s, the ids passed 65535 after six
//! minutes and from then on every click in the tray menu was dropped silently.

use std::cell::RefCell;

use anyhow::Result;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::menu::{model, Action, Node};
use super::Jobs;
use crate::assets;
use crate::winutil::{self, PopupItem};

struct Tray {
    icon: TrayIcon,
    running: Option<bool>,
    /// Exit is stopping the services: the menu and tooltip only say so.
    exiting: bool,
}

const EXITING: &str = "Stopping services, then closing…";

thread_local! {
    // Tray objects aren't Send; they live on the UI thread, which is also where tray events
    // are delivered on Windows.
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

fn icon(running: bool) -> Option<Icon> {
    let rgba = if running { assets::ICON_RUNNING } else { assets::ICON_STOPPED };
    Icon::from_rgba(rgba.to_vec(), assets::TRAY_ICON_SIZE, assets::TRAY_ICON_SIZE).ok()
}

/// The menu model as popup items. An item's id is its action's position in `actions` + 1.
fn popup_items(nodes: &[Node], actions: &mut Vec<Action>) -> Vec<PopupItem> {
    fn entry(label: &str, checked: bool, a: &Action, actions: &mut Vec<Action>) -> PopupItem {
        actions.push(a.clone());
        PopupItem::Item { label: label.to_string(), id: actions.len() as u32, checked, enabled: true }
    }
    let mut items = vec![];
    for n in nodes {
        items.push(match n {
            Node::Item(label, a) => entry(label, false, a, actions),
            Node::Check(label, checked, a) => entry(label, *checked, a, actions),
            Node::Disabled(label) => PopupItem::Item { label: label.clone(), id: 0, checked: false, enabled: false },
            Node::Sub(label, kids) => PopupItem::Sub(label.clone(), popup_items(kids, actions)),
            Node::Sep => PopupItem::Sep,
        });
    }
    items
}

/// Right click on the icon: build the menu now, show it, run what was chosen. Blocks in the
/// menu's modal loop (frames still run inside it), so TRAY must not stay borrowed here.
fn show_menu(jobs: &Jobs) {
    if winutil::menu_open() {
        return; // a second right click while the menu is up
    }
    let exiting = TRAY.with(|t| t.try_borrow().ok().and_then(|g| g.as_ref().map(|tray| tray.exiting)).unwrap_or(false));
    let nodes = if exiting { vec![Node::Disabled(EXITING.into())] } else { model(&jobs.app) };
    let mut actions = vec![];
    let items = popup_items(&nodes, &mut actions);
    let chosen = winutil::popup_menu(&items).and_then(|id| actions.into_iter().nth(id as usize - 1));
    if let Some(action) = chosen {
        super::tray_action(jobs, action);
    }
}

/// Exit is stopping the services (Laragon-style status): the tooltip says so and the menu
/// shows nothing else until the process ends.
pub fn set_exiting() {
    TRAY.with(|t| {
        if let Some(tray) = t.try_borrow_mut().ok().as_mut().and_then(|g| g.as_mut()) {
            tray.exiting = true;
            let _ = tray.icon.set_tooltip(Some(format!("devstack: {}", EXITING.to_lowercase())));
        }
    });
}

pub fn set_running(running: bool) {
    TRAY.with(|t| {
        if let Some(tray) = t.try_borrow_mut().ok().as_mut().and_then(|g| g.as_mut()) {
            if tray.running != Some(running) {
                tray.running = Some(running);
                let _ = tray.icon.set_icon(icon(running));
                if !tray.exiting {
                    let _ = tray.icon.set_tooltip(Some(format!("devstack: {}", if running { "running" } else { "stopped" })));
                }
            }
        }
    });
}

pub fn exists() -> bool {
    TRAY.with(|t| t.try_borrow().map(|g| g.is_some()).unwrap_or(true)) // borrowed = in use = exists
}

/// Takes the icon out of the notification area, right before the process exits. The tray
/// objects are forgotten, not dropped and not left in TRAY: this runs inside the tray's own
/// event handler, and `process::exit` still runs this thread's thread-local destructors.
pub fn remove() {
    TRAY.with(|t| {
        if let Some(tray) = t.try_borrow_mut().ok().and_then(|mut g| g.take()) {
            let _ = tray.icon.set_visible(false);
            std::mem::forget(tray);
        }
    });
}

pub fn create(jobs: Jobs) -> Result<()> {
    // No menu is handed to tray-icon (see the top of this file); it only reports the clicks.
    let tray = TrayIconBuilder::new()
        .with_tooltip("devstack")
        .with_icon(icon(false).ok_or_else(|| anyhow::anyhow!("bad icon"))?)
        .build()?;
    TRAY.with(|t| *t.borrow_mut() = Some(Tray { icon: tray, running: None, exiting: false }));

    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| match e {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
        | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => jobs.show(),
        TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Up, .. } => show_menu(&jobs),
        _ => {}
    }));
    Ok(())
}
