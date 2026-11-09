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
    assert!(fs::read_to_string(root.join("etc/apache/sites.conf")).unwrap().contains("ServerName blog.test"));
    assert!(fs::read_to_string(root.join("etc/nginx/nginx.conf")).unwrap().contains("server 127.0.0.1:10987;"));
    let nginx_sites = fs::read_to_string(root.join("etc/nginx/sites.conf")).unwrap();
    assert!(nginx_sites.contains("server_name blog.test *.blog.test;"));
    let ini = fs::read_to_string(php.join("php.ini")).unwrap();
    assert!(ini.contains("extension=curl") && ini.contains("zend_extension=opcache") && !ini.contains("zend_extension=xdebug"));
    assert!(app.generate(&|_| {}).unwrap().is_empty(), "second generate should change nothing");
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
