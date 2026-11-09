# CLAUDE.md

devstack is a personal, portable Laragon-style local dev environment for Windows, written in
Rust. Two executables: `devstack.exe` (tray icon + window, egui) and `ds.exe` (command line).
Everything they need (config templates, the Quick add catalogue, icons) is compiled in, so an
installation is just those two files; all runtime data is created next to them.

It is a port of a Python prototype whose behavior was tested against real Apache, Nginx,
php-cgi, MariaDB, Redis and Memcached. The Rust version passed the same live tests on Linux.

## First tasks on Windows (do these in order)

Steps 1-3 are done (2026-11-09): it builds on Windows with no errors or clippy warnings,
`cargo test` passes, and `ds status` / `ds doctor` / `ds add composer phpmyadmin` work.
Laragon also uses ports 80, 443, 3306, 1025 and 8025; quit it before step 4.

1. Toolchain: `winget install Rustlang.Rustup` and the MSVC build tools
   (`winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`).
2. `cargo build`, then `cargo clippy --all-targets` (keep it warning-free).
3. `cargo test` must pass.
4. `cargo run --bin devstack` (data goes to `./dev-root`, see `.cargo/config.toml`) and walk
   through: Quick add PHP 8.4 + Apache + MySQL 8.4 + Mailpit + Composer + phpMyAdmin → Start All
   → UAC prompt for hosts → Menu > Web server > Trust local CA → create `dev-root/www/test` →
   https://test.test works without a warning → Quick app > Laravel → Nginx switch → Stop All.
5. Check the Windows-only behaviors: tray icon (left click shows the window, right-click menu
   is current and works while the window is hidden), closing the window hides to the tray,
   single instance (second launch focuses the first), Tools > PATH add/remove, Run at startup,
   Terminal button, `bin/current/*` junctions, Share (needs an ngrok token).
6. `ds resolve` (hidden) looks up every Quick add entry without downloading and prints URL,
   folder and checksum type; all 63 resolved on 2026-11-09. Installed for real so far:
   composer, composer-lts, phpmyadmin, python-3.12, php-7.1. Still to try: the big ones
   (Apache, MySQL, MariaDB, PostgreSQL, MongoDB) and actually starting them.
   **MySQL entries are pinned versions** (`"urls"` = Downloads/ then archives/ on
   cdn.mysql.com): dev.mysql.com answers devstack's client with 403, so it can't be scraped.
   Bump the versions in `assets/packages.json` when MySQL releases.
7. `scripts\build-installer.ps1` → `dist\devstack-setup-<version>.exe`. Test it in Windows
   Sandbox: install to C:\devstack with the Recommended set, run, uninstall.

## Map

| File | What it does |
|---|---|
| `src/core.rs` | `App`: start_all / stop_all / reload / switch_version / set_web_server / set_ssl / PATH / doctor. The CLI and GUI only call this. |
| `src/services.rs` | `Service` trait (default methods do port checks, spawn, health check, stop) + one struct per program: Apache, Nginx, Php (php-cgi pool), MySql, Postgres, Mongo, Redis, Memcached, Mailpit. |
| `src/configs.rs` | Renders `assets/templates/*.j2` (minijinja) into `etc/` (php.ini into the PHP folder). Returns which services' configs changed. |
| `src/sites.rs` | Each folder in `www/` → a site (`{name}.test`, `public/` docroot if present, `usr/sites.json` overrides; `"proxy": <port>` forwards the site to an app server). |
| `src/hosts.rs` | Owns one marked block in the hosts file; writes through a UAC helper only when it changed. |
| `src/ssl.rs` | Local CA + per-site certs (rcgen), trust via `certutil -user`. |
| `src/packages.rs` | Quick add: resolvers → download → checksum → extract to `bin/<tool>/<folder>/`. Sources: php (releases.json, archive folder for 7.0-7.3), python (official NuGet package, SHA-512), mariadb (REST API, SHA-256), node, composer (`lts` = 2.2), github, go, phpmyadmin, postgresql, mongodb, scrape, url (`urls` = mirrors). |
| `src/updates.rs` | Daily: newest PHP patch per installed series, cacert.pem refresh. |
| `src/database.rs` | MySQL tools (create db, root password, backups, recover/restore), PostgreSQL backup, scheduled backups. |
| `src/quickapp.rs` | Quick app recipes (built-in list merged with `usr/quickapps.json`): blank, zip, composer, command, python (.venv + pip + commands, proxy site on a free port 8001+, `run.bat`, dev server opened in a console). Composer 2.2 LTS is picked automatically for PHP < 7.2.5. |
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
9. Downloads with a published checksum are verified; mismatches are hard errors. Downloads
   are HTTPS only.
10. **Untrusted names are validated before they touch the disk or admin code.** Folder and
    file names from download sources go through `util::safe_file_name`; host names (from
    `www/` folders, the host name template, `usr/sites.json`) through `sites::valid_host`,
    because they end up in the hosts file. The elevated hosts helper only accepts
    `127.0.0.1 <host>` lines (`hosts::block_is_safe`) and ignores `DEVSTACK_HOSTS`.
11. **Local only by default.** Apache/Nginx listen on 127.0.0.1 unless `lan_access` is on
    (Preferences, `ds lan on`); every other service always binds 127.0.0.1. Dotfiles
    (`.env`, `.git`) are never served.
12. **A Quick add install never switches versions.** `packages::pin_version` saves the
    version that was active before (`versions.<tool>`); only `switch_version` changes it.
13. Secrets stay off command lines (ngrok gets `NGROK_AUTHTOKEN`, MySQL tools read
    `etc/mysql/client.cnf`).

## Old versions

* PHP 7.0/7.1 need `extension=php_x.dll` (configs pass `php_ext_dll`); 7.0 gets
  `serialize_precision = 17`. mbstring is loaded before exif.
* MySQL 5.6 has no `--initialize`: the data folder is copied from the zip's `data/`. It links
  the Visual C++ 2010 runtime (`services::mysql_missing_runtime` checks before starting);
  5.7+ use the current runtime. Password changes fall back to `SET PASSWORD` on 5.6.
* Apache Lounge builds are now VS18 (Visual Studio 2026) and want a 14.50+ VC++ runtime;
  the installer and `ds doctor` check the version, not just presence.

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
