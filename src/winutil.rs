//! Windows integration: UAC elevation, user PATH, autostart, junctions, showing/hiding the
//! window, single instance, opening things. Every Win32 call in the project lives here.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

pub const APP_RUN_NAME: &str = "devstack";

/// Command line that re-runs this executable (used for the UAC helper).
pub fn self_command(args: &[&str]) -> Vec<String> {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "devstack".into());
    std::iter::once(exe).chain(args.iter().map(|s| s.to_string())).collect()
}

/// Quote one argument the way CommandLineToArgvW expects.
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The Windows system folder (C:\Windows\System32), asked from Windows itself. The admin
/// helper uses this instead of %SystemRoot% or PATH, which whoever started it could have set.
#[cfg(windows)]
pub fn system_dir() -> PathBuf {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buf = [0u16; 512];
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n > 0 && n < buf.len() {
        PathBuf::from(String::from_utf16_lossy(&buf[..n]))
    } else {
        PathBuf::from(r"C:\Windows\System32")
    }
}

#[cfg(not(windows))]
pub fn system_dir() -> PathBuf {
    PathBuf::from("/usr/bin")
}

/// Stop child processes from inheriting this process's own stdin/stdout/stderr. Services are
/// started with their output going to a log file, but Windows hands a child every inheritable
/// handle the parent has, including the pipe `ds start | more` writes to; the service then
/// keeps that pipe open and whatever reads it waits until the service stops. Children that
/// are meant to share our output still do: Rust passes them their own copies.
#[cfg(windows)]
pub fn keep_std_handles_private() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    let handles = [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()];
    for h in handles {
        if !h.is_null() {
            unsafe {
                SetHandleInformation(h as _, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn keep_std_handles_private() {}

// ------------------------------------------------------------------ elevation
/// Run a command through a UAC prompt and wait for it. Returns its exit code (1223 if the
/// user clicked No).
#[cfg(windows)]
pub fn run_elevated(args: &[String]) -> Result<i32> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let verb = wide("runas");
    let file = wide(&args[0]);
    let params = wide(&args[1..].iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" "));
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 {
            return Ok(GetLastError() as i32);
        }
        WaitForSingleObject(info.hProcess, 120_000);
        let mut code: u32 = 1;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        Ok(code as i32)
    }
}

#[cfg(not(windows))]
pub fn run_elevated(args: &[String]) -> Result<i32> {
    let status = Command::new(&args[0]).args(&args[1..]).status()?;
    Ok(status.code().unwrap_or(1))
}

// ------------------------------------------------------------------ environment / registry
#[cfg(windows)]
pub fn broadcast_env_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE};
    let env = wide("Environment");
    let mut result: usize = 0;
    unsafe {
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, env.as_ptr() as isize, SMTO_ABORTIFHUNG, 5000, &mut result);
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn norm(p: &str) -> String {
    p.trim().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase()
}

/// Put `add` at the front of the user PATH and drop `remove`.
#[cfg(windows)]
pub fn update_user_path(add: &[PathBuf], remove: &[PathBuf]) -> Result<()> {
    use winreg::enums::{RegType, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::{RegKey, RegValue};
    let env = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
    let current: String = env.get_value("Path").unwrap_or_default();
    let kind = env.get_raw_value("Path").map(|v| v.vtype).unwrap_or(RegType::REG_EXPAND_SZ);
    let drop: Vec<String> = add.iter().chain(remove).map(|p| norm(&p.display().to_string())).collect();
    let mut parts: Vec<String> = add.iter().map(|p| p.display().to_string()).collect();
    parts.extend(current.split(';').filter(|x| !x.trim().is_empty() && !drop.contains(&norm(x))).map(str::to_string));
    let joined = parts.join(";");
    let bytes: Vec<u8> = joined.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect();
    let vtype = if matches!(kind, RegType::REG_SZ) { RegType::REG_SZ } else { RegType::REG_EXPAND_SZ };
    env.set_raw_value("Path", &RegValue { bytes, vtype })?;
    broadcast_env_change();
    Ok(())
}

#[cfg(not(windows))]
pub fn update_user_path(_add: &[PathBuf], _remove: &[PathBuf]) -> Result<()> {
    anyhow::bail!("Editing PATH is only supported on Windows.")
}

#[cfg_attr(not(windows), allow(dead_code))]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
pub fn set_autostart(command: Option<&str>) -> Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY)?;
    match command {
        Some(c) => key.set_value(APP_RUN_NAME, &c.to_string())?,
        None => {
            let _ = key.delete_value(APP_RUN_NAME);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_autostart(_command: Option<&str>) -> Result<()> {
    Ok(())
}

/// Installed Visual C++ 2015+ runtime (x64) as (major, minor, build), from the registry key
/// its installer writes.
#[cfg(windows)]
pub fn vc_runtime_version() -> Option<(u32, u32, u32)> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\X64", KEY_READ | KEY_WOW64_64KEY)
        .ok()?;
    if key.get_value::<u32, _>("Installed").ok()? != 1 {
        return None;
    }
    Some((key.get_value("Major").ok()?, key.get_value("Minor").ok()?, key.get_value("Bld").ok()?))
}

#[cfg(not(windows))]
pub fn vc_runtime_version() -> Option<(u32, u32, u32)> {
    None
}

/// Whether the built-in groups that include every account on the PC (Users, Authenticated
/// Users, Everyone) may write to `path`. None if it couldn't be checked.
#[cfg(windows)]
pub fn others_can_write(path: &Path) -> Option<bool> {
    // The path travels in an environment variable, so nothing in it is read as script.
    const SCRIPT: &str = "$r=[System.Security.AccessControl.FileSystemRights]; $w=[int]($r::WriteData -bor $r::AppendData); \
        $n=@((Get-Acl -LiteralPath $env:DS_ACL_PATH).Access | Where-Object { $_.AccessControlType -eq 'Allow' -and \
        (@('S-1-5-11','S-1-5-32-545','S-1-1-0') -contains $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value) -and \
        (([int]$_.FileSystemRights -band $w) -ne 0) }).Count; Write-Output $n";
    let powershell = system_dir().join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    let mut c = crate::util::command(powershell, ["-NoProfile", "-NonInteractive", "-Command", SCRIPT]);
    c.env("DS_ACL_PATH", path);
    let out = crate::util::run(c, std::time::Duration::from_secs(20)).ok()?;
    if !out.ok() {
        return None;
    }
    out.stdout.trim().parse::<u32>().ok().map(|n| n > 0)
}

#[cfg(not(windows))]
pub fn others_can_write(_path: &Path) -> Option<bool> {
    None
}

pub fn gui_command(root: &Path) -> String {
    format!("{} --hidden", quote_arg(&root.join(crate::util::exe("devstack")).display().to_string()))
}

#[cfg(windows)]
pub fn is_admin() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

#[cfg(not(windows))]
pub fn is_admin() -> bool {
    false
}

// ------------------------------------------------------------------ junctions
pub fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
        || {
            #[cfg(windows)]
            {
                junction::exists(p).unwrap_or(false)
            }
            #[cfg(not(windows))]
            {
                false
            }
        }
}

/// Directory junction on Windows (no admin needed), symlink elsewhere.
pub fn make_link(link: &Path, target: &Path) -> Result<()> {
    remove_link(link)?;
    if link.exists() {
        return Ok(()); // a real folder is in the way; never delete user data
    }
    if let Some(p) = link.parent() {
        std::fs::create_dir_all(p)?;
    }
    #[cfg(windows)]
    junction::create(target, link).with_context(|| format!("creating junction {}", link.display()))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).with_context(|| format!("creating link {}", link.display()))?;
    Ok(())
}

pub fn remove_link(link: &Path) -> Result<()> {
    if std::fs::symlink_metadata(link).is_ok() && is_link(link) {
        #[cfg(windows)]
        std::fs::remove_dir(link)?; // removes the junction only, never the target
        #[cfg(unix)]
        std::fs::remove_file(link)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ window helpers
/// Brings the window back, also from where `hide_window` parked it.
#[cfg(windows)]
pub fn show_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, IsIconic, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE, SW_HIDE, SW_RESTORE, SW_SHOW,
        WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };
    unsafe {
        let h = hwnd as windows_sys::Win32::Foundation::HWND;
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        if ex & WS_EX_TOOLWINDOW as isize != 0 {
            // parked: give it its taskbar button back (the taskbar only notices while hidden)
            ShowWindow(h, SW_HIDE);
            SetWindowLongPtrW(h, GWL_EXSTYLE, (ex & !(WS_EX_TOOLWINDOW as isize)) | WS_EX_APPWINDOW as isize);
        }
        // SW_RESTORE only when minimized: on an open maximized window it would un-maximize it
        ShowWindow(h, if IsIconic(h) != 0 { SW_RESTORE } else { SW_SHOW });
        SetForegroundWindow(h);
    }
}

/// "Hide to the tray". Not SW_HIDE: a hidden window gets no WM_PAINT, so eframe stops
/// drawing frames. It then spins a CPU core asking for redraws, and anything handed to the
/// window waits forever (Exit from the tray with services running never happened). The
/// window is parked instead as a minimized tool window off-screen: no taskbar button, not in
/// Alt+Tab, still drawing about once a second. winit leaves the styles of a minimized window
/// alone, and `show_window` puts them back.
#[cfg(windows)]
pub fn hide_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        SW_HIDE, SW_SHOWMINNOACTIVE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };
    unsafe {
        let h = hwnd as windows_sys::Win32::Foundation::HWND;
        ShowWindow(h, SW_HIDE); // the taskbar only notices a style change while the window is hidden
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, (ex | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize));
        ShowWindow(h, SW_SHOWMINNOACTIVE);
        // a minimized tool window sits above the taskbar; move it to where minimized windows go
        SetWindowPos(h, std::ptr::null_mut(), -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Makes Windows send the window a real WM_PAINT. eframe draws only when winit turns a
/// WM_PAINT into RedrawRequested; winit asks for one with RDW_INTERNALPAINT, and that request
/// can get lost around the tray menu's modal loop. eframe then spins a CPU core waiting for
/// it and draws nothing until the window is invalidated. Safe from any thread.
#[cfg(windows)]
pub fn repaint_window(hwnd: isize) {
    use windows_sys::Win32::Graphics::Gdi::{RedrawWindow, RDW_INTERNALPAINT, RDW_INVALIDATE};
    unsafe {
        RedrawWindow(hwnd as windows_sys::Win32::Foundation::HWND, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_INTERNALPAINT);
    }
}

#[cfg(not(windows))]
pub fn repaint_window(_hwnd: isize) {}

/// True while this thread shows a popup menu (the tray menu runs a modal loop on the UI thread).
#[cfg(windows)]
pub fn menu_open() -> bool {
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO, GUI_INMENUMODE, GUI_POPUPMENUMODE};
    unsafe {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        GetGUIThreadInfo(GetCurrentThreadId(), &mut info) != 0 && info.flags & (GUI_POPUPMENUMODE | GUI_INMENUMODE) != 0
    }
}

#[cfg(not(windows))]
pub fn menu_open() -> bool {
    false
}

#[cfg(not(windows))]
pub fn show_window(_hwnd: isize) {}
#[cfg(not(windows))]
pub fn hide_window(_hwnd: isize) {}

/// Returns false if another devstack from the same folder is already running (and brings
/// its window to the front).
#[cfg(windows)]
pub fn single_instance(root: &Path) -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use sha1::Digest;
    let hash = crate::util::hex(&sha1::Sha1::digest(root.display().to_string().to_lowercase().as_bytes()));
    let name = wide(&format!("Local\\devstack-{}", &hash[..12]));
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if !handle.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            if let Some(hwnd) = find_running_window() {
                show_window(hwnd);
            }
            return false;
        }
        // The mutex handle is intentionally leaked: it lives as long as the process.
    }
    true
}

/// The window of the devstack that is already running. Matched by title *and* by the owning
/// process's executable name, because an Explorer window open on C:\devstack has the same title.
#[cfg(windows)]
fn find_running_window() -> Option<isize> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId};
    let me = std::env::current_exe().ok()?.file_name()?.to_string_lossy().to_lowercase();
    let title = wide(crate::APP_NAME);
    let mut after: HWND = std::ptr::null_mut();
    loop {
        let hwnd = unsafe { FindWindowExW(std::ptr::null_mut(), after, std::ptr::null(), title.as_ptr()) };
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != std::process::id() && crate::procs::process_name(pid).is_some_and(|n| n.to_lowercase() == me) {
            return Some(hwnd as isize);
        }
        after = hwnd;
    }
}

#[cfg(not(windows))]
pub fn single_instance(_root: &Path) -> bool {
    true
}

// ------------------------------------------------------------------ opening things
pub fn open_path(path: &Path) -> Result<()> {
    open::that_detached(path).with_context(|| format!("couldn't open {}", path.display()))
}

pub fn open_url(url: &str) -> Result<()> {
    open::that_detached(url).with_context(|| format!("couldn't open {url}"))
}

pub fn edit_file(path: &Path, editor: &str) -> Result<()> {
    if !path.exists() {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, "")?;
    }
    if !editor.trim().is_empty() {
        Command::new(editor.trim()).arg(path).spawn()?;
    } else if cfg!(windows) {
        Command::new("notepad.exe").arg(path).spawn()?;
    } else {
        open_path(path)?;
    }
    Ok(())
}

/// Run a script in its own console window (e.g. a project's dev server), left open so its
/// output stays visible.
pub fn open_console(script: &Path, cwd: &Path, env: &std::collections::HashMap<String, String>) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x10;
        // Started as the script itself, not as `cmd /k <path>`: Rust then quotes the path for
        // cmd.exe, so an `&` in a folder name can't be read as a second command.
        Command::new(script).current_dir(cwd).env_clear().envs(env)
            .creation_flags(CREATE_NEW_CONSOLE).spawn().context("couldn't open a console")?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (script, cwd, env);
        anyhow::bail!("starting a dev server console is only supported on Windows")
    }
}

pub fn open_terminal(cwd: &Path, env: &std::collections::HashMap<String, String>, kind: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x10;
        let mut c = match kind {
            // wt.exe usually hands the new window to an already running Windows Terminal,
            // which doesn't inherit our environment, and it treats ';' (all over PATH) as a
            // command separator. So the environment goes into a small script the tab runs.
            "wt" => {
                let script = std::env::temp_dir().join("devstack-terminal.cmd");
                let mut text = String::from("@echo off\r\n");
                for key in ["PATH", "COMPOSER_HOME", "PHPRC"] {
                    if let Some(v) = env.get(key) {
                        text.push_str(&format!("set \"{key}={}\"\r\n", v.replace('%', "%%")));
                    }
                }
                text.push_str("title devstack\r\n");
                std::fs::write(&script, text).context("couldn't write the terminal start script")?;
                let mut c = Command::new("wt.exe");
                c.args(["-w", "new", "-d"]).arg(cwd).args(["cmd.exe", "/k"]).arg(&script);
                c
            }
            "powershell" => {
                let mut c = Command::new("powershell.exe");
                c.args(["-NoExit", "-NoLogo"]).creation_flags(CREATE_NEW_CONSOLE);
                c
            }
            "pwsh" => {
                let mut c = Command::new("pwsh.exe");
                c.args(["-NoExit", "-NoLogo"]).creation_flags(CREATE_NEW_CONSOLE);
                c
            }
            _ => {
                let mut c = Command::new("cmd.exe");
                c.args(["/k", "title devstack"]).creation_flags(CREATE_NEW_CONSOLE);
                c
            }
        };
        c.current_dir(cwd).env_clear().envs(env).spawn().context("couldn't open a terminal")?;
    }
    #[cfg(not(windows))]
    {
        let _ = kind;
        Command::new("x-terminal-emulator").current_dir(cwd).env_clear().envs(env).spawn().context("couldn't open a terminal")?;
    }
    Ok(())
}
