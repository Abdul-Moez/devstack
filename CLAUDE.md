# CLAUDE.md

devstack is a personal, portable Laragon-style local dev environment for Windows, written in
Rust. Two executables: `devstack.exe` (tray icon + window, egui) and `ds.exe` (command line).
Everything they need (config templates, the Quick add catalogue, icons) is compiled in, so an
installation is just those two files; all runtime data is created next to them.

It is a port of a Python prototype whose behavior was tested against real Apache, Nginx,
php-cgi, MariaDB, Redis and Memcached. The Rust version passed the same live tests on Linux.

## First tasks on Windows (do these in order)

The Windows-only code paths have **never been compiled** (the port was written on Linux,
without a Windows target). Expect a few small compile errors there; fix them first.

1. Toolchain: `winget install Rustlang.Rustup` and the MSVC build tools
   (`winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`).
2. `cargo build` and fix errors. Likely spots, all behind `#[cfg(windows)]`:
   `src/winutil.rs` (windows-sys 0.59 types: HWND/HANDLE are `*mut c_void`; winreg `RegValue`),
   `src/procs.rs` (creation flags), `src/gui/mod.rs` (getting the HWND from
   `CreationContext` via raw-window-handle), `build.rs` (winresource icon).
3. `cargo test` must pass.
4. `cargo run --bin devstack` (data goes to `./dev-root`, see `.cargo/config.toml`) and walk
   through: Quick add PHP 8.4 + Apache + MySQL 8.4 + Mailpit + Composer + phpMyAdmin → Start All
   → UAC prompt for hosts → Menu > Web server > Trust local CA → create `dev-root/www/test` →
   https://test.test works without a warning → Quick app > Laravel → Nginx switch → Stop All.
5. Check the Windows-only behaviors: tray icon (left click shows the window, right-click menu
   is current and works while the window is hidden), closing the window hides to the tray,
   single instance (second launch focuses the first), Tools > PATH add/remove, Run at startup,
   Terminal button, `bin/current/*` junctions, Share (needs an ngrok token).
6. Check every Quick add entry actually downloads (`ds packages`, then `ds add <id>`). The
   sources' formats were checked against current pages, but the sandbox couldn't download.
   Fix entries in `assets/packages.json`. Least certain: `memcached`, `apache` (scraper).
7. `scripts\build-installer.ps1` → `dist\devstack-setup-<version>.exe`. Test it in Windows
   Sandbox: install to C:\devstack with the Recommended set, run, uninstall.

## Map

| File | What it does |
|---|---|
| `src/core.rs` | `App`: start_all / stop_all / reload / switch_version / set_web_server / set_ssl / PATH / doctor. The CLI and GUI only call this. |
| `src/services.rs` | `Service` trait (default methods do port checks, spawn, health check, stop) + one struct per program: Apache, Nginx, Php (php-cgi pool), MySql, Postgres, Mongo, Redis, Memcached, Mailpit. |
| `src/configs.rs` | Renders `assets/templates/*.j2` (minijinja) into `etc/` (php.ini into the PHP folder). Returns which services' configs changed. |
| `src/sites.rs` | Each folder in `www/` → a site (`{name}.test`, `public/` docroot if present, `usr/sites.json` overrides). |
| `src/hosts.rs` | Owns one marked block in the hosts file; writes through a UAC helper only when it changed. |
| `src/ssl.rs` | Local CA + per-site certs (rcgen), trust via `certutil -user`. |
| `src/packages.rs` | Quick add: resolvers (php, node, github, scrape, …) → download → SHA-256 → extract to `bin/<tool>/<folder>/`. |
| `src/updates.rs` | Daily: newest PHP patch per installed series, cacert.pem refresh. |
| `src/database.rs` | MySQL tools (create db, root password, backups, recover/restore), PostgreSQL backup, scheduled backups. |
| `src/quickapp.rs` | Quick app recipes (`usr/quickapps.json`): blank, zip, composer, command. |
| `src/share.rs` | ngrok tunnel and its public URL. |
| `src/winutil.rs` | **All Win32 code**: UAC elevation, user PATH, autostart, junctions, show/hide window, single instance, terminals. |
| `src/procs.rs` | Detached spawning, PID + start-time tracking in `tmp/state.json`, port owners (netstat2/sysinfo). |
| `src/versions.rs` | `TOOLS`: tool → bin folder + executable(s). A version is any subfolder containing the exe. |
| `src/gui/mod.rs` | Window, dialogs, preferences, background jobs (`Jobs::spawn`), ticker thread (PHP pool healing, backups, update checks). |
| `src/gui/menu.rs` | One menu model rendered both in the window and in the tray. `Action` is serialized as the tray item id. |
| `src/gui/tray.rs` | Tray icon (Windows only at runtime). Menu rebuilt when the pointer enters the icon. |
| `src/cli.rs` | `ds` commands, plus hidden `_elevated hosts <file>` and installer helper `setup`. |
| `installer/devstack.iss` | Inno Setup script: folder choice, component downloads (`ds setup`), PATH/autostart tasks, VC++ runtime, clean uninstall. |

## Rules

1. **Generated files are disposable.** Never hand-edit `etc/`. Customization goes in `usr/`
   (appended files, extra `*.conf`, template overrides in `usr/templates/`).
2. **No hardcoded absolute paths.** Everything derives from the root (folder of the exe, or
   `DEVSTACK_ROOT`). Config files use forward slashes (`util::fwd`).
3. **Databases are never killed silently.** MySQL/PostgreSQL stop with their shutdown command;
   on timeout `stop()` returns `DsError::NeedsForce` and the UI asks.
4. **The main process never runs elevated.** Admin work goes through `winutil::run_elevated`
   → `<exe> _elevated …`, doing one task.
5. **`util::run()` only for short commands.** Commands that leave background processes
   (pg_ctl start) use `run_logged()`, or inherited pipes make `run()` hang.
6. **Locking:** public `App` operations take `op_lock`; internal `*_inner` helpers assume it's
   held. Background maintenance uses `try_lock` and skips if an operation is running.
7. **Win32 code only in `winutil.rs`** (plus the HWND lookup in `gui/mod.rs` and creation flags
   in `procs.rs`/`util.rs`). Keep everything else cross-platform so it can be tested on Linux.
8. The GUI thread never blocks: slow work goes through `Jobs::spawn`.
9. Downloads with a published checksum are verified; mismatches are hard errors.

## How PHP is wired (and why)

* Apache loads PHP as a module (`php8apache2_4.dll`, thread-safe builds; Quick add defaults to
  `ts`). Apache gets the PHP folder on PATH so extension DLLs load. php.ini changes restart Apache.
* Nginx uses the php-cgi pool: N processes from port 10987 (fallback 20987), healed every 2 s.
* mod_proxy_fcgi + php-cgi for Apache was tried and rejected: with `.htaccess` rewrites Apache
  sends a bogus `PATH_INFO`/`PATH_TRANSLATED` and php-cgi answers "No input file specified".
  That path stays only as a fallback for NTS builds.

## Adding things

* **A service:** implement `Service` in `services.rs`, add it to `services::all()`, add the
  tool to `versions::TOOLS`, defaults to `settings::defaults()` (services + ports).
* **A Quick add package:** `assets/packages.json` (built in) or `usr/packages.json` (local).
* **A Quick app recipe:** `assets/quickapps.json` / `usr/quickapps.json`.
* **A config template:** `assets/templates/<tool>/x.j2`, register it in `assets.rs` and
  `configs::outputs()`.

## Testing

* `cargo test` runs anywhere (hosts block, versions, sites, certs incl. `openssl verify`,
  config rendering, menu ids, catalogue regexes).
* The core runs on Linux (`util::exe()` strips `.exe`); it was live-tested by symlinking Linux
  binaries into a fake `bin/` tree. Use `DEVSTACK_ROOT` and `DEVSTACK_HOSTS` to point at a
  test folder and a fake hosts file.
* `ds doctor` after changes; failing services include their last log lines in the error.
