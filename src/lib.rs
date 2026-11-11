//! devstack: a personal, portable Laragon-style local dev environment for Windows.
//!
//! The `devstack` binary is the tray app + window, `ds` is the command line. Both drive
//! [`core::App`]; nothing else talks to services directly.

// The template context in configs.rs is one large json! literal.
#![recursion_limit = "256"]

pub mod assets;
pub mod cli;
pub mod configs;
pub mod core;
pub mod database;
pub mod error;
pub mod gui;
pub mod hosts;
pub mod laragon;
pub mod packages;
pub mod paths;
pub mod procs;
pub mod quickapp;
pub mod services;
pub mod settings;
pub mod share;
pub mod sites;
pub mod ssl;
pub mod updates;
pub mod util;
pub mod versions;
pub mod winutil;

pub const APP_NAME: &str = "devstack";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Log callback passed into long-running operations (CLI prints, GUI shows in the log pane).
pub type Log<'a> = &'a dyn Fn(&str);
