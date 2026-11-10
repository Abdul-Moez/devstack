//! `devstack.exe`: tray icon + window. No console window on Windows.
//!
//! Options: `--hidden` (start in the tray), `--page=<name>` (open on a page: overview, sites,
//! new, install, install:php, php, databases, tools, logs, settings).
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("_elevated") {
        // UAC helper (hosts file): runs the CLI command silently and exits.
        let mut cli_args = vec!["ds".to_string()];
        cli_args.extend(args.into_iter().skip(1));
        std::process::exit(devstack::cli::run(cli_args));
    }
    let hidden = args.iter().any(|a| a == "--hidden");
    let page = args.iter().find_map(|a| a.strip_prefix("--page=")).map(str::to_string);
    std::process::exit(devstack::gui::run(hidden, page));
}
