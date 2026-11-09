# devstack

A personal, portable Laragon-style dev environment for Windows 10/11, written in Rust.
Tray icon + window + command line. Apache or Nginx, PHP, MySQL/MariaDB, PostgreSQL, MongoDB,
Redis, Memcached, Mailpit; every folder in `www\` becomes `https://name.test` with a trusted
certificate.

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
| New project | Menu > Quick app > Laravel / WordPress / Symfony / … |
| Switch PHP / MySQL / Node version | Version dropdown in the table, or Menu > PHP > Version |
| Toggle a PHP extension | Menu > PHP > Extensions |
| php/composer/node/mysql in any terminal | Menu > Tools > PATH > Add devstack to PATH |
| Terminal with everything on PATH | **Terminal** button |
| See caught emails | **Mail** button (Mailpit) |
| Show a site to someone | Menu > www > Share (needs an ngrok authtoken in Preferences) |
| Back up / restore databases | Menu > MySQL |

Command line: `ds start`, `ds stop`, `ds reload`, `ds status`, `ds new Laravel shop`,
`ds use php php-8.4.27-ts`, `ds add php-8.5`, `ds db backup`, `ds doctor`, `ds --help`.

## Customizing

* Extra settings: `usr\php.ini`, `usr\my.ini`, `usr\redis.conf`, `usr\postgresql.conf`
* Extra Apache/Nginx config: any `*.conf` in `usr\apache\` or `usr\nginx\`
* Per-site options (docroot, aliases, no SSL, another host name): `usr\sites.json`
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
