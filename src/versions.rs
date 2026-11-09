//! Installed versions live in bin/<tool>/<folder>/. Any folder that contains the tool's
//! executable counts, so you can also extract a zip there by hand.

use std::fs;
use std::path::PathBuf;

use crate::util::{exe, parse_version, version_key};

/// tool -> (folder under bin/, executables to look for inside each version folder)
pub const TOOLS: &[(&str, &str, &[&str])] = &[
    ("apache", "apache", &["bin/httpd.exe"]),
    ("nginx", "nginx", &["nginx.exe"]),
    ("php", "php", &["php-cgi.exe"]),
    ("mysql", "mysql", &["bin/mysqld.exe", "bin/mariadbd.exe"]),
    ("postgresql", "postgresql", &["bin/postgres.exe"]),
    ("mongodb", "mongodb", &["bin/mongod.exe"]),
    ("redis", "redis", &["redis-server.exe"]),
    ("memcached", "memcached", &["memcached.exe"]),
    ("mailpit", "mailpit", &["mailpit.exe"]),
    ("nodejs", "nodejs", &["node.exe"]),
    ("python", "python", &["python.exe"]),
    ("composer", "composer", &["composer.phar"]),
    ("git", "git", &["cmd/git.exe"]),
    ("go", "go", &["bin/go.exe"]),
    ("ngrok", "ngrok", &["ngrok.exe"]),
    ("phpmyadmin", "phpmyadmin", &["index.php"]),
    ("heidisql", "heidisql", &["heidisql.exe"]),
    ("dbeaver", "dbeaver", &["dbeaver.exe"]),
    ("vscode", "vscode", &["Code.exe"]),
    ("pocketbase", "pocketbase", &["pocketbase.exe"]),
];

/// Executable paths to look for, adjusted for the current OS.
pub fn candidates(tool: &str) -> Vec<String> {
    TOOLS
        .iter()
        .find(|(t, _, _)| *t == tool)
        .map(|(_, _, c)| {
            c.iter()
                .map(|p| match p.rsplit_once('/') {
                    Some((head, name)) => format!("{head}/{}", exe(name)),
                    None => exe(p),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn folder(tool: &str) -> &'static str {
    TOOLS.iter().find(|(t, _, _)| *t == tool).map(|(_, f, _)| *f).unwrap_or("unknown")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    pub tool: String,
    pub name: String,
    pub path: PathBuf,
    pub exe: PathBuf,
}

impl Version {
    pub fn number(&self) -> String {
        parse_version(&self.name).unwrap_or_else(|| self.name.clone())
    }
    pub fn key(&self) -> Vec<u64> {
        version_key(&self.name)
    }
    pub fn series(&self) -> String {
        self.number().split('.').take(2).collect::<Vec<_>>().join(".")
    }
    pub fn major(&self) -> String {
        self.number().split('.').next().unwrap_or("").to_string()
    }
}

pub fn root(bin: &std::path::Path, tool: &str) -> PathBuf {
    bin.join(folder(tool))
}

pub fn list(bin: &std::path::Path, tool: &str) -> Vec<Version> {
    let dir = root(bin, tool);
    let Ok(entries) = fs::read_dir(&dir) else { return vec![] };
    let cands = candidates(tool);
    let mut found: Vec<Version> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let path = e.path();
            cands.iter().map(|c| path.join(c)).find(|p| p.exists()).map(|exe| Version {
                tool: tool.to_string(),
                name: e.file_name().to_string_lossy().into_owned(),
                path: path.clone(),
                exe,
            })
        })
        .collect();
    found.sort_by(|a, b| (b.key(), &b.name).cmp(&(a.key(), &a.name)));
    found
}
