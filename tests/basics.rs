//! Platform-independent checks: `cargo test`

use std::fs;
use std::path::{Path, PathBuf};

use devstack::core::App;
use devstack::util::exe;
use devstack::{hosts, ssl};

fn temp_root() -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let p = std::env::temp_dir().join(format!("devstack-test-{n}-{}", std::process::id()));
    fs::create_dir_all(&p).unwrap();
    p
}

/// For tests that start, reload or switch things (all of which sync the hosts file): points
/// devstack at a fake hosts file inside `root`, and keeps other such tests from changing the
/// variable meanwhile. Without it devstack would ask Windows for admin rights to edit the
/// real hosts file.
fn fake_hosts(root: &Path) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let file = root.join("hosts");
    fs::write(&file, "127.0.0.1 localhost\n").unwrap();
    std::env::set_var("DEVSTACK_HOSTS", &file);
    (guard, file)
}

fn touch(p: &Path) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, "").unwrap();
}

#[test]
fn hosts_block_is_idempotent_and_leaves_user_lines() {
    let base = "127.0.0.1 localhost\n# my own line\n";
    let b = hosts::build_block(&["b.test".into(), "a.test".into(), "a.test".into()]);
    let t1 = hosts::with_block(base, &b);
    assert_eq!(t1.matches(hosts::BEGIN).count(), 1);
    assert!(t1.contains("127.0.0.1 a.test\n127.0.0.1 b.test"));
    let t2 = hosts::with_block(&t1, &hosts::build_block(&["c.test".into()]));
    assert!(!t2.contains("a.test") && t2.contains("c.test") && t2.contains("# my own line"));
    assert_eq!(hosts::with_block(&t2, ""), base);
    assert_eq!(hosts::with_block(&hosts::with_block(&t2, &b), &b), hosts::with_block(&t2, &b));
}

#[test]
fn versions_sort_numerically_and_sites_are_discovered() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    for name in ["php-8.3.20-ts", "php-8.10.1-ts", "php-8.4.14-ts"] {
        touch(&root.join("bin/php").join(name).join(exe("php-cgi")));
    }
    let names: Vec<String> = app.versions("php").into_iter().map(|v| v.name).collect();
    assert_eq!(names, ["php-8.10.1-ts", "php-8.4.14-ts", "php-8.3.20-ts"]);
    app.settings.set("versions.php", serde_json::json!("php-8.4.14-ts")).unwrap();
    assert_eq!(app.active("php").unwrap().series(), "8.4");

    fs::create_dir_all(root.join("www/My Blog")).unwrap();
    fs::create_dir_all(root.join("www/shop/public")).unwrap();
    fs::write(root.join("usr/sites.json"), r#"{"shop": {"aliases": ["shop.local"]}}"#).unwrap();
    let sites = app.sites();
    let blog = sites.iter().find(|s| s.name == "My Blog").unwrap();
    let shop = sites.iter().find(|s| s.name == "shop").unwrap();
    assert_eq!(blog.host, "my-blog.test");
    assert!(shop.docroot.ends_with("public"));
    assert_eq!(shop.aliases, ["shop.local"]);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn certificates_are_signed_by_the_local_ca_and_reused() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    let hosts = vec!["shop.test".to_string(), "*.shop.test".to_string()];
    let (crt, key, made) = ssl::ensure_cert(&app.paths, "shop.test", &hosts).unwrap();
    assert!(made && crt.exists() && key.exists());
    assert!(!ssl::ensure_cert(&app.paths, "shop.test", &hosts).unwrap().2, "unchanged cert should be reused");
    // verify the chain with openssl if it's available
    if let Ok(out) = std::process::Command::new("openssl")
        .args(["verify", "-CAfile"])
        .arg(ssl::ca_files(&app.paths).0)
        .arg(&crt)
        .output()
    {
        assert!(out.status.success(), "openssl verify: {}", String::from_utf8_lossy(&out.stderr));
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn configs_render_and_are_stable() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    app.settings.set("updates.cacert", serde_json::json!(false)).unwrap();
    let php = root.join("bin/php/php-8.4.14-ts");
    touch(&php.join(exe("php-cgi")));
    touch(&php.join("php8apache2_4.dll"));
    for e in ["curl", "mbstring", "opcache", "xdebug"] {
        touch(&php.join("ext").join(format!("php_{e}.dll")));
    }
    touch(&root.join("bin/apache/apache-2.4.70/bin").join(exe("httpd")));
    touch(&root.join("bin/nginx/nginx-1.28.0").join(exe("nginx")));
    touch(&root.join("bin/mysql/mysql-8.4.7/bin").join(exe("mysqld")));
    fs::create_dir_all(root.join("www/blog")).unwrap();

    let changed = app.generate(&|_| {}).unwrap();
    for s in ["apache", "nginx", "php", "mysql"] {
        assert!(changed.contains(s), "{s} should have been generated");
    }
    let httpd = fs::read_to_string(root.join("etc/apache/httpd.conf")).unwrap();
    assert!(httpd.contains("LoadModule php_module") && !httpd.contains("balancer://"));
    assert!(httpd.contains("Listen 127.0.0.1:80\n") && httpd.contains("RedirectMatch 404"), "local-only by default, dotfiles blocked");
    assert!(fs::read_to_string(root.join("etc/apache/sites.conf")).unwrap().contains("ServerName blog.test"));
    assert!(fs::read_to_string(root.join("etc/nginx/nginx.conf")).unwrap().contains("server 127.0.0.1:10987;"));
    let nginx_sites = fs::read_to_string(root.join("etc/nginx/sites.conf")).unwrap();
    assert!(nginx_sites.contains("server_name blog.test *.blog.test;"));
    assert!(nginx_sites.contains("listen 127.0.0.1:80 default_server;") && nginx_sites.contains("listen 127.0.0.1:80;"));
    assert!(fs::read_to_string(root.join("etc/mysql/my.ini")).unwrap().contains("loose-mysqlx-bind-address = 127.0.0.1"));
    let ini = fs::read_to_string(php.join("php.ini")).unwrap();
    assert!(ini.contains("extension=curl") && ini.contains("zend_extension=opcache") && !ini.contains("zend_extension=xdebug"));
    assert!(app.generate(&|_| {}).unwrap().is_empty(), "second generate should change nothing");

    app.settings.set("lan_access", serde_json::json!(true)).unwrap();
    let changed = app.generate(&|_| {}).unwrap();
    assert!(changed.contains("apache") && changed.contains("nginx"));
    assert!(fs::read_to_string(root.join("etc/apache/httpd.conf")).unwrap().contains("Listen 80\n"));
    assert!(fs::read_to_string(root.join("etc/nginx/sites.conf")).unwrap().contains("listen 80 default_server;"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn host_names_from_overrides_are_validated() {
    use devstack::sites::valid_host;
    for ok in ["blog.test", "my-blog.test", "a.b.c", "shop.local", "x1.test"] {
        assert!(valid_host(ok), "{ok} should be accepted");
    }
    for bad in ["", "-x.test", "x-.test", "a b.test", "evil.test\n1.2.3.4 bank.com", "*.x.test", "a..test", "UPPER.test", "a_b.test"] {
        assert!(!valid_host(bad), "{bad:?} should be rejected");
    }

    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    fs::create_dir_all(root.join("www/shop")).unwrap();
    fs::write(root.join("usr/sites.json"), r#"{"shop": {"aliases": ["Shop.Local", "bad alias", "x.test\n9.9.9.9 bank.com"]}}"#).unwrap();
    let shop = app.sites().into_iter().find(|s| s.name == "shop").unwrap();
    assert_eq!(shop.aliases, ["shop.local"]);
    fs::write(root.join("usr/sites.json"), r#"{"shop": {"host": "bank.com 1.2.3.4"}}"#).unwrap();
    assert!(app.sites().iter().all(|s| s.name != "shop"), "a site with an invalid host is skipped");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn elevated_hosts_helper_only_accepts_loopback_entries() {
    let good = hosts::with_block("127.0.0.1 localhost\n", &hosts::build_block(&["a.test".into(), "b.test".into()]));
    assert!(hosts::block_is_safe(&good));
    assert!(hosts::block_is_safe("127.0.0.1 localhost\n"), "no block at all is fine");
    let evil = good.replace("127.0.0.1 a.test", "6.6.6.6 bank.com");
    assert!(!hosts::block_is_safe(&evil));
    let unterminated = format!("{}\n127.0.0.1 a.test\n", hosts::BEGIN);
    assert!(!hosts::block_is_safe(&unterminated));
}

#[test]
fn old_php_builds_are_found_in_the_archive_listing() {
    use devstack::packages::newest_php_archive;
    let html = r#"<a href="php-7.1.9-Win32-VC14-x64.zip"> <a href="php-7.1.33-Win32-VC14-x64.zip"> <a href="php-7.1.33-Win32-VC14-x86.zip">
                  <a href="php-7.1.33-nts-Win32-VC14-x64.zip"> <a href="php-7.4.33-Win32-vc15-x64.zip"> <a href="php-7.10.1-Win32-VC14-x64.zip">"#;
    assert_eq!(newest_php_archive(html, "7.1", "ts"), Some((33, "php-7.1.33-Win32-VC14-x64.zip".into())));
    assert_eq!(newest_php_archive(html, "7.1", "nts"), Some((33, "php-7.1.33-nts-Win32-VC14-x64.zip".into())));
    assert_eq!(newest_php_archive(html, "7.4", "ts"), Some((33, "php-7.4.33-Win32-vc15-x64.zip".into())));
    assert_eq!(newest_php_archive(html, "7.0", "ts"), None);
}

#[test]
fn php_7_0_and_7_1_get_full_extension_file_names() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    app.settings.set("updates.cacert", serde_json::json!(false)).unwrap();
    let php = root.join("bin/php/php-7.1.33-ts");
    touch(&php.join(exe("php-cgi")));
    for e in ["curl", "exif", "mbstring", "opcache"] {
        touch(&php.join("ext").join(format!("php_{e}.dll")));
    }
    app.generate(&|_| {}).unwrap();
    let ini = fs::read_to_string(php.join("php.ini")).unwrap();
    assert!(ini.contains("extension=php_curl.dll") && ini.contains("zend_extension=php_opcache.dll"), "{ini}");
    assert!(ini.find("extension=php_mbstring.dll").unwrap() < ini.find("extension=php_exif.dll").unwrap(), "mbstring loads before exif");
    assert!(ini.contains("serialize_precision = -1"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn proxied_sites_forward_to_their_app_server() {
    use devstack::sites::proxy_port;
    use serde_json::json;
    assert_eq!(proxy_port(Some(&json!(8001))), Some(8001));
    assert_eq!(proxy_port(Some(&json!("8002"))), Some(8002));
    for bad in [json!(0), json!(70000), json!("http://evil/"), json!("8001; include x"), json!(true)] {
        assert_eq!(proxy_port(Some(&bad)), None, "{bad} must be rejected");
    }

    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    app.settings.set("updates.cacert", serde_json::json!(false)).unwrap();
    touch(&root.join("bin/apache/apache-2.4.70/bin").join(exe("httpd")));
    touch(&root.join("bin/nginx/nginx-1.28.0").join(exe("nginx")));
    fs::create_dir_all(root.join("www/api")).unwrap();
    fs::create_dir_all(root.join("www/blog")).unwrap();
    fs::write(root.join("usr/sites.json"), r#"{"api": {"proxy": 8001}}"#).unwrap();
    app.generate(&|_| {}).unwrap();
    let apache = fs::read_to_string(root.join("etc/apache/sites.conf")).unwrap();
    assert_eq!(apache.matches("ProxyPass / http://127.0.0.1:8001/").count(), 2, "http and https vhosts");
    assert!(apache.contains(r#"X-Forwarded-Proto "https""#));
    let nginx = fs::read_to_string(root.join("etc/nginx/sites.conf")).unwrap();
    assert_eq!(nginx.matches("proxy_pass http://127.0.0.1:8001;").count(), 1);
    assert!(nginx.contains("try_files $uri $uri/ /index.php$is_args$args;"), "normal sites still serve PHP");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn quick_app_recipes_merge_with_the_users_own() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    let names: Vec<String> = devstack::quickapp::recipes(&app).into_iter().map(|r| r.name).collect();
    for n in ["Laravel", "WordPress", "Django", "Flask", "Laravel 8 (PHP 7.3+)"] {
        assert!(names.iter().any(|x| x == n), "{n} missing from {names:?}");
    }
    fs::write(root.join("usr/quickapps.json"), r#"[{"name": "Flask", "type": "blank"}, {"name": "Mine", "type": "blank"}]"#).unwrap();
    let list = devstack::quickapp::recipes(&app);
    assert_eq!(list.iter().find(|r| r.name == "Flask").unwrap().kind, "blank", "user recipe replaces the built-in one");
    assert!(list.iter().any(|r| r.name == "Mine") && list.iter().any(|r| r.name == "Django"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn old_php_uses_composer_2_2() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    touch(&root.join("bin/php/php-7.1.33-ts").join(exe("php-cgi")));
    touch(&root.join("bin/composer/composer-2.11.1/composer.phar"));
    let err = devstack::quickapp::composer_cmd(&app).unwrap_err().to_string();
    assert!(err.contains("Composer 2.2 LTS"), "{err}");
    touch(&root.join("bin/composer/composer-2.2.31/composer.phar"));
    assert!(devstack::quickapp::composer_cmd(&app).unwrap().1.contains("composer-2.2.31"));
    touch(&root.join("bin/php/php-8.4.14-ts").join(exe("php-cgi")));
    app.settings.set("versions.php", serde_json::json!("php-8.4.14-ts")).unwrap();
    app.settings.set("versions.composer", serde_json::json!("composer-2.11.1")).unwrap();
    assert!(devstack::quickapp::composer_cmd(&app).unwrap().1.contains("composer-2.11.1"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn mysql_5_6_starts_from_the_bundled_data_folder() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    let base = root.join("bin/mysql/mysql-5.6.51");
    touch(&base.join("bin").join(exe("mysqld")));
    touch(&base.join("data/mysql/user.frm"));
    touch(&base.join("data/ibdata1"));
    let v = app.active("mysql").unwrap();
    if devstack::services::mysql_missing_runtime(&v).is_some() {
        return; // this machine has no Visual C++ 2010 runtime; prepare() refuses, as intended
    }
    app.svc("mysql").prepare(&app, &v, &|_| {}).unwrap();
    let dd = devstack::services::mysql_data_dir(&app, &v);
    assert!(dd.ends_with("mysql-5.6") && dd.join("mysql/user.frm").exists() && dd.join("ibdata1").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn quick_add_menu_is_grouped_by_tool() {
    use devstack::gui::menu::{count, quick_add_menu, Node};
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    let catalogue = devstack::packages::catalogue(&app);
    let menu = quick_add_menu(&catalogue);
    let groups: Vec<&str> = menu.iter().filter_map(|n| if let Node::Sub(l, _) = n { Some(l.as_str()) } else { None }).collect();
    for g in ["PHP", "MySQL / MariaDB", "PostgreSQL", "Python", "Node.js"] {
        assert!(groups.contains(&g), "{g} missing from {groups:?}");
    }
    assert_eq!(count(&menu) - groups.len(), catalogue.len(), "every package appears exactly once");
    for id in ["php-7.0", "php-7.4", "python-3.8", "python-3.15", "mysql-5.6", "mysql-5.7", "mariadb-10.6", "postgresql-13", "nodejs-16", "composer-lts"] {
        assert!(catalogue.iter().any(|p| p.id == id), "{id} missing from the catalogue");
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn tools_without_an_exe_are_still_found() {
    use devstack::versions::candidates;
    assert_eq!(candidates("phpmyadmin"), ["index.php"]);
    assert_eq!(candidates("composer"), ["composer.phar"]);
    assert_eq!(candidates("vscode"), [exe("Code.exe")]);
    assert_eq!(candidates("php"), [exe("php-cgi")]);
    if cfg!(windows) {
        assert_eq!(exe("php-cgi"), "php-cgi.exe");
        assert_eq!(exe("redis-cli"), "redis-cli.exe");
    }
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    touch(&root.join("bin/composer/composer-2.8.4/composer.phar"));
    touch(&root.join("bin/phpmyadmin/phpmyadmin-5.2.3/index.php"));
    assert_eq!(app.active("composer").unwrap().name, "composer-2.8.4");
    assert_eq!(app.active("phpmyadmin").unwrap().name, "phpmyadmin-5.2.3");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn download_names_cannot_escape_their_folder() {
    use devstack::util::safe_file_name;
    for ok in ["php-8.4.14-ts", "composer.phar", "go1.23.4.windows-amd64.zip", "MinGit-2.47.1-64-bit.zip", "phpMyAdmin-6.0+snapshot.zip"] {
        assert!(safe_file_name(ok), "{ok} should be accepted");
    }
    for bad in ["", "..", "../bin", "php-../../x", "a/b", r"a\b", "C:x", ".hidden", "x\ny"] {
        assert!(!safe_file_name(bad), "{bad:?} should be rejected");
    }
}

#[test]
fn installing_a_newer_version_keeps_the_active_one() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    touch(&root.join("bin/mysql/mysql-8.4.7/bin").join(exe("mysqld")));
    let before = app.active("mysql");
    assert_eq!(before.as_ref().unwrap().name, "mysql-8.4.7");
    // What Quick add does: extract the new version, then pin.
    touch(&root.join("bin/mysql/mysql-9.5.0/bin").join(exe("mysqld")));
    devstack::packages::pin_version(&app, "mysql", before, "mysql-9.5.0").unwrap();
    assert_eq!(app.active("mysql").unwrap().name, "mysql-8.4.7", "a new install must not switch versions");

    // First install of a tool: that version becomes the saved choice.
    touch(&root.join("bin/php/php-8.4.14-ts").join(exe("php-cgi")));
    devstack::packages::pin_version(&app, "php", None, "php-8.4.14-ts").unwrap();
    assert_eq!(app.settings.str("versions.php"), "php-8.4.14-ts");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn settings_changed_by_another_process_are_not_overwritten() {
    let root = temp_root();
    let gui = App::new(Some(root.clone())).unwrap();
    let cli = App::new(Some(root.clone())).unwrap();
    cli.settings.set("web_server", serde_json::json!("nginx")).unwrap();
    gui.settings.set("ssl", serde_json::json!(false)).unwrap();
    let on_disk = devstack::util::load_json(&root.join("settings.json")).unwrap();
    assert_eq!(on_disk["web_server"], "nginx", "the CLI's change survives the GUI's save");
    assert_eq!(on_disk["ssl"], false);
    assert_eq!(gui.settings.str("web_server"), "nginx");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn env_overrides_replace_any_spelling_of_the_name() {
    let env = devstack::util::env_with(&std::collections::HashMap::from([("PATH".to_string(), "ours".to_string())]));
    let paths: Vec<_> = env.iter().filter(|(k, _)| k.eq_ignore_ascii_case("path")).collect();
    if cfg!(windows) {
        assert_eq!(paths.len(), 1, "only one PATH may reach the child process: {paths:?}");
    }
    assert_eq!(env.get("PATH").map(String::as_str), Some("ours"));
}

#[test]
fn ports_fall_back_to_defaults_and_processes_are_tracked() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    app.settings.set("ports.http", serde_json::json!(70000)).unwrap();
    assert_eq!(app.settings.port("http"), 80, "an out-of-range port falls back to the default");
    app.settings.set("ports.http", serde_json::json!("8080")).unwrap();
    assert_eq!(app.settings.port("http"), 8080);
    let me = devstack::procs::record(std::process::id());
    assert!(me.ctime > 0 && devstack::procs::alive(&me));
    assert!(!devstack::procs::alive(&devstack::settings::ProcRec { pid: me.pid, ctime: me.ctime + 1000, ..Default::default() }),
            "a reused PID (different start time) is not our process");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn menu_actions_round_trip_through_tray_ids() {
    use devstack::gui::menu::Action;
    for a in [Action::StartAll, Action::Version("php".into(), "php-8.4.14-ts".into()), Action::Ext("gd".into(), true),
              Action::OpenPath(PathBuf::from(r"C:\devstack\www"))] {
        assert_eq!(Action::from_id(&a.id()), Some(a));
    }
}

#[test]
fn quick_add_catalogue_is_valid() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    let items = devstack::packages::catalogue(&app);
    assert!(items.len() >= 25);
    for i in &items {
        assert!(devstack::versions::TOOLS.iter().any(|(t, _, _)| *t == i.tool), "{} has unknown tool {}", i.id, i.tool);
        if let Some(rx) = i.source.get("asset").or(i.source.get("pattern")).and_then(|v| v.as_str()) {
            regex::Regex::new(rx).unwrap_or_else(|e| panic!("{}: bad regex: {e}", i.id));
        }
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn new_series_are_discovered_from_the_sources() {
    use serde_json::json;
    let have: std::collections::HashSet<String> = ["php-8.5", "python-3.15", "nodejs-26", "mariadb-11.4", "postgresql-18", "mongodb-8.0"]
        .into_iter()
        .map(String::from)
        .collect();
    let php = json!({"8.5": {}, "8.6": {}});
    let nuget = json!({"versions": ["3.7.9", "3.15.2", "3.16.0", "3.17.0-a1"]});
    let node = json!([
        {"version": "v29.0.0", "files": ["win-x64-zip"]},
        {"version": "v28.1.0", "files": ["win-x64-zip"]},
        {"version": "v27.9.0", "files": ["win-x64-zip"]},
        {"version": "v26.0.0", "files": ["win-x64-zip"]},
        {"version": "v14.0.0", "files": ["win-x64-zip"]}
    ]);
    let mariadb = json!({"major_releases": [
        {"release_id": "13.1", "release_status": "RC"},
        {"release_id": "13.0", "release_status": "Stable", "release_support_type": "Rolling"},
        {"release_id": "11.4", "release_status": "Stable", "release_support_type": "Long Term Support"}
    ]});
    let pg = json!([{"major": "19", "supported": true}, {"major": "12", "supported": false}, {"major": "18", "supported": true}]);
    let mongo = json!({"versions": [{"version": "9.1.0", "production_release": true}, {"version": "5.0.1", "production_release": true}]});
    let ids: Vec<String> = devstack::packages::new_packages(&have, Some(&php), Some(&nuget), Some(&node), Some(&mariadb), Some(&pg), Some(&mongo))
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, ["php-8.6", "python-3.16", "nodejs-29", "nodejs-28", "mariadb-13.0", "postgresql-19", "mongodb-9.1"],
               "no pre-releases, no old odd Node majors, no RC MariaDB, no unsupported PostgreSQL, no Python < 3.8");
}

#[test]
fn discovered_versions_join_the_catalogue_in_order() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    let doc = serde_json::json!({"packages": [
        {"id": "php-8.6", "label": "PHP 8.6 (latest patch)", "tool": "php", "source": {"type": "php", "series": "8.6"}},
        {"id": "nodejs-28", "label": "Node.js 28", "tool": "nodejs", "source": {"type": "node", "major": 28}}
    ]});
    fs::write(root.join("tmp/discovered.json"), doc.to_string()).unwrap();
    let ids: Vec<String> = devstack::packages::catalogue(&app).into_iter().map(|p| p.id).collect();
    let pos = |id: &str| ids.iter().position(|i| i == id).unwrap_or_else(|| panic!("{id} missing"));
    assert_eq!(pos("php-8.6") + 1, pos("php-8.5"), "newest PHP comes first");
    assert!(pos("nodejs-28") < pos("nodejs-26") && pos("nodejs-28") > pos("nodejs-lts"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn laravel_versions_come_from_packagist() {
    // Packagist's minified format: later entries only list what changed.
    let packagist = serde_json::json!({"minified": "composer/2.0", "packages": {"laravel/laravel": [
        {"version": "v14.0.0", "require": {"php": "^8.4"}},
        {"version": "v14.0.0-beta1"},
        {"version": "v13.11.0", "require": {"php": "^8.3"}},
        {"version": "v13.10.0"},
        {"version": "v8.6.12", "require": {"php": "^7.3|^8.0"}},
        {"version": "v6.20.1", "require": {"php": "^7.2.5|^8.0"}},
        {"version": "v5.5.28", "require": {"php": ">=7.0.0"}},
        {"version": "v5.4.30", "require": {"php": ">=5.6.4"}}
    ]}});
    let r = devstack::quickapp::laravel_recipes(&packagist);
    let names: Vec<&str> = r.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Laravel 14 (PHP 8.4+)", "Laravel 13 (PHP 8.3+)", "Laravel 8 (PHP 7.3+)", "Laravel 6 LTS (PHP 7.2+)", "Laravel 5.5 LTS (PHP 7.0+)"]);
    assert_eq!(r[0].package.as_deref(), Some("laravel/laravel:^14.0"));
    assert_eq!(r[4].package.as_deref(), Some("laravel/laravel:5.5.*"));

    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    fs::write(root.join("tmp/discovered.json"), serde_json::json!({"packages": [], "laravel": r}).to_string()).unwrap();
    let recipes: Vec<String> = devstack::quickapp::recipes(&app).into_iter().map(|r| r.name).collect();
    assert!(recipes.contains(&"Laravel 14 (PHP 8.4+)".to_string()) && recipes.contains(&"Laravel".to_string()));
    assert!(!recipes.contains(&"Laravel 10 (PHP 8.1+)".to_string()), "the built-in pinned list is replaced");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn projects_choose_their_own_versions() {
    let root = temp_root();
    let (_hosts_guard, fake_hosts) = fake_hosts(&root);
    let app = App::new(Some(root.clone())).unwrap();
    app.settings.set("updates.cacert", serde_json::json!(false)).unwrap();
    for v in ["php-8.4.14-ts", "php-7.4.33-ts"] {
        touch(&root.join("bin/php").join(v).join(exe("php-cgi")));
    }
    for v in ["mysql-8.4.13", "mysql-5.7.44"] {
        touch(&root.join("bin/mysql").join(v).join("bin").join(exe("mysqld")));
    }
    touch(&root.join("bin/nodejs/node-20.20.2").join(exe("node")));
    app.settings.set("versions.php", serde_json::json!("php-8.4.14-ts")).unwrap();
    fs::create_dir_all(root.join("www/legacy")).unwrap();
    touch(&root.join("www/legacy/composer.json"));
    touch(&root.join("www/legacy/package.json"));

    let mut versions = std::collections::BTreeMap::new();
    versions.insert("php".to_string(), "php-7.4.33-ts".to_string());
    versions.insert("mysql".to_string(), "mysql-5.7.44".to_string());
    app.save_project_versions("legacy", &versions, &|_| {}).unwrap();
    let site = app.sites().into_iter().find(|s| s.name == "legacy").unwrap();
    assert_eq!(site.versions, versions);
    assert_eq!(site.stack(), ["PHP", "Node"]);

    assert_eq!(app.active("php").unwrap().name, "php-8.4.14-ts", "no project chosen yet: defaults");
    app.set_project(Some("legacy"), &|_| {}).unwrap();
    assert_eq!(app.project().as_deref(), Some("legacy"));
    assert_eq!(app.active("php").unwrap().name, "php-7.4.33-ts");
    assert_eq!(app.active("mysql").unwrap().name, "mysql-5.7.44");
    assert_eq!(app.active("nodejs").unwrap().name, "node-20.20.2", "tools the project doesn't set keep the default");
    let ini = fs::read_to_string(root.join("bin/php/php-7.4.33-ts/php.ini")).unwrap();
    assert!(ini.contains("php-7.4.33-ts/ext"), "configs follow the project's PHP");

    // Picking a default version while the project decides PHP only changes the default.
    app.switch_version("php", "php-7.4.33-ts", &|_| {}).unwrap();
    assert_eq!(app.default_version("php").unwrap().name, "php-7.4.33-ts");
    app.switch_version("php", "php-8.4.14-ts", &|_| {}).unwrap();
    assert_eq!(app.active("php").unwrap().name, "php-7.4.33-ts");

    assert!(fs::read_to_string(&fake_hosts).unwrap().contains("127.0.0.1 legacy.test"));
    app.set_project(None, &|_| {}).unwrap();
    assert_eq!(app.project(), None);
    assert_eq!(app.active("php").unwrap().name, "php-8.4.14-ts");

    assert!(app.set_project(Some("nope"), &|_| {}).is_err(), "unknown project");
    versions.insert("php".to_string(), "php-5.6.40".to_string());
    app.save_project_versions("legacy", &versions, &|_| {}).unwrap();
    let err = app.set_project(Some("legacy"), &|_| {}).unwrap_err().to_string();
    assert!(err.contains("isn't installed"), "{err}");
    versions.insert("php".to_string(), "../../evil".to_string());
    assert!(app.save_project_versions("legacy", &versions, &|_| {}).is_err(), "unsafe names are refused");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn web_folder_can_be_another_folder_inside_the_project() {
    use devstack::sites::{clean_docroot, docroot_candidates, set_docroot};
    for (given, want) in [("another_folder", "another_folder"), ("src\\public\\", "src/public"), ("/web/", "web"), (".", "."), ("o'brien", "o'brien")] {
        assert_eq!(clean_docroot(given).as_deref(), Some(want), "{given}");
    }
    for bad in ["", "..", "../other", "a/../../b", "C:\\Windows", "a\"\nInclude evil.conf", "a\nb", "x;y", "a/$host", "a/./b"] {
        assert_eq!(clean_docroot(bad), None, "{bad:?} must be refused");
    }

    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    app.settings.set("updates.cacert", serde_json::json!(false)).unwrap();
    touch(&root.join("www/shop/another_folder/index.php"));
    touch(&root.join("www/shop/vendor/pkg/index.php"));
    touch(&root.join("www/shop/deep/web/index.html"));
    fs::create_dir_all(root.join("www/blog/public")).unwrap();
    touch(&root.join("www/blog/index.php"));
    let site = |name: &str| app.sites().into_iter().find(|s| s.name == name).unwrap();

    assert!(site("shop").docroot.ends_with("shop"), "the project folder stays the default");
    assert_eq!(docroot_candidates(&site("shop").path, 10), ["another_folder", "deep/web"], "nearest first, vendor skipped");

    set_docroot(&app, "shop", Some("another_folder")).unwrap();
    assert!(site("shop").docroot.ends_with("another_folder"));
    assert_eq!(site("shop").docroot_rel.as_deref(), Some("another_folder"));
    assert!(set_docroot(&app, "shop", Some("missing")).is_err(), "the folder has to exist");
    assert!(set_docroot(&app, "shop", Some("..")).is_err());
    assert!(site("shop").docroot.ends_with("another_folder"), "a refused change leaves the old one");

    // "." picks the project folder even when public/ exists.
    assert!(site("blog").docroot.ends_with("public"));
    set_docroot(&app, "blog", Some(".")).unwrap();
    assert!(site("blog").docroot.ends_with("blog"));
    set_docroot(&app, "blog", None).unwrap();
    assert!(site("blog").docroot.ends_with("public"));

    // A hand-written override that points outside the project is ignored.
    fs::write(root.join("usr/sites.json"), r#"{"shop": {"docroot": "../../Windows"}, "blog": {"docroot": "C:/Windows"}}"#).unwrap();
    assert!(site("shop").docroot.ends_with("shop") && site("blog").docroot.ends_with("public"));

    // The web servers get the chosen folder.
    touch(&root.join("bin/apache/apache-2.4.70/bin").join(exe("httpd")));
    set_docroot(&app, "shop", Some("another_folder")).unwrap();
    app.generate(&|_| {}).unwrap();
    assert!(fs::read_to_string(root.join("etc/apache/sites.conf")).unwrap().contains("/www/shop/another_folder\""));
    let _ = fs::remove_dir_all(root);
}

/// A folder that looks like a Laragon installation with PHP, two MySQL builds and data.
fn fake_laragon(dir: &Path) {
    touch(&dir.join("laragon.exe"));
    touch(&dir.join("bin/php/php-8.3.16-Win32-vs16-x64").join(exe("php-cgi")));
    touch(&dir.join("bin/php/php-8.3.16-Win32-vs16-x64/ext/php_curl.dll"));
    touch(&dir.join("bin/mysql/mysql-8.0.30-winx64/bin").join(exe("mysqld")));
    touch(&dir.join("bin/mysql/mysql-8.4.3-winx64/bin").join(exe("mysqld")));
    touch(&dir.join("bin/nodejs/node-v22").join(exe("node")));
    fs::create_dir_all(dir.join("data/mysql-8/mysql")).unwrap();
    fs::create_dir_all(dir.join("data/mysql-8/shopdb")).unwrap();
    fs::create_dir_all(dir.join("www/oldsite")).unwrap();
}

/// Every file under `dir` with its size, to prove nothing was written there.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.push((path, e.metadata().map(|m| m.len()).unwrap_or(0)));
        }
    }
    out.sort();
    out
}

#[test]
fn laragon_folders_are_used_in_place_and_never_written_to() {
    use devstack::laragon;
    use devstack::services::mysql_data_dir;
    let root = temp_root();
    let (_hosts_guard, _) = fake_hosts(&root);
    let lara = root.join("laragon");
    fake_laragon(&lara);
    let dev = root.join("devstack");
    let app = App::new(Some(dev.clone())).unwrap();
    app.settings.set("updates.cacert", serde_json::json!(false)).unwrap();
    let before = snapshot(&lara);

    assert!(app.versions("php").is_empty(), "nothing is used until it is turned on");
    assert!(laragon::detect(&app).is_some_and(|p| p.ends_with("laragon")), "a Laragon next to devstack's folder is found");
    assert!(laragon::use_all(&app, &root.join("nope"), &|_| {}).is_err(), "a folder without laragon.exe is refused");
    laragon::use_all(&app, &lara, &|_| {}).unwrap();

    // Projects, versions and databases now come from Laragon.
    assert_eq!(app.sites().into_iter().map(|s| s.name).collect::<Vec<_>>(), ["oldsite"]);
    let php = app.active("php").unwrap();
    assert!(php.external && php.name == "php-8.3.16-Win32-vs16-x64");
    assert_eq!(app.active("nodejs").unwrap().name, "node-v22");
    assert_eq!(app.active("mysql").unwrap().name, "mysql-8.0.30-winx64", "the build whose data folder Laragon has");
    let data = mysql_data_dir(&app, &app.active("mysql").unwrap());
    assert!(data.starts_with(&lara) && data.ends_with("mysql-8"), "Laragon calls 8.0's folder mysql-8: {}", data.display());

    // A series Laragon has no data for gets its folder in devstack's own data folder.
    app.settings.set("versions.mysql", serde_json::json!("mysql-8.4.3-winx64")).unwrap();
    let data84 = mysql_data_dir(&app, &app.active("mysql").unwrap());
    assert!(data84.starts_with(&app.paths.root) && data84.ends_with("mysql-8.4"), "{}", data84.display());

    // devstack's settings for Laragon's PHP live in devstack's folder.
    app.generate(&|_| {}).unwrap();
    assert!(dev.join("etc/php/php-8.3.16-Win32-vs16-x64/php.ini").is_file());
    let ini = fs::read_to_string(dev.join("etc/php/php-8.3.16-Win32-vs16-x64/php.ini")).unwrap();
    assert!(ini.contains("extension=curl") && ini.contains("laragon/bin/php/php-8.3.16-Win32-vs16-x64/ext"));

    // devstack's own newer MySQL may not open (and upgrade) Laragon's databases...
    touch(&dev.join("bin/mysql/mysql-8.0.46/bin").join(exe("mysqld")));
    let newer = app.versions("mysql").into_iter().find(|v| v.name == "mysql-8.0.46").unwrap();
    assert!(!newer.external);
    let laragon_data = lara.join("data/mysql-8");
    let err = laragon::check_data_version(&app, &newer, &laragon_data).unwrap_err().to_string();
    assert!(err.contains("upgrade") && err.contains("8.0.30"), "{err}");
    // ...Laragon's own build may, and so may anything once the user allows it.
    let theirs = app.versions("mysql").into_iter().find(|v| v.name == "mysql-8.0.30-winx64").unwrap();
    laragon::check_data_version(&app, &theirs, &laragon_data).unwrap();
    app.settings.set("laragon.allow_upgrade", serde_json::json!(true)).unwrap();
    laragon::check_data_version(&app, &newer, &laragon_data).unwrap();

    // Recover never moves Laragon's data folder.
    app.settings.set("versions.mysql", serde_json::json!("mysql-8.0.30-winx64")).unwrap();
    let err = devstack::database::MySqlTools::new(&app).recover(&|_| {}).unwrap_err().to_string();
    assert!(err.contains("belongs to Laragon"), "{err}");

    // Choosing a project with Laragon's PHP works like any other version.
    let mut versions = std::collections::BTreeMap::new();
    versions.insert("php".to_string(), "php-8.3.16-Win32-vs16-x64".to_string());
    app.save_project_settings("oldsite", None, &versions, &|_| {}).unwrap();
    app.set_project(Some("oldsite"), &|_| {}).unwrap();
    assert!(app.active("php").unwrap().external);

    assert_eq!(snapshot(&lara), before, "devstack must not create or change anything inside Laragon");

    laragon::stop_using(&app, &|_| {}).unwrap();
    assert!(app.versions("php").is_empty() && app.sites().is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generated_files_only_carry_what_devstack_wrote() {
    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    // A tampered tmp/discovered.json: a package with a download URL, a recipe with a command.
    let doc = serde_json::json!({
        "packages": [
            {"id": "php-8.6", "label": "PHP 8.6 (latest patch)", "tool": "php", "source": {"type": "php", "series": "8.6"}},
            {"id": "php-9.9", "label": "PHP 9.9", "tool": "php", "source": {"type": "url", "url": "https://evil.example/php.zip", "series": "9.9"}},
            {"id": "php-8.7", "label": "PHP 8.7", "tool": "php", "source": {"type": "php", "series": "8.7\"; x"}},
            {"id": "../x", "label": "x", "tool": "php", "source": {"type": "php", "series": "8.8"}}
        ],
        "laravel": [
            {"name": "Laravel 14 (PHP 8.4+)", "type": "composer", "package": "laravel/laravel:^14.0"},
            {"name": "Laravel 15 (PHP 8.4+)", "type": "command", "command": "calc.exe"},
            {"name": "Laravel 16 (PHP 8.4+)", "type": "composer", "package": "evil/package"},
            {"name": "Laravel 17 (PHP 8.4+)", "type": "composer", "package": "laravel/laravel:^17.0", "commands": ["calc.exe"]}
        ]
    });
    fs::write(root.join("tmp/discovered.json"), doc.to_string()).unwrap();
    let ids: Vec<String> = devstack::packages::catalogue(&app).into_iter().map(|p| p.id).collect();
    assert!(ids.contains(&"php-8.6".to_string()));
    for bad in ["php-9.9", "php-8.7", "../x"] {
        assert!(!ids.contains(&bad.to_string()), "{bad} must be ignored");
    }
    let names: Vec<String> = devstack::quickapp::recipes(&app).into_iter().map(|r| r.name).collect();
    assert!(names.contains(&"Laravel 14 (PHP 8.4+)".to_string()));
    for bad in ["Laravel 15 (PHP 8.4+)", "Laravel 16 (PHP 8.4+)", "Laravel 17 (PHP 8.4+)"] {
        assert!(!names.contains(&bad.to_string()), "{bad} must be ignored");
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn settings_cannot_smuggle_code_into_generated_configs() {
    // A folder with an apostrophe (C:\Users\O'Brien\...) and settings edited by hand.
    let base = temp_root();
    let root = base.join("o'brien");
    let app = App::new(Some(root.clone())).unwrap();
    app.settings.set("updates.cacert", serde_json::json!(false)).unwrap();
    app.settings.set("ports.mysql", serde_json::json!("3306'; system('calc'); //")).unwrap();
    app.settings.set("php.timezone", serde_json::json!("UTC\nauto_prepend_file=evil.php")).unwrap();
    app.settings.set("hostname_template", serde_json::json!("{name}.test'; system('calc'); //")).unwrap();
    touch(&root.join("bin/php/php-8.4.14-ts").join(exe("php-cgi")));
    touch(&root.join("bin/phpmyadmin/phpmyadmin-5.2.3/index.php"));
    touch(&root.join("bin/mysql/mysql-8.4.13/bin").join(exe("mysqld")));
    app.generate(&|_| {}).unwrap();

    let dashboard = root.join("etc/dashboard/index.php");
    let text = fs::read_to_string(&dashboard).unwrap();
    assert!(text.contains(r"o\'brien") && text.contains(r"{name}.test\'; system(\'calc\'); //"), "quotes are escaped");
    let pma = fs::read_to_string(root.join("bin/phpmyadmin/phpmyadmin-5.2.3/config.inc.php")).unwrap();
    assert!(pma.contains("['port'] = '3306';") && pma.contains(r"o\'brien"), "the port is a number again: {pma}");
    let ini = fs::read_to_string(root.join("bin/php/php-8.4.14-ts/php.ini")).unwrap();
    assert!(!ini.contains("\nauto_prepend_file"), "a setting can't add its own line");
    // If PHP is around, the generated PHP files must still parse.
    for file in [&dashboard, &root.join("bin/phpmyadmin/phpmyadmin-5.2.3/config.inc.php")] {
        if let Ok(out) = std::process::Command::new("php").arg("-l").arg(file).output() {
            assert!(out.status.success(), "php -l {}: {}", file.display(), String::from_utf8_lossy(&out.stdout));
        }
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn local_ca_can_only_vouch_for_local_names() {
    assert_eq!(ssl::zone_for("blog.test", "test"), "test");
    assert_eq!(ssl::zone_for("*.shop.local", "test"), "local");
    assert_eq!(ssl::zone_for("api.dev.acme.com", "dev.acme.com"), "dev.acme.com");
    assert_eq!(ssl::zone_for("intranet.acme.com", "test"), "intranet.acme.com", "never a whole public suffix");

    let root = temp_root();
    let app = App::new(Some(root.clone())).unwrap();
    let hosts = vec!["shop.test".to_string(), "*.shop.test".to_string()];
    assert!(!ssl::ensure_ca_covers(&app.paths, &hosts, "test").unwrap(), "first CA: nothing to trust again");
    assert_eq!(ssl::ca_zones(&app.paths).unwrap(), ["localhost", "test"]);
    let thumb = ssl::thumbprint(&app.paths).unwrap();
    assert!(!ssl::ensure_ca_covers(&app.paths, &hosts, "test").unwrap());
    assert_eq!(ssl::thumbprint(&app.paths).unwrap(), thumb, "covered hosts keep the CA");

    let ca = ssl::ca_files(&app.paths).0;
    let verify = |crt: &Path| std::process::Command::new("openssl").args(["verify", "-CAfile"]).arg(&ca).arg(crt).output();
    let (good, _, _) = ssl::ensure_cert(&app.paths, "shop.test", &hosts).unwrap();
    let (local, _, _) = ssl::ensure_cert(&app.paths, "localhost", &["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
    // What a thief with the CA key could try: a certificate for a real site.
    let (evil, _, _) = ssl::ensure_cert(&app.paths, "bank", &["www.bank.com".to_string()]).unwrap();
    if let Ok(out) = verify(&good) {
        assert!(out.status.success(), "shop.test: {}", String::from_utf8_lossy(&out.stderr));
        assert!(verify(&local).unwrap().status.success(), "localhost and 127.0.0.1 are allowed");
        let out = verify(&evil).unwrap();
        assert!(!out.status.success(), "a certificate for www.bank.com must not verify");
        let msg = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(msg.contains("permitted subtree violation"), "{msg}");
    }

    // A host in a new zone: the CA is made again (and must be trusted again), keeping the old zones.
    let more = vec!["shop.test".to_string(), "intranet.acme.com".to_string()];
    assert!(ssl::ensure_ca_covers(&app.paths, &more, "test").unwrap());
    assert_eq!(ssl::ca_zones(&app.paths).unwrap(), ["intranet.acme.com", "localhost", "test"]);
    assert_ne!(ssl::thumbprint(&app.paths).unwrap(), thumb);
    assert!(ssl::ensure_cert(&app.paths, "shop.test", &hosts).unwrap().2, "site certificates follow the new CA");

    // A CA from before constraints existed (no zone list) is replaced.
    fs::remove_file(root.join("etc/ssl/ca/devstack-ca.json")).unwrap();
    assert!(ssl::ensure_ca_covers(&app.paths, &hosts, "test").unwrap());
    assert!(ssl::ca_zones(&app.paths).is_some());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn windows_files_are_found_through_windows() {
    if cfg!(windows) {
        let dir = devstack::winutil::system_dir();
        assert!(dir.join("drivers").join("etc").join("hosts").is_file(), "{}", dir.display());
        assert!(dir.join("ipconfig.exe").is_file());
    }
}
