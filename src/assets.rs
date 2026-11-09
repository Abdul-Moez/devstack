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

/// 64x64 RGBA icons (window + tray).
pub const ICON_GREEN: &[u8] = include_bytes!("../assets/icon-green.rgba");
pub const ICON_GREY: &[u8] = include_bytes!("../assets/icon-grey.rgba");
pub const ICON_SIZE: u32 = 64;

pub fn template(name: &str) -> Option<&'static str> {
    TEMPLATES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}
