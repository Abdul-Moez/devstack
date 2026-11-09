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
