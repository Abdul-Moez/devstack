//! Everything that ships inside the executables: config templates, the Quick add catalogue,
//! Quick app recipes and icons. Users can override templates in `usr/templates/`.

macro_rules! templates {
    ($($name:literal),* $(,)?) => {
        pub const TEMPLATES: &[(&str, &str)] = &[
            $(($name, include_str!(concat!("../assets/templates/", $name)))),*
        ];
    };
}

templates!(
    "apache/httpd.conf.j2",
    "apache/sites.conf.j2",
    "nginx/nginx.conf.j2",
    "nginx/sites.conf.j2",
    "php/php.ini.j2",
    "mysql/my.ini.j2",
    "mysql/client.cnf.j2",
    "redis/redis.conf.j2",
    "phpmyadmin/config.inc.php.j2",
    "dashboard/index.php.j2",
);

pub const PACKAGES_JSON: &str = include_str!("../assets/packages.json");
pub const QUICKAPPS_JSON: &str = include_str!("../assets/quickapps.json");

// Icons are drawn by `cargo run --example make_icons` (examples/make_icons.rs).
/// The app icon (window, sidebar): 64x64 RGBA, Signal-orange status lights.
pub const ICON_BRAND: &[u8] = include_bytes!("../assets/icon-brand.rgba");
pub const ICON_BRAND_SIZE: u32 = 64;
/// Tray icons, 32x32 RGBA (drawn bolder for small sizes): green lights when something is
/// running, unlit when nothing is.
pub const ICON_RUNNING: &[u8] = include_bytes!("../assets/icon-running.rgba");
pub const ICON_STOPPED: &[u8] = include_bytes!("../assets/icon-stopped.rgba");
pub const TRAY_ICON_SIZE: u32 = 32;

pub fn template(name: &str) -> Option<&'static str> {
    TEMPLATES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}
