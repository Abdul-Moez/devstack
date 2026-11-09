# devstack

A personal, portable Laragon-style dev environment for Windows 10/11, written in Rust.
Tray icon + window + command line. Apache or Nginx, PHP, Python, Node.js, MySQL/MariaDB,
PostgreSQL, MongoDB, Redis, Memcached, Mailpit; every folder in `www\` becomes
`https://name.test` with a trusted certificate.

Several versions of each can sit side by side (switch with the Version dropdown):

| Tool | Versions in Quick add |
|---|---|
| PHP | 8.5, 8.4, 8.3, 8.2, 8.1, 8.0, 7.4, 7.3, 7.2, 7.1, 7.0 |
| Python | 3.15, 3.14, 3.13, 3.12, 3.11, 3.10, 3.9, 3.8 (full Python with pip and venv) |
| MySQL | 26.10 Innovation, 9.7 LTS, 8.4 LTS, 8.0, 5.7, 5.6 |
| MariaDB | 12.3, 11.8, 11.4, 10.11, 10.6 (all LTS) |
| PostgreSQL | 18, 17, 16, 15, 14, 13 |
| MongoDB | newest, 8.0, 7.0, 6.0 |
| Node.js | current LTS, 26, 24, 22, 20, 18, 16 |
| Composer | latest, and 2.2 LTS (used automatically with PHP 7.0/7.1) |

Each MySQL/MariaDB series keeps its own data folder, so switching versions never touches
another version's databases. MySQL 5.6 needs the Visual C++ 2010 runtime (`ds doctor` says
so if it's missing).

## Install

Run `devstack-setup-<version>.exe`:

1. Pick a folder (default `C:\devstack`; anywhere you can write, not Program Files).
2. Pick what to download now: Recommended = Apache, PHP 8.4, MySQL 8.4, Mailpit, Composer,
   phpMyAdmin. Anything else can be added later from **Menu > Tools > Quick add**.
3. Optional: desktop icon, add tools to PATH, run at Windows startup.

The installed folder contains only `devstack.exe`, `ds.exe` and the uninstaller. Everything
else (`bin`, `www`, `data`, `etc`, `logs`, …) is created next to them when needed. Quit
Laragon first if it's running; both want ports 80, 443 and 3306.

## First run

1. Open devstack, click **Start All**. Windows asks once to edit the hosts file.
2. **Menu > Web server > Trust local CA** so HTTPS works without browser warnings.
3. Create a folder in `www\`, or use **Menu > Quick app**. `www\blog` becomes https://blog.test.

## Everyday use

| Want to… | Do |
|---|---|
| New project | Menu > Quick app > Laravel (latest or 11/10/9/8/6/5.5) / WordPress / Symfony / Django / Flask / … |
| Django or Flask | Quick app makes the project with its own `.venv`, starts the dev server in a console window (`run.bat` starts it again later) and https://name.test forwards to it |
| Switch PHP / MySQL / Node version | Version dropdown in the table, or Menu > PHP > Version |
| Toggle a PHP extension | Menu > PHP > Extensions |
| php/composer/node/mysql in any terminal | Menu > Tools > PATH > Add devstack to PATH |
| Terminal with everything on PATH | **Terminal** button |
| See caught emails | **Mail** button (Mailpit) |
| Show a site to someone | Menu > www > Share (needs an ngrok authtoken in Preferences) |
| Open sites from your phone on the same Wi-Fi | Preferences > General > Let other devices on my network open my sites (`ds lan on`). Off by default, so nobody else on the network can see your projects |
| Back up / restore databases | Menu > MySQL |

Command line: `ds start`, `ds stop`, `ds reload`, `ds status`, `ds new Laravel shop`,
`ds use php php-8.4.27-ts`, `ds add php-8.5`, `ds db backup`, `ds lan on`, `ds doctor`, `ds --help`.

Adding a version with Quick add never switches to it; pick it in the Version dropdown
(or `ds use`) when you're ready.

## Customizing

* Extra settings: `usr\php.ini`, `usr\my.ini`, `usr\redis.conf`, `usr\postgresql.conf`
* Extra Apache/Nginx config: any `*.conf` in `usr\apache\` or `usr\nginx\`
* Per-site options (docroot, aliases, no SSL, another host name): `usr\sites.json`
* Any app server (Django, Flask, Node, …) behind https://name.test: `usr\sites.json` →
  `{"myapp": {"proxy": 3000}}`
* Replace a config template: put your copy in `usr\templates\` (same path as in `assets\templates\`)
* Quick app recipes: `usr\quickapps.json`; extra Quick add packages: `usr\packages.json`

## Building from source

Open the folder in VS Code (recommended extensions: rust-analyzer). Then:

* `cargo run --bin devstack`: run the app (data goes to `.\dev-root`)
* `cargo test`: tests
* `scripts\build-installer.ps1`: release build + `dist\devstack-setup-<version>.exe`
  (installs Inno Setup with winget if needed)

Or push to GitHub: `.github/workflows/build.yml` builds the installer and offers it under the
Actions tab (and attaches it to a Release when you push a `v*` tag).

`CLAUDE.md` describes the architecture and the remaining Windows checks for Claude Code.
