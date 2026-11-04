//! Startup journal, written from the very first line of `run()` until the first unlock,
//! then deleted. It is readable while the window is blank (tray menu, fallback page,
//! plain file), so a tester can paste it in a bug report.
//!
//! Before unlock there is no amount or address to leak; the home directory is replaced
//! by `~` so that user names do not end up in reports.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

const IDENTIFIER: &str = "com.libre-market.autopay";
const FILE: &str = "startup.log";
const PREVIOUS: &str = "startup.previous.log";
/// Lines beyond this size are dropped: a looping error must not fill the disk.
const MAX_BYTES: u64 = 512 * 1024;

static ACTIVE: AtomicBool = AtomicBool::new(true);
static FRONTEND_READY: AtomicBool = AtomicBool::new(false);
static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static HOME: OnceLock<Option<String>> = OnceLock::new();

/// Same folder as Tauri's `app_log_dir()`, resolved before Tauri starts.
fn dir() -> Option<&'static PathBuf> {
    DIR.get_or_init(|| {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if cfg!(target_os = "macos") {
            env("HOME").map(|h| h.join("Library/Logs").join(IDENTIFIER))
        } else if cfg!(windows) {
            env("LOCALAPPDATA").map(|d| d.join(IDENTIFIER).join("logs"))
        } else {
            env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")))
                .map(|d| d.join(IDENTIFIER).join("logs"))
        }
    }).as_ref()
}

pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join(FILE))
}

fn scrub(msg: &str) -> String {
    let home = HOME.get_or_init(|| {
        std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok().filter(|h| h.len() > 1)
    });
    match home {
        Some(h) => msg.replace(h.as_str(), "~"),
        None => msg.to_string(),
    }
}

/// Start a new journal; the previous one is kept once, in case the last launch failed.
pub fn init() {
    let Some(dir) = dir() else { return };
    let _ = std::fs::create_dir_all(dir);
    let current = dir.join(FILE);
    if current.exists() {
        let _ = std::fs::rename(&current, dir.join(PREVIOUS));
    }
    write("INFO", "boot", &format!(
        "Crypto AutoPay {} starting — {} {} ({})",
        env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH, os_version(),
    ));

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write("ERROR", "panic", &info.to_string());
        previous_hook(info);
    }));
}

/// Append one line, timestamped like the plugin's log file, while the journal is open.
pub fn write(level: &str, target: &str, msg: &str) {
    append(&format!("{}[{}][{}] {}", chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"), level, target, msg));
}

/// Append an already formatted line (from the Rust logger).
pub fn append(line: &str) {
    if !active() { return; }
    let Some(path) = path() else { return };
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) { return; }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", scrub(line.trim_end()));
    }
}

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// First unlock: the journal has served its purpose.
pub fn finish() {
    if !ACTIVE.swap(false, Ordering::Relaxed) { return; }
    if let Some(dir) = dir() {
        let _ = std::fs::remove_file(dir.join(FILE));
        let _ = std::fs::remove_file(dir.join(PREVIOUS));
    }
}

/// Current journal, preceded by the previous launch's when there is one.
pub fn read() -> String {
    let Some(dir) = dir() else { return String::new() };
    let current = std::fs::read_to_string(dir.join(FILE)).unwrap_or_default();
    match std::fs::read_to_string(dir.join(PREVIOUS)) {
        Ok(prev) if !prev.is_empty() => format!("{prev}\n----- current launch -----\n{current}"),
        _ => current,
    }
}

pub fn set_frontend_ready() {
    if !FRONTEND_READY.swap(true, Ordering::Relaxed) {
        write("INFO", "boot", "Interface displayed");
    }
}

pub fn frontend_ready() -> bool {
    FRONTEND_READY.load(Ordering::Relaxed)
}

/// Shown in the window when the interface never came up: plain DOM calls only, so that
/// it still runs on a WebView too old for the app bundle.
pub fn fallback_page_script() -> String {
    let file = path().map(|p| scrub(&p.to_string_lossy())).unwrap_or_default();
    let intro = format!(
        "AutoPay n'a pas pu afficher son interface / AutoPay could not display its interface. \
         Copiez ce journal et envoyez-le / Copy this log and send it (Ctrl/Cmd+A, Ctrl/Cmd+C). \
         Fichier / File: {file}"
    );
    let js = |s: &str| serde_json::to_string(s).unwrap_or_default()
        .replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029");
    format!(
        "(function(){{var d=document,b=d.body||d.documentElement;b.innerHTML='';\
         b.style.cssText='margin:0;padding:16px;font:13px sans-serif;background:#fff;color:#222';\
         var p=d.createElement('p');p.textContent={};b.appendChild(p);\
         var t=d.createElement('textarea');t.readOnly=true;t.value={};\
         t.style.cssText='width:100%;height:75vh;font:11px monospace';b.appendChild(t);t.focus();t.select();}})();",
        js(&intro), js(&read()),
    )
}

/// Open the journal in the system's text viewer (works when the window is blank).
pub fn open_in_viewer() {
    let Some(path) = path().filter(|p| p.exists()) else { return };
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-t").arg(&path).spawn();
    #[cfg(windows)]
    let r = std::process::Command::new("notepad.exe").arg(&path).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open").arg(&path).spawn();
    if let Err(e) = r {
        write("WARN", "boot", &format!("Could not open the startup log: {e}"));
    }
}

fn os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sw_vers").arg("-productVersion").output().ok()
            .map(|o| format!("macOS {}", String::from_utf8_lossy(&o.stdout).trim()))
            .unwrap_or_else(|| "macOS ?".into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd").args(["/C", "ver"]).creation_flags(CREATE_NO_WINDOW)
            .output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "Windows ?".into())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        std::fs::read_to_string("/etc/os-release").ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_else(|| "Linux ?".into())
    }
}
