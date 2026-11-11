//! Keeps one marked block in the Windows hosts file in sync with your sites. Nothing outside
//! the block is ever touched. Writing needs admin, so it goes through a UAC prompt, and only
//! when the block actually changed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;

use crate::core::App;
use crate::util::{timestamp, IS_WIN};
use crate::{winutil, Log};

pub const BEGIN: &str = "# BEGIN devstack";
pub const END: &str = "# END devstack";

pub fn hosts_path() -> PathBuf {
    if let Ok(p) = std::env::var("DEVSTACK_HOSTS") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    system_hosts_path()
}

fn system_hosts_path() -> PathBuf {
    if IS_WIN {
        winutil::system_dir().join("drivers").join("etc").join("hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

/// True if every line inside our block is `127.0.0.1 <host name>` (or there is no block).
/// The elevated helper refuses anything else, so a tampered staging file can't point real
/// domains at another address.
pub fn block_is_safe(text: &str) -> bool {
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(BEGIN) {
            inside = true;
        } else if line.starts_with(END) {
            inside = false;
        } else if inside && !line.strip_prefix("127.0.0.1 ").is_some_and(crate::sites::valid_host) {
            return false;
        }
    }
    !inside
}

/// Read with line endings normalized to \n.
pub fn read(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let text = String::from_utf8(bytes).context("the hosts file isn't UTF-8 text; not touching it")?;
    Ok(text.replace("\r\n", "\n"))
}

pub fn write(path: &Path, text: &str) -> Result<()> {
    let out = if IS_WIN { text.replace('\n', "\r\n") } else { text.to_string() };
    fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

pub fn build_block(hosts: &[String]) -> String {
    if hosts.is_empty() {
        return String::new();
    }
    let mut sorted: Vec<&String> = hosts.iter().collect();
    sorted.sort();
    sorted.dedup();
    let mut lines = vec![format!("{BEGIN} (managed automatically; edits inside this block are overwritten)")];
    lines.extend(sorted.iter().map(|h| format!("127.0.0.1 {h}")));
    lines.push(END.to_string());
    lines.join("\n")
}

pub fn with_block(text: &str, block: &str) -> String {
    let re = Regex::new(&format!(r"(?ms)\n*^{}.*?^{}[^\n]*\n?", regex::escape(BEGIN), regex::escape(END))).unwrap();
    let replaced = re.replace_all(text, "\n");
    let base = replaced.trim_end_matches('\n');
    if block.is_empty() {
        return format!("{base}\n");
    }
    if base.is_empty() {
        format!("{block}\n")
    } else {
        format!("{base}\n\n{block}\n")
    }
}

fn backup(app: &App, path: &Path) -> Result<()> {
    let dest = app.paths().backup.join("hosts");
    fs::create_dir_all(&dest)?;
    fs::copy(path, dest.join(format!("hosts-{}.bak", timestamp())))?;
    let mut olds: Vec<_> = fs::read_dir(&dest)?.flatten().map(|e| e.path()).collect();
    olds.sort();
    let n = olds.len();
    for old in olds.into_iter().take(n.saturating_sub(10)) {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

/// Make the hosts file contain exactly these hosts in our block. Returns true if it changed.
pub fn sync(app: &App, hosts: &[String], log: Log) -> Result<bool> {
    let path = hosts_path();
    let current = read(&path)?;
    let new = with_block(&current, &build_block(hosts));
    if new == current {
        return Ok(false);
    }
    let direct = fs::OpenOptions::new().append(true).open(&path).is_ok();
    if direct {
        backup(app, &path)?;
        write(&path, &new)?;
    } else {
        // The backup is made here, without admin rights; the helper only writes the hosts file.
        backup(app, &path)?;
        let staged = app.paths().tmp.join("hosts.new");
        fs::write(&staged, &new)?;
        log("Updating the hosts file (Windows will ask for permission)…");
        let code = winutil::run_elevated(&winutil::self_command(&["_elevated", "hosts", &staged.display().to_string()]))?;
        if code != 0 {
            bail!("Hosts file was not updated (permission denied or cancelled).");
        }
    }
    log(&format!("Hosts file updated ({} site{})", hosts.len(), if hosts.len() == 1 { "" } else { "s" }));
    Ok(true)
}

/// Runs inside the elevated helper process, so it does one thing and trusts nothing it is
/// given: it reads the staged file, refuses changes outside devstack's block and block lines
/// that aren't `127.0.0.1 <host>`, and writes the real hosts file. It never loads settings,
/// creates folders or writes anywhere in devstack's own (user-writable) folder, and it finds
/// Windows' files through the system, not through environment variables or PATH.
pub fn apply_staged(staged: &Path) -> Result<i32> {
    let path = if IS_WIN { system_hosts_path() } else { hosts_path() };
    if fs::metadata(staged)?.len() > 1024 * 1024 {
        return Ok(4);
    }
    let new = fs::read_to_string(staged)?.replace("\r\n", "\n");
    if with_block(&new, "") != with_block(&read(&path)?, "") {
        return Ok(2);
    }
    if !block_is_safe(&new) {
        return Ok(3);
    }
    write(&path, &new)?;
    if IS_WIN {
        let ipconfig = winutil::system_dir().join("ipconfig.exe");
        let _ = crate::util::run(crate::util::command(ipconfig, ["/flushdns"]), std::time::Duration::from_secs(15));
    }
    Ok(0)
}
