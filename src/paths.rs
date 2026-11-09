//! Every folder derives from the app root (the folder that holds devstack.exe), so the whole
//! installation can be moved anywhere.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

pub fn find_root() -> PathBuf {
    if let Ok(env) = std::env::var("DEVSTACK_ROOT") {
        if !env.is_empty() {
            let p = PathBuf::from(env);
            let _ = fs::create_dir_all(&p);
            return p.canonicalize().map(strip_verbatim).unwrap_or(p);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok())
        .map(strip_verbatim)
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// canonicalize() on Windows returns \\?\C:\... which some programs (Apache, MySQL) dislike.
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if !rest.starts_with("UNC\\") {
            return PathBuf::from(rest);
        }
    }
    p
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub bin: PathBuf,
    pub etc: PathBuf,
    pub data: PathBuf,
    pub logs: PathBuf,
    pub tmp: PathBuf,
    pub usr: PathBuf,
    pub backup: PathBuf,
    pub ssl: PathBuf,
    pub www: PathBuf,
    pub settings_file: PathBuf,
}

impl Paths {
    pub fn new(root: PathBuf) -> Self {
        let etc = root.join("etc");
        Paths {
            bin: root.join("bin"),
            ssl: etc.join("ssl"),
            etc,
            data: root.join("data"),
            logs: root.join("logs"),
            tmp: root.join("tmp"),
            usr: root.join("usr"),
            backup: root.join("backup"),
            www: root.join("www"),
            settings_file: root.join("settings.json"),
            root,
        }
    }

    pub fn set_www(&mut self, custom: &str) {
        self.www = if custom.trim().is_empty() { self.root.join("www") } else { PathBuf::from(custom.trim()) };
    }

    pub fn ensure(&self) -> Result<()> {
        for d in [&self.bin, &self.etc, &self.data, &self.logs, &self.tmp, &self.usr, &self.backup, &self.www, &self.ssl] {
            fs::create_dir_all(d)?;
        }
        for d in ["apache", "nginx", "templates"] {
            fs::create_dir_all(self.usr.join(d))?;
        }
        fs::create_dir_all(self.tmp.join("phpmyadmin"))?;
        let starters = [
            ("php.ini", "; Your php.ini additions. Appended to every generated php.ini.\n"),
            ("my.ini", "# Your MySQL [mysqld] additions. Appended to the generated my.ini.\n"),
            ("sites.json", "{\n}\n"),
        ];
        for (name, text) in starters {
            let p = self.usr.join(name);
            if !p.exists() {
                fs::write(p, text)?;
            }
        }
        Ok(())
    }
}
