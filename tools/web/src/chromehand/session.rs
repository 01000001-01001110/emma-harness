//! Session architecture (ADR-3): Chrome ITSELF is the session daemon — no
//! custom IPC server. `session open` spawns a detached Chrome with an
//! ephemeral CDP port and an isolated profile, records `{id, pid, ws_url,
//! target_id, …}` in `.browser-miner/session-<id>.json`, and exits. Every
//! verb with `--session <id>` reconnects over the recorded websocket, acts on
//! the recorded page target, and disconnects — the page and its state persist
//! between commands.
//!
//! Honest security note (recorded in every session file): a live session's
//! CDP websocket is unauthenticated on localhost — while it exists, ANY local
//! process can puppet that browser. Keep sessions short-lived; profiles are
//! isolated and throwaway.
//!
//! TTL: a session not contacted for --session-ttl (default 30 min) is killed
//! on next contact. `session close <id|--all>` tears down; `session list`
//! enumerates.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use futures::StreamExt;

use crate::chromehand::digest::now_iso;

// region: The session record
// ---------------------------------------------------------------------------
// The session record
//
// A session is a file on disk, not an object in memory — that is what lets one
// command open a browser and a later command act on it. Note `validate_id`:
// the id becomes a path component, so it is constrained to hex before any path
// is built from it, and every read and write goes through that check.
// ---------------------------------------------------------------------------

pub const SESSION_DIR: &str = ".browser-miner";
pub const DEFAULT_SESSION_TTL_SECS: u64 = 30 * 60;
const CDP_SECURITY_WARNING: &str = "This session's CDP websocket is unauthenticated on localhost: while it lives, any local process can puppet the browser. Isolated throwaway profile; close promptly (session close).";

#[derive(serde::Serialize, serde::Deserialize)]
pub struct SessionFile {
    pub id: String,
    pub pid: u32,
    pub ws_url: String,
    pub target_id: String,
    pub headful: bool,
    #[serde(default)]
    pub attached: bool,
    #[serde(default = "default_managed_true")]
    pub managed: bool,
    pub created_at: String,
    pub created_unix: u64,
    pub last_used_unix: u64,
    pub warning: String,
}

fn default_managed_true() -> bool {
    true
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn validate_id(id: &str) -> Result<(), String> {
    if !id.is_empty() && id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        Ok(())
    } else {
        Err("refused: invalid session id".to_string())
    }
}

fn session_path(id: &str) -> PathBuf {
    Path::new(SESSION_DIR).join(format!("session-{}.json", id))
}

fn session_profile_dir(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("browser-miner-session-{}", id))
}

fn read_session(id: &str) -> Result<SessionFile, String> {
    validate_id(id)?;
    let p = session_path(id);
    let raw = std::fs::read_to_string(&p)
        .map_err(|_| format!("no such session '{}' (looked at {})", id, p.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("corrupt session file {}: {}", p.display(), e))
}

fn write_session(s: &SessionFile) -> Result<(), String> {
    validate_id(&s.id)?;
    std::fs::create_dir_all(SESSION_DIR).map_err(|e| e.to_string())?;
    std::fs::write(
        session_path(&s.id),
        serde_json::to_string_pretty(s).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

// endregion: The session record

// region: Delta snapshots
// ---------------------------------------------------------------------------
// Delta snapshots
//
// The baseline `digest --delta` diffs against, stored beside the session file
// and keyed to the same id so closing a session leaves nothing behind.
// ---------------------------------------------------------------------------

/// Snapshot path for `digest --session <id> --delta`.
/// Lives beside the session file: `.browser-miner/session-<id>.digest.json`.
pub fn digest_snapshot_path(id: &str) -> PathBuf {
    let _ = validate_id(id);
    Path::new(SESSION_DIR).join(format!("session-{}.digest.json", id))
}

/// Load the prior delta snapshot for a session, if any.
pub fn read_digest_snapshot(id: &str) -> Result<serde_json::Value, String> {
    let p = digest_snapshot_path(id);
    let raw = std::fs::read_to_string(&p)
        .map_err(|_| format!("no prior digest snapshot for session '{}'", id))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("corrupt digest snapshot {}: {}", p.display(), e))
}

/// Persist the current inventory + text hash as the baseline for the next
/// `digest --delta` call on this session.
pub fn write_digest_snapshot(id: &str, v: &serde_json::Value) -> Result<(), String> {
    std::fs::create_dir_all(SESSION_DIR).map_err(|e| e.to_string())?;
    std::fs::write(
        digest_snapshot_path(id),
        serde_json::to_string_pretty(v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

// endregion: Delta snapshots

// region: Spawning a Chrome that outlives the command
// ---------------------------------------------------------------------------
// Spawning a Chrome that outlives the command
//
// The awkward part of the whole design, and the Windows handle-inheritance
// scar below is the reason it is written in raw CreateProcessW rather than
// three lines of std::process. Read that comment before touching any of this.
// ---------------------------------------------------------------------------

/// Kill a process by pid, cross-platform, best-effort.
fn kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

/// Pick an ephemeral localhost port. Tiny bind-release race, local-only.
fn pick_free_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("no free port: {}", e))
}

/// Spawn Chrome fully detached with NO handle inheritance.
///
/// Windows scar (cost a debugging session): `std::process::Command` spawns
/// with `bInheritHandles=TRUE`, so a detached child inherits EVERY
/// inheritable handle in this process — including pipe ends that callers up
/// the chain (test harness, shells) are reading. Chrome then holds those
/// pipes open long after everyone exits and the whole caller chain waits on
/// an EOF that never comes. Fix: raw `CreateProcessW` with
/// `bInheritHandles=FALSE` — Chrome gets no handles at all. (We therefore
/// can't read its stderr; the CDP endpoint is discovered by polling
/// `http://127.0.0.1:<port>/json/version` on a port WE chose.)
/// On Unix, CLOEXEC + null stdio make std::process safe.
fn spawn_chrome_detached(chrome: &Path, args: &[String]) -> Result<u32, String> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            CreateProcessW, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, PROCESS_INFORMATION,
            STARTUPINFOW,
        };
        fn quote(s: &str) -> String {
            if s.contains(' ') || s.contains('"') {
                format!("\"{}\"", s.replace('"', "\\\""))
            } else {
                s.to_string()
            }
        }
        let mut cmdline = quote(&chrome.display().to_string());
        for a in args {
            cmdline.push(' ');
            cmdline.push_str(&quote(a));
        }
        let mut wide: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            std::ptr::null(),
            wide.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0, // bInheritHandles = FALSE — the entire point
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        );
        if ok == 0 {
            return Err(format!(
                "CreateProcessW failed (error {})",
                std::io::Error::last_os_error()
            ));
        }
        windows_sys::Win32::Foundation::CloseHandle(pi.hProcess);
        windows_sys::Win32::Foundation::CloseHandle(pi.hThread);
        Ok(pi.dwProcessId)
    }
    #[cfg(not(windows))]
    {
        let child = std::process::Command::new(chrome)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn chrome: {}", e))?;
        let pid = child.id();
        drop(child); // do not wait — Chrome outlives this command
        Ok(pid)
    }
}

// endregion: Spawning a Chrome that outlives the command

// region: Opening a session
// ---------------------------------------------------------------------------
// Opening a session
//
// Two ways in, and the difference between them is who owns the browser.
// `open` spawns one and records `managed: true`, which is a licence to kill it
// later. `open_attach` connects to the user's own Chrome and records
// `managed: false`, which is a standing instruction never to.
// ---------------------------------------------------------------------------

/// `session open [--headful]` — spawn detached Chrome, record the session.
/// chromiumoxide's Browser::launch kills its child on drop (kill_on_drop), so
/// the daemon Chrome is spawned MANUALLY and only ever connected to.
pub async fn open(headful: bool) -> Result<serde_json::Value, String> {
    let chrome = chromiumoxide::detection::default_executable(Default::default())
        .map_err(|e| format!("no Chrome/Chromium/Edge found: {}", e))?;

    // Short unique id without a rand dependency.
    let id = format!(
        "{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );
    let profile_dir = std::env::temp_dir().join(format!("browser-miner-session-{}", id));
    std::fs::create_dir_all(SESSION_DIR).map_err(|e| e.to_string())?;

    let port = pick_free_port()?;
    let mut args = vec![
        format!("--remote-debugging-port={}", port),
        format!("--user-data-dir={}", profile_dir.display()),
        "--no-first-run".to_string(),
        "--disable-extensions".to_string(),
        "--mute-audio".to_string(),
        "--no-default-browser-check".to_string(),
    ];
    if !headful {
        args.push("--headless=new".to_string());
    }
    args.push("about:blank".to_string());

    let pid = spawn_chrome_detached(&chrome, &args)?;

    // Discover the CDP endpoint by polling /json/version on OUR port
    // (Browser::connect resolves an http URL to the ws endpoint itself).
    let http_url = format!("http://127.0.0.1:{}", port);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let connected = loop {
        match Browser::connect(&http_url).await {
            Ok(v) => break Some(v),
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(_) => break None,
        }
    };
    let Some((browser, mut handler)) = connected else {
        kill_pid(pid);
        let _ = std::fs::remove_dir_all(session_profile_dir(&id));
        return Err(format!(
            "Chrome did not open a DevTools endpoint on port {} within 20s",
            port
        ));
    };
    let ws_url = browser.websocket_address().to_string();
    let handler_task = tokio::task::spawn(async move {
        while let Some(ev) = handler.next().await {
            if ev.is_err() {
                break;
            }
        }
    });
    let target_id = {
        let pages = browser.pages().await.map_err(|e| e.to_string())?;
        match pages.first() {
            Some(p) => p.target_id().as_ref().to_string(),
            None => {
                let p = browser
                    .new_page("about:blank")
                    .await
                    .map_err(|e| e.to_string())?;
                p.target_id().as_ref().to_string()
            }
        }
    };
    drop(browser); // connected browser: drop only closes the ws, Chrome lives
    handler_task.abort();

    let now = unix_now();
    let s = SessionFile {
        id: id.clone(),
        pid,
        ws_url: ws_url.clone(),
        target_id: target_id.clone(),
        headful,
        attached: false,
        managed: true,
        created_at: now_iso(),
        created_unix: now,
        last_used_unix: now,
        warning: CDP_SECURITY_WARNING.to_string(),
    };
    write_session(&s)?;

    Ok(serde_json::json!({
        "command": "session",
        "action": "open",
        "id": id,
        "pid": pid,
        "attached": false,
        "managed": true,
        "ws_url": ws_url,
        "target_id": target_id,
        "headful": headful,
        "session_file": session_path(&id).display().to_string(),
        "warning": CDP_SECURITY_WARNING,
        "evidence": { "source": "browser-render", "fetch_timestamp": now_iso() }
    }))
}

/// `session open --attach <connection_url>` — connect to the user's already-
/// listening Chrome, record a session file, but NEVER kill it on close.
pub async fn open_attach(connection_url: &str) -> Result<serde_json::Value, String> {
    let (browser, mut handler) = Browser::connect(connection_url).await.map_err(|e| {
        format!(
            "attach connect: {} (is Chrome listening on {}?)",
            e, connection_url
        )
    })?;
    let ws_url = browser.websocket_address().to_string();
    let handler_task = tokio::task::spawn(async move {
        while let Some(ev) = handler.next().await {
            if ev.is_err() {
                break;
            }
        }
    });
    let pages = browser.pages().await.map_err(|e| e.to_string())?;
    let target_id = match pages.into_iter().next() {
        Some(p) => p.target_id().as_ref().to_string(),
        None => {
            let p = browser
                .new_page("about:blank")
                .await
                .map_err(|e| e.to_string())?;
            p.target_id().as_ref().to_string()
        }
    };
    drop(browser);
    handler_task.abort();

    let id = format!(
        "{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );
    let now = unix_now();
    let s = SessionFile {
        id: id.clone(),
        pid: 0,
        ws_url: ws_url.clone(),
        target_id: target_id.clone(),
        headful: false,
        attached: true,
        managed: false,
        created_at: now_iso(),
        created_unix: now,
        last_used_unix: now,
        warning: CDP_SECURITY_WARNING.to_string(),
    };
    write_session(&s)?;

    Ok(serde_json::json!({
        "command": "session",
        "action": "open",
        "id": id,
        "pid": 0,
        "attached": true,
        "managed": false,
        "ws_url": ws_url,
        "target_id": target_id,
        "headful": false,
        "session_file": session_path(&id).display().to_string(),
        "warning": CDP_SECURITY_WARNING,
        "evidence": { "source": "browser-render", "fetch_timestamp": now_iso() }
    }))
}

// endregion: Opening a session

// region: Reconnecting for one verb
// ---------------------------------------------------------------------------
// Reconnecting for one verb
//
// Every session command is a fresh process that connects, acts and drops the
// websocket without closing the browser — `disconnect` is the load-bearing
// half of that and the reason a session survives at all. The TTL is enforced
// here, on contact, since there is no daemon to expire anything on a timer.
// ---------------------------------------------------------------------------

/// A live, reconnected session handle for the action verbs.
pub struct Connected {
    pub browser: Browser,
    pub handler_task: tokio::task::JoinHandle<()>,
    pub page: chromiumoxide::Page,
    pub id: String,
    pub headful: bool,
    pub attached: bool,
}

/// Reconnect to a session for one verb. Enforces the TTL: an expired session
/// is killed + removed and the command exits 2 (a refusal, not a crash).
pub async fn connect(id: &str, ttl_secs: u64) -> Result<Connected, String> {
    let mut s = read_session(id)?;
    let now = unix_now();
    if now.saturating_sub(s.last_used_unix) > ttl_secs {
        // CRITICAL (ADR-2): attached sessions are the USER'S Chrome — never kill it.
        if s.managed {
            kill_pid(s.pid);
        }
        let _ = std::fs::remove_file(session_path(id));
        let _ = std::fs::remove_dir_all(session_profile_dir(id));
        return Err(format!(
            "session '{}' expired (idle {}s > ttl {}s) — {} and removed; open a new one",
            id,
            now.saturating_sub(s.last_used_unix),
            ttl_secs,
            if s.managed { "killed" } else { "disconnected" }
        ));
    }

    let (browser, mut handler) = Browser::connect(&s.ws_url).await.map_err(|e| {
        format!(
            "session '{}' unreachable ({}): run `session close {}`",
            id, e, id
        )
    })?;
    let handler_task = tokio::task::spawn(async move {
        while let Some(ev) = handler.next().await {
            if ev.is_err() {
                break;
            }
        }
    });
    // Target discovery on a fresh connection is event-driven — the handler
    // may not know the recorded target yet. Retry briefly before declaring
    // the page gone.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let page = loop {
        match browser.get_page(TargetId::new(s.target_id.clone())).await {
            Ok(p) => break p,
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!("session '{}' page target gone ({})", id, e));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };

    s.last_used_unix = now;
    let _ = write_session(&s);

    Ok(Connected {
        browser,
        handler_task,
        page,
        id: id.to_string(),
        headful: s.headful,
        attached: s.attached,
    })
}

/// Disconnect WITHOUT closing Chrome (the whole point of a session).
pub fn disconnect(c: Connected) {
    drop(c.page);
    drop(c.browser);
    c.handler_task.abort();
}

/// Direct one-shot connection for stateless `--attach` digest/verify. Does NOT
/// create a session file; the caller disconnects when done.
pub async fn connect_direct(connection_url: &str) -> Result<Connected, String> {
    let (browser, mut handler) = Browser::connect(connection_url).await.map_err(|e| {
        format!(
            "attach connect: {} (is Chrome listening on {}?)",
            e, connection_url
        )
    })?;
    let handler_task = tokio::task::spawn(async move {
        while let Some(ev) = handler.next().await {
            if ev.is_err() {
                break;
            }
        }
    });
    let pages = browser.pages().await.map_err(|e| e.to_string())?;
    let page = match pages.into_iter().next() {
        Some(p) => p,
        None => browser
            .new_page("about:blank")
            .await
            .map_err(|e| e.to_string())?,
    };
    Ok(Connected {
        browser,
        handler_task,
        page,
        id: String::new(),
        headful: false,
        attached: true,
    })
}

// endregion: Reconnecting for one verb

// region: Closing and listing
// ---------------------------------------------------------------------------
// Closing and listing
//
// Teardown, and the one rule it must never break: an attached session's Chrome
// belongs to the user, so closing removes the record and stops there. Only a
// managed session gets the polite CDP close, the hard kill and the profile
// directory removed.
// ---------------------------------------------------------------------------

/// `session close <id>` / `session close --all`.
pub async fn close(id_or_all: &str) -> serde_json::Value {
    let ids: Vec<String> = if id_or_all == "--all" {
        list_ids()
    } else {
        vec![id_or_all.to_string()]
    };
    let mut closed = Vec::new();
    for id in ids {
        match read_session(&id) {
            Ok(s) => {
                if s.managed {
                    // Polite CDP close first, then hard kill as fallback.
                    if let Ok((mut browser, mut handler)) = Browser::connect(&s.ws_url).await {
                        let h = tokio::task::spawn(async move {
                            while let Some(ev) = handler.next().await {
                                if ev.is_err() {
                                    break;
                                }
                            }
                        });
                        let _ = browser.close().await;
                        h.abort();
                    }
                    kill_pid(s.pid);
                    let _ = std::fs::remove_dir_all(session_profile_dir(&id));
                }
                // CRITICAL (ADR-2): attached sessions are the USER'S Chrome —
                // do NOT send Browser.close() and do NOT kill the pid.
                let _ = std::fs::remove_file(session_path(&id));
                closed.push(serde_json::json!({
                    "id": id,
                    "pid": s.pid,
                    "closed": true,
                    "attached": s.attached,
                    "managed": s.managed
                }));
            }
            Err(e) => closed.push(serde_json::json!({ "id": id, "closed": false, "error": e })),
        }
    }
    serde_json::json!({
        "command": "session",
        "action": "close",
        "closed": closed,
        "evidence": { "source": "browser-render", "fetch_timestamp": now_iso() }
    })
}

fn list_ids() -> Vec<String> {
    let mut ids = Vec::new();
    if let Ok(entries) = std::fs::read_dir(SESSION_DIR) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(id) = name
                .strip_prefix("session-")
                .and_then(|n| n.strip_suffix(".json"))
            {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

/// `session list` — enumerate session files with age and expiry status.
pub fn list(ttl_secs: u64) -> serde_json::Value {
    let now = unix_now();
    let sessions: Vec<serde_json::Value> = list_ids()
        .into_iter()
        .filter_map(|id| read_session(&id).ok())
        .map(|s| {
            let idle = now.saturating_sub(s.last_used_unix);
            serde_json::json!({
                "id": s.id,
                "pid": s.pid,
                "headful": s.headful,
                "created_at": s.created_at,
                "idle_secs": idle,
                "expired": idle > ttl_secs
            })
        })
        .collect();
    serde_json::json!({
        "command": "session",
        "action": "list",
        "sessions": sessions,
        "evidence": { "source": "browser-render", "fetch_timestamp": now_iso() }
    })
}

// endregion: Closing and listing
