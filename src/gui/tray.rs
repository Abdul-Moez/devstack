//! The tray icon. Created only on Windows (on Linux the tray needs a GTK loop; the window
//! works without it). The menu is rebuilt when the pointer enters the icon, so it always
//! lists current sites and versions, and actions run even while the window is hidden.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use anyhow::Result;
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::menu::{model, Action, Node};
use super::Jobs;
use crate::assets;

struct Tray {
    icon: TrayIcon,
    built: Instant,
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

fn append(add: &dyn Fn(&dyn IsMenuItem), nodes: &[Node]) {
    for n in nodes {
        match n {
            Node::Item(label, a) => add(&MenuItem::with_id(a.id(), label, true, None)),
            Node::Disabled(label) => add(&MenuItem::new(label, false, None)),
            Node::Check(label, checked, a) => add(&CheckMenuItem::with_id(a.id(), label, true, *checked, None)),
            Node::Sub(label, kids) => {
                let sub = Submenu::new(label, true);
                append(&|i| { let _ = sub.append(i); }, kids);
                add(&sub);
            }
            Node::Sep => add(&PredefinedMenuItem::separator()),
        }
    }
}

fn build_menu(jobs: &Jobs, exiting: bool) -> Menu {
    let menu = Menu::new();
    let nodes = if exiting { vec![Node::Disabled(EXITING.into())] } else { model(&jobs.app) };
    append(&|i| { let _ = menu.append(i); }, &nodes);
    menu
}

/// Rebuild the menu (on the UI thread) if it's older than a second. Never while the menu is
/// open: replacing it destroys the menu Windows is showing (a frame or an Enter event can
/// arrive inside the menu's modal loop).
pub fn refresh_menu(jobs: &Jobs) {
    if crate::winutil::menu_open() {
        return;
    }
    TRAY.with(|t| {
        // try_borrow: never panic if Windows re-enters us while the menu is being replaced
        if let Some(tray) = t.try_borrow_mut().ok().as_mut().and_then(|g| g.as_mut()) {
            if tray.built.elapsed() > Duration::from_secs(1) {
                tray.icon.set_menu(Some(Box::new(build_menu(jobs, tray.exiting))));
                tray.built = Instant::now();
            }
        }
    });
}

/// Exit is stopping the services (Laragon-style status): the tooltip says so and the menu
/// shows nothing else until the process ends.
pub fn set_exiting(jobs: &Jobs) {
    let open = crate::winutil::menu_open();
    TRAY.with(|t| {
        if let Some(tray) = t.try_borrow_mut().ok().as_mut().and_then(|g| g.as_mut()) {
            tray.exiting = true;
            let _ = tray.icon.set_tooltip(Some(format!("devstack: {}", EXITING.to_lowercase())));
            if !open {
                tray.icon.set_menu(Some(Box::new(build_menu(jobs, true))));
                tray.built = Instant::now();
            } // else refresh_menu swaps it once the menu is closed
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
    TRAY.with(|t| t.borrow().is_some())
}

/// Takes the icon out of the notification area, right before the process exits. The tray
/// objects are never dropped: Exit runs inside the tray menu's click handler, and dropping
/// the menu there panics (muda: "RefCell already borrowed"). They are forgotten rather than
/// left in TRAY, because `process::exit` still runs this thread's thread-local destructors.
pub fn remove() {
    TRAY.with(|t| {
        if let Some(tray) = t.try_borrow_mut().ok().and_then(|mut g| g.take()) {
            let _ = tray.icon.set_visible(false);
            std::mem::forget(tray);
        }
    });
}

pub fn create(jobs: Jobs) -> Result<()> {
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(build_menu(&jobs, false)))
        .with_tooltip("devstack")
        .with_icon(icon(false).ok_or_else(|| anyhow::anyhow!("bad icon"))?)
        .with_menu_on_left_click(false)
        .build()?;
    TRAY.with(|t| *t.borrow_mut() = Some(Tray { icon: tray, built: Instant::now(), running: None, exiting: false }));

    let j = jobs.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Some(action) = Action::from_id(&e.id.0) {
            super::tray_action(&j, action);
        }
    }));
    let j = jobs;
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| match e {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
        | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => j.show(),
        TrayIconEvent::Enter { .. } => refresh_menu(&j),
        _ => {}
    }));
    Ok(())
}
