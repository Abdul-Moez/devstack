//! Process spawning, liveness checks and port helpers.
//!
//! Services run detached (no console window, own process group) and are tracked by PID +
//! start time in tmp/state.json, so the GUI and the CLI control the same services and they
//! keep running if the GUI closes.

use std::collections::HashMap;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

use crate::settings::ProcRec;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

static CHILDREN: Mutex<Vec<Child>> = Mutex::new(Vec::new());

/// One process list shared by every liveness check. Taking a snapshot lists every process on
/// the machine, and the GUI checks a dozen PIDs every refresh, so snapshots are reused for
/// a short time. Spawning or killing clears it so those changes are seen immediately.
static SNAPSHOT: Mutex<Option<(Instant, System)>> = Mutex::new(None);
const SNAPSHOT_TTL: Duration = Duration::from_millis(500);

fn invalidate_snapshot() {
    *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn with_snapshot<R>(f: impl FnOnce(&System) -> R) -> R {
    let mut guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = guard.as_ref().is_some_and(|(at, _)| at.elapsed() < SNAPSHOT_TTL);
    if !fresh {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
        *guard = Some((Instant::now(), sys));
    }
    f(&guard.as_ref().expect("snapshot was just taken").1)
}

fn reap() {
    let mut kids = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
    kids.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
}

fn rotate(log: &Path) {
    if fs::metadata(log).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let old = log.with_extension("log.1");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(log, old);
    }
}

/// Start a background process with stdout/stderr appended to `log`.
pub fn spawn(args: &[String], cwd: &Path, env: Option<&HashMap<String, String>>, log: &Path) -> Result<ProcRec> {
    if let Some(p) = log.parent() {
        fs::create_dir_all(p)?;
    }
    rotate(log);
    let mut f = fs::OpenOptions::new().create(true).append(true).open(log)?;
    {
        use std::io::Write;
        let name = Path::new(&args[0]).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let _ = writeln!(f, "\n===== {} starting {name}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
    let f2 = f.try_clone()?;
    let build = |extra_flags: u32| {
        let mut c = Command::new(&args[0]);
        c.args(&args[1..]).current_dir(cwd).stdin(Stdio::null());
        if let Some(env) = env {
            c.env_clear().envs(env);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            c.creation_flags(crate::util::CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | extra_flags);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let _ = extra_flags;
            c.process_group(0);
        }
        c
    };
    #[cfg(windows)]
    let breakaway: u32 = 0x0100_0000; // CREATE_BREAKAWAY_FROM_JOB
    #[cfg(not(windows))]
    let breakaway: u32 = 0;
    let mut cmd = build(breakaway);
    cmd.stdout(f.try_clone()?).stderr(f2.try_clone()?);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) if breakaway != 0 => {
            let mut c = build(0);
            c.stdout(f).stderr(f2);
            c.spawn().with_context(|| format!("couldn't start {}", args[0]))?
        }
        Err(e) => return Err(e).with_context(|| format!("couldn't start {}", args[0])),
    };
    let rec = record(child.id());
    CHILDREN.lock().unwrap_or_else(|e| e.into_inner()).push(child);
    Ok(rec)
}

/// Track a process that was just started (always looks at a fresh process list).
pub fn record(pid: u32) -> ProcRec {
    invalidate_snapshot();
    let ctime = with_snapshot(|sys| sys.process(Pid::from_u32(pid)).map(sysinfo::Process::start_time)).unwrap_or(0);
    ProcRec { pid, ctime, ..Default::default() }
}

pub fn alive(rec: &ProcRec) -> bool {
    reap();
    with_snapshot(|sys| match sys.process(Pid::from_u32(rec.pid)) {
        Some(p) => {
            if rec.ctime != 0 && p.start_time().abs_diff(rec.ctime) > 2 {
                return false; // PID was reused by another program
            }
            !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead)
        }
        None => false,
    })
}

/// Terminate a process and all its children.
pub fn kill_tree(pid: u32, timeout: Duration) {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let mut tree = vec![Pid::from_u32(pid)];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        // A child can't be older than its parent. An older process pointing at this PID is
        // an orphan of whatever program used the PID before, so leave it alone.
        let born = sys.process(parent).map_or(0, sysinfo::Process::start_time);
        for (cpid, p) in sys.processes() {
            if p.parent() == Some(parent) && p.start_time() >= born && !tree.contains(cpid) {
                tree.push(*cpid);
            }
        }
        i += 1;
    }
    for p in &tree {
        if let Some(proc_) = sys.process(*p) {
            #[cfg(unix)]
            {
                if proc_.kill_with(sysinfo::Signal::Term).is_none() {
                    proc_.kill();
                }
            }
            #[cfg(not(unix))]
            {
                proc_.kill();
            }
        }
    }
    invalidate_snapshot();
    // Remember start times so a PID reused while we wait isn't mistaken for our process.
    let recs: Vec<ProcRec> = tree
        .iter()
        .map(|p| ProcRec { pid: p.as_u32(), ctime: sys.process(*p).map_or(0, sysinfo::Process::start_time), ..Default::default() })
        .collect();
    if !wait_exit(&recs, timeout) {
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
        for r in &recs {
            if let Some(proc_) = sys.process(Pid::from_u32(r.pid)).filter(|p| r.ctime == 0 || p.start_time().abs_diff(r.ctime) <= 2) {
                proc_.kill();
            }
        }
        invalidate_snapshot();
        wait_exit(&recs, Duration::from_secs(3));
    }
    invalidate_snapshot();
    reap();
}

pub fn wait_exit(recs: &[ProcRec], timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !recs.iter().any(alive) {
            return true;
        }
        thread::sleep(Duration::from_millis(250));
    }
    !recs.iter().any(alive)
}

pub fn port_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// port -> pid for every listening TCP socket (best effort).
pub fn listening() -> HashMap<u16, u32> {
    use netstat2::{get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};
    let mut out = HashMap::new();
    if let Ok(sockets) = get_sockets_info(AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6, ProtocolFlags::TCP) {
        for s in sockets {
            if let ProtocolSocketInfo::Tcp(tcp) = &s.protocol_socket_info {
                if tcp.state == TcpState::Listen {
                    out.entry(tcp.local_port).or_insert_with(|| s.associated_pids.first().copied().unwrap_or(0));
                }
            }
        }
    }
    out
}

pub fn process_name(pid: u32) -> Option<String> {
    with_snapshot(|sys| sys.process(Pid::from_u32(pid)).map(|p| p.name().to_string_lossy().into_owned()))
}

/// Who is listening on `port`, or None if it's free.
pub fn port_owner(port: u16, table: &HashMap<u16, u32>) -> Option<String> {
    if let Some(&pid) = table.get(&port) {
        if pid == 0 {
            return Some("another program".into());
        }
        let name = process_name(pid).unwrap_or_else(|| "unknown program".into());
        let hint = if pid == 4 { " (Windows HTTP service: usually IIS / 'World Wide Web Publishing Service')" } else { "" };
        return Some(format!("{name} (PID {pid}){hint}"));
    }
    if port_open(port) {
        return Some("another program".into());
    }
    None
}
