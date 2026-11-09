//! Small helpers shared by every module: paths, files, running commands, versions.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use regex::Regex;

pub const IS_WIN: bool = cfg!(windows);
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// "mysqld" -> "mysqld.exe" on Windows, unchanged elsewhere (lets the core run on Linux for tests).
/// Names that already have an extension ("index.php", "composer.phar", "Code.exe") stay as they are.
pub fn exe(name: &str) -> String {
    let has_ext = Path::new(name).extension().is_some();
    if IS_WIN && !has_ext {
        format!("{name}.exe")
    } else if !IS_WIN && name.to_lowercase().ends_with(".exe") {
        name[..name.len() - 4].to_string()
    } else {
        name.to_string()
    }
}

/// Forward-slash path for config files.
pub fn fwd(p: impl AsRef<Path>) -> String {
    p.as_ref().to_string_lossy().replace('\\', "/")
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn write_if_changed(path: &Path, text: &str) -> Result<bool> {
    if let Ok(cur) = fs::read(path) {
        if cur == text.as_bytes() {
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!("{}.tmp", file_name(path)));
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(true)
}

/// Copy a folder tree (regular files and folders only).
pub fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &dest).with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

pub fn read_text(path: &Path) -> String {
    fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

pub fn load_json(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Last `lines` lines of a log file.
pub fn tail(path: &Path, lines: usize) -> String {
    let Ok(mut f) = fs::File::open(path) else { return "(no log output)".into() };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(size.saturating_sub(16384)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A Command that never flashes a console window on Windows.
pub fn command<S: AsRef<OsStr>>(program: impl AsRef<OsStr>, args: impl IntoIterator<Item = S>) -> Command {
    let mut c = Command::new(program);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

pub struct CmdOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOut {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    /// stderr if there is any, else stdout (trimmed).
    pub fn message(&self) -> String {
        let s = if self.stderr.trim().is_empty() { &self.stdout } else { &self.stderr };
        s.trim().to_string()
    }
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<i32> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().unwrap_or(-1));
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("command timed out after {}s", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Run a short command and capture its output. Do NOT use for commands that leave background
/// processes behind (they inherit the pipes); use [`run_logged`] for those.
pub fn run(mut cmd: Command, timeout: Duration) -> Result<CmdOut> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("couldn't run {:?}", cmd.get_program()))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t1 = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t2 = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let code = wait_with_timeout(&mut child, timeout)?;
    let stdout = String::from_utf8_lossy(&t1.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&t2.join().unwrap_or_default()).into_owned();
    Ok(CmdOut { code, stdout, stderr })
}

/// Run a command with input on stdin and capture output.
pub fn run_input(mut cmd: Command, input: &str, timeout: Duration) -> Result<CmdOut> {
    use std::io::Write;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("couldn't run {:?}", cmd.get_program()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let data = input.to_string();
        thread::spawn(move || {
            let _ = stdin.write_all(data.as_bytes());
        });
    }
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t1 = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t2 = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let code = wait_with_timeout(&mut child, timeout)?;
    Ok(CmdOut {
        code,
        stdout: String::from_utf8_lossy(&t1.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&t2.join().unwrap_or_default()).into_owned(),
    })
}

/// Run a command with its output appended to a file instead of a pipe.
pub fn run_logged(mut cmd: Command, log_path: &Path, timeout: Duration) -> Result<i32> {
    if let Some(p) = log_path.parent() {
        fs::create_dir_all(p)?;
    }
    let f = fs::OpenOptions::new().create(true).append(true).open(log_path)?;
    let f2 = f.try_clone()?;
    cmd.stdin(Stdio::null()).stdout(f).stderr(f2);
    let mut child = cmd.spawn().with_context(|| format!("couldn't run {:?}", cmd.get_program()))?;
    wait_with_timeout(&mut child, timeout)
}

/// Run a command and pass every output line to `log`. Fails on a non-zero exit code.
pub fn run_stream(mut cmd: Command, log: crate::Log) -> Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("couldn't run {:?}", cmd.get_program()))?;
    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
                 child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        }));
    }
    drop(tx);
    for line in rx {
        let line = line.trim_end();
        if !line.is_empty() {
            log(line);
        }
    }
    for r in readers {
        let _ = r.join();
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("command failed with exit code {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d+(?:\.\d+)+)").unwrap())
}

pub fn parse_version(s: &str) -> Option<String> {
    version_re().captures(s).map(|c| c[1].to_string())
}

pub fn version_key(s: &str) -> Vec<u64> {
    parse_version(s)
        .map(|v| v.split('.').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_default()
}

pub fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn valid_name(name: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$").unwrap()).is_match(name)
}

/// A single file or folder name that is safe to join onto a path: no separators, no `..`.
/// Used for names that come from download sources (version strings, asset names).
pub fn safe_file_name(name: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,119}$").unwrap()).is_match(name) && !name.contains("..")
}

/// Environment for a child process: current env plus overrides. On Windows variable names
/// are case-insensitive, so an override replaces any spelling of the same name (the
/// inherited `Path` must not survive next to our `PATH`; which one wins would be random).
pub fn env_with(extra: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    for (k, v) in extra {
        if IS_WIN {
            env.retain(|old, _| !old.eq_ignore_ascii_case(k));
        }
        env.insert(k.clone(), v.clone());
    }
    env
}

pub fn path_sep() -> &'static str {
    if IS_WIN { ";" } else { ":" }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Random hex string from the OS's secure RNG (via a throwaway key pair).
pub fn random_hex(len: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut out = String::new();
    while out.len() < len {
        let kp = rcgen::KeyPair::generate().expect("key generation");
        out.push_str(&hex(&Sha256::digest(kp.serialize_der())));
    }
    out.truncate(len);
    out
}
