//! Private local Chrome CDP transport, over Chromium's fd 3/4 pipe protocol.
//! No debugging TCP port, no bundled Chrome, and no access to the daily profile.
//! This module is macOS-only. Chrome owns a private profile inside Herdr's home.
mod framing;

use base64::Engine as _;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    os::{fd::{FromRawFd, RawFd}, unix::{fs::{MetadataExt, PermissionsExt}, process::CommandExt}},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, Weak, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    },
    time::{Duration, Instant},
};

const MAX_MESSAGE: usize = 32 * 1024 * 1024;
const MAX_FRAME_BASE64: usize = 16 * 1024 * 1024;
const CDP_TIMEOUT: Duration = Duration::from_secs(15);
#[derive(Clone, Debug)]
pub struct ChromeBinary {
    pub path: PathBuf,
    pub version: String,
}

// Availability::current and shared_client commonly probe the same binary
// back-to-back. Reuse only a successful, very recent version check and only
// while the exact executable identity is unchanged. Never cache a miss.
const DISCOVERY_TTL: Duration = Duration::from_secs(5);
#[derive(Clone, Copy, PartialEq, Eq)]
struct ExecutableIdentity {
    dev: u64, ino: u64, size: u64, mode: u32,
    mtime: i64, mtime_nsec: i64, ctime: i64, ctime_nsec: i64,
}
impl ExecutableIdentity {
    fn of(meta: &fs::Metadata) -> Self {
        Self { dev: meta.dev(), ino: meta.ino(), size: meta.size(), mode: meta.mode(),
            mtime: meta.mtime(), mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(), ctime_nsec: meta.ctime_nsec() }
    }
}
struct DiscoveryEntry {
    binary: ChromeBinary,
    identity: ExecutableIdentity,
    checked: Instant,
}
static DISCOVERY: OnceLock<Mutex<Option<DiscoveryEntry>>> = OnceLock::new();

fn discover_candidate(
    path: &Path, cache: &Mutex<Option<DiscoveryEntry>>,
    probe: impl FnOnce(&Path) -> Option<String>,
) -> Option<ChromeBinary> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 { return None }
    let identity = ExecutableIdentity::of(&meta);
    {
        let cached = cache.lock().unwrap();
        if let Some(entry) = cached.as_ref()
            && entry.binary.path == path && entry.identity == identity
            && entry.checked.elapsed() < DISCOVERY_TTL
        {
            return Some(entry.binary.clone());
        }
    }
    // Do not hold the global mutex while starting a subprocess.
    let version = probe(path)?;
    if !chrome_major(&version).is_some_and(|major| major >= 136) { return None }
    if ExecutableIdentity::of(&fs::metadata(path).ok()?) != identity { return None }
    let binary = ChromeBinary { path: path.to_owned(), version };
    *cache.lock().unwrap() = Some(DiscoveryEntry {
        binary: binary.clone(), identity, checked: Instant::now(),
    });
    Some(binary)
}

/// Discover only the user's installed Chrome, never an automated download.
/// HERDR_CHROME_BINARY is an explicit override intended for managed macOS
/// installations and CI; it must still be a usable executable.
pub fn discover() -> Result<ChromeBinary, String> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("HERDR_CHROME_BINARY") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    }
    let cache = DISCOVERY.get_or_init(|| Mutex::new(None));
    for path in candidates {
        if let Some(binary) = discover_candidate(&path, cache, |path| {
            let output = Command::new(path).arg("--version").output().ok()?;
            output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }) { return Ok(binary); }
    }
    Err("CHROME_NOT_INSTALLED: install Google Chrome 136+ (stable) in /Applications, or set HERDR_CHROME_BINARY to its executable".into())
}

fn chrome_major(version: &str) -> Option<u32> {
    version.split_whitespace().find_map(|word| {
        let head = word.split('.').next()?;
        (word.contains('.') && head.len() <= 4).then(|| head.parse().ok()).flatten()
    })
}

fn private_profile(path: &Path) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && (!meta.is_dir() || meta.file_type().is_symlink())
    {
        return Err("Chrome profile path must be a real directory (not a symlink)".into());
    }
    fs::create_dir_all(path).map_err(|e| format!("cannot create Chrome profile: {e}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot protect Chrome profile: {e}"))?;
    Ok(())
}

/// Duplicate both ends above fd 4; the Chrome protocol reserves exactly
/// fd 3 (read commands) and fd 4 (write events/responses).
fn pipe_pair() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [-1, -1];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut safe = [-1, -1];
    for i in 0..2 {
        safe[i] = unsafe { libc::fcntl(fds[i], libc::F_DUPFD_CLOEXEC, 10) };
        unsafe { libc::close(fds[i]); }
        if safe[i] < 0 {
            let err = io::Error::last_os_error();
            if i == 1 { unsafe { libc::close(safe[0]); } }
            return Err(err);
        }
    }
    Ok((safe[0], safe[1]))
}

// Readiness notifications are hints, not proof that the requested document
// is interactive. Coalesce bursts without ever blocking the CDP reader.
const DOCUMENT_POLL_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Clone)]
struct SessionEvents {
    frames: SyncSender<String>,
    document_ready: SyncSender<()>,
}

fn is_document_ready_event(method: &str) -> bool {
    matches!(method, "Page.domContentEventFired" | "Page.loadEventFired")
}

fn wait_for_document_hint(receiver: &Receiver<()>) {
    if let Err(RecvTimeoutError::Disconnected) = receiver.recv_timeout(DOCUMENT_POLL_INTERVAL) {
        // Keep the original bounded polling cadence even if the hint channel
        // disconnects unexpectedly; never spin on a disconnected receiver.
        std::thread::sleep(DOCUMENT_POLL_INTERVAL);
    }
}

pub struct Client {
    child: Mutex<Child>,
    writer: Mutex<File>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    // Keep the CDP reader free of PNG decoding; its job is dispatching
    // responses promptly, including navigation and security-sensitive inputs.
    frames: Mutex<HashMap<String, SessionEvents>>,
    // Screencast ACK writes must never happen inside the pipe reader:
    // Chrome can block writing a large PNG while waiting for the next ACK.
    // Writing an ACK synchronously from its reader creates a pipe deadlock.
    // ACKs are tiny and Chrome throttles frames on them, so this queue must
    // never drop one (a dropped ACK stalls the stream) and never block the
    // reader; an unbounded channel gives both.
    frame_acks: Sender<(String, i64)>,
}

impl Client {
    pub fn start(binary: &ChromeBinary, profile: &Path) -> Result<Arc<Self>, String> {
        private_profile(profile)?;
        let (cmd_read, cmd_write) = pipe_pair().map_err(|e| e.to_string())?;
        let (out_read, out_write) = match pipe_pair() {
            Ok(p) => p,
            Err(e) => {
                unsafe { libc::close(cmd_read); libc::close(cmd_write); }
                return Err(e.to_string());
            }
        };
        let mut command = Command::new(&binary.path);
        command
            .arg("--headless=new")
            .arg("--remote-debugging-pipe")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-networking")
            // Chrome 158+ initialises the "Chrome Safe Storage" keychain item
            // on the first real navigation and blocks forever when HOME is
            // fresh (CI, disposable homes): the page stays on about:blank and
            // Page.navigate stops answering. Every macOS automation stack
            // (Playwright, Puppeteer, Selenium) passes --use-mock-keychain for
            // exactly this. shortcut: cookies in this private agent-only
            // profile are obfuscated with a fixed key instead of the login
            // keychain, upgrade if the profile ever stores real credentials.
            .arg("--use-mock-keychain")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(cmd_read, 3) < 0 || libc::dup2(out_write, 4) < 0 {
                    return Err(io::Error::last_os_error());
                }
                for fd in [cmd_read, cmd_write, out_read, out_write] {
                    if fd != 3 && fd != 4 { libc::close(fd); }
                }
                Ok(())
            });
        }
        let spawned = command.spawn();
        unsafe { libc::close(cmd_read); libc::close(out_write); }
        let child = match spawned {
            Ok(child) => child,
            Err(e) => {
                unsafe { libc::close(cmd_write); libc::close(out_read); }
                return Err(format!("CHROME_START_FAILED: {e}"));
            }
        };
        let writer = unsafe { File::from_raw_fd(cmd_write) };
        let reader = unsafe { File::from_raw_fd(out_read) };
        let (frame_acks, ack_rx) = mpsc::channel::<(String, i64)>();
        let client = Arc::new(Self {
            child: Mutex::new(child),
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            frames: Mutex::new(HashMap::new()),
            frame_acks,
        });
        let weak = Arc::downgrade(&client);
        std::thread::Builder::new()
            .name("herdr-chrome-cdp-read".into())
            .spawn(move || read_messages(reader, weak))
            .map_err(|e| format!("cannot create CDP reader: {e}"))?;
        let weak = Arc::downgrade(&client);
        std::thread::Builder::new()
            .name("herdr-chrome-cdp-ack".into())
            .spawn(move || {
                while let Ok((session, frame_id)) = ack_rx.recv() {
                    let Some(client) = weak.upgrade() else { break };
                    if client.send("Page.screencastFrameAck",
                        json!({"sessionId":frame_id}), Some(&session)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("cannot create CDP ACK writer: {e}"))?;
        client.request("Browser.getVersion", json!({}), None)?;
        Ok(client)
    }

    pub fn request(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.write_message(id, method, params, session) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(CDP_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("CDP_TIMEOUT: {method} did not answer"))
            }
        }
    }

    fn write_message(&self, id: u64, method: &str, params: Value, session: Option<&str>) -> Result<(), String> {
        let mut value = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            value["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        bytes.push(0);
        self.writer.lock().unwrap().write_all(&bytes)
            .map_err(|e| format!("CDP_DISCONNECTED: {e}"))
    }

    pub fn send(&self, method: &str, params: Value, session: Option<&str>) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.write_message(id, method, params, session)
    }

    fn received(&self, mut message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                let result = if let Some(error) = message.get("error") {
                    Err(format!("CDP_ERROR: {error}"))
                } else {
                    Ok(message.get_mut("result").map(Value::take).unwrap_or(Value::Null))
                };
                let _ = tx.send(result);
            }
            return;
        }
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        if is_document_ready_event(method) {
            if let Some(session) = message.get("sessionId").and_then(Value::as_str)
                && let Some(events) = self.frames.lock().unwrap().get(session)
            {
                let _ = events.document_ready.try_send(());
            }
            return;
        }
        if method != "Page.screencastFrame" {
            return;
        }
        let Some(Value::String(session)) = message.get_mut("sessionId").map(Value::take) else { return };
        let Some(params) = message.get_mut("params") else { return };
        if let Some(frame_id) = params.get("sessionId").and_then(Value::as_i64) {
            // Never block the only CDP reader on a write into Chrome's pipe.
            // Even discarded frames need ACKs to keep the screencast moving.
            let _ = self.frame_acks.send((session.clone(), frame_id));
        }
        let Some(Value::String(data)) = params.get_mut("data").map(Value::take) else { return };
        if data.len() > MAX_FRAME_BASE64 { return }
        let sender = self.frames.lock().unwrap().get(&session).map(|events| events.frames.clone());
        if let Some(sender) = sender {
            // Transfer ownership of the bounded encoded payload. No extra
            // full-frame copy and no PNG decoding on the pipe-reader thread.
            let _ = sender.try_send(data);
        }
    }

    pub fn open(self: &Arc<Self>, url: &str) -> Result<Session, String> {
        let url = crate::browser::normalize_url(url)?;
        // Create the real destination directly as the first page in the target.
        // The old about:blank -> Page.startScreencast -> Page.navigate sequence
        // consistently hangs ONLY in the packaged GUI app (while the isolated
        // Rust fixture passes), even with off-thread PNG decoding/ACK writes.
        // Target.createTarget(url) lets Chrome navigate during target creation,
        // before any screen frames or CDP per-target commands are subscribed.
        let target = self.request("Target.createTarget", json!({"url":url}), None)?
            .get("targetId").and_then(Value::as_str).ok_or("CDP targetId missing")?.to_owned();
        let session = self.request("Target.attachToTarget", json!({"targetId":target,"flatten":true}), None)?
            .get("sessionId").and_then(Value::as_str).ok_or("CDP sessionId missing")?.to_owned();
        let (tx, rx) = mpsc::sync_channel(2);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        self.frames.lock().unwrap().insert(session.clone(), SessionEvents {
            frames: tx,
            document_ready: ready_tx,
        });
        let pane = Session { client: self.clone(), target, session, frames: Mutex::new(rx) };
        pane.command("Page.enable", json!({}))?;
        // Runtime.enable only subscribes to execution-context notifications.
        // This adapter evaluates directly in the default context and never
        // consumes those notifications. Runtime.evaluate and debugger resume
        // do not require the subscription. Avoid that extra round trip.
        // Chrome 144+ may leave a newly attached target waiting for the
        // debugger even after domain enable succeeds. A suspended target can
        // respond to enable/evaluate but never navigate or fetch page content.
        // Resume before any navigation, screencast or network-dependent work.
        // Use ordered CDP pipe delivery without waiting on the paused renderer:
        // runIfWaitingForDebugger is a no-op for already-running targets.
        pane.post("Runtime.runIfWaitingForDebugger", json!({}))?;
        pane.resize(1024, 768)?;
        // Chrome may create an attached target while its initial navigation is
        // still pending. Starting Page.startScreencast before the renderer has
        // an interactive document causes a persistent blank pane in the real
        // packaged App, even though the isolated CDP fixture can pass.
        // Explicitly establish a navigated document *before* frame capture.
        let started = Instant::now();
        let mut retried_navigation = false;
        let mut saw_blank = true;
        let mut last_state = "unknown".to_owned();
        let mut ready = false;
        while started.elapsed() < Duration::from_secs(20) {
            // Consume a stale hint before checking current state. A hint that
            // arrives during/after eval stays queued and wakes the next check.
            let _ = ready_rx.try_recv();
            if let Ok(state) = pane.eval("({href:location.href,ready:document.readyState})") {
                let href = state["href"].as_str().unwrap_or("");
                let phase = state["ready"].as_str().unwrap_or("unknown");
                saw_blank = href.is_empty() || href == "about:blank";
                last_state = phase.to_owned();
                if !saw_blank && matches!(phase, "interactive" | "complete") {
                    ready = true;
                    break;
                }
            }
            // Target.createTarget(url) should initiate navigation, but some
            // packaged Chrome launches remain on about:blank. Retry once via
            // Page.navigate *before* screencast starts, not while PNG events
            // are filling the debugging pipe.
            if saw_blank && !retried_navigation
                && started.elapsed() > Duration::from_secs(2)
            {
                retried_navigation = true;
                pane.navigate(&url)?;
            }
            wait_for_document_hint(&ready_rx);
        }
        if !ready {
            return Err(format!(
                "CHROME_DOCUMENT_NOT_READY: initial target never became interactive (phase={last_state}, blank={saw_blank}, retry={retried_navigation})"
            ));
        }
        pane.command("Page.startScreencast", json!({"format":"png","everyNthFrame":1}))?;
        Ok(pane)
    }
}


static SHARED_CHROME: OnceLock<Mutex<Weak<Client>>> = OnceLock::new();

/// All Chrome panes share one headless Chrome process and a private profile.
/// The mutex prevents concurrent pane creation from racing on the profile lock.
pub fn shared_client() -> Result<Arc<Client>, String> {
    let lock = SHARED_CHROME.get_or_init(|| Mutex::new(Weak::new()));
    let mut shared = lock.lock().unwrap();
    if let Some(client) = shared.upgrade() { return Ok(client); }
    let chrome = discover()?;
    let client = Client::start(&chrome, &persistent_profile())?;
    *shared = Arc::downgrade(&client);
    Ok(client)
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn read_messages(reader: File, client: Weak<Client>) {
    let mut reader = BufReader::with_capacity(32 * 1024, reader);
    let mut buffer = Vec::new();
    // Frame parsing is independent from routing and never writes to Chrome.
    // EOF, an oversized message or an I/O error all reach disconnect cleanup.
    while let Ok(true) = framing::read_message(&mut reader, &mut buffer, MAX_MESSAGE) {
        if let Ok(value) = serde_json::from_slice(&buffer)
            && let Some(client) = client.upgrade()
        {
            client.received(value);
        }
    }
    if let Some(client) = client.upgrade() {
        for (_, sender) in client.pending.lock().unwrap().drain() {
            let _ = sender.send(Err("CDP_DISCONNECTED: Chrome process exited".into()));
        }
    }
}

/// One Herdr pane owns exactly one Chrome Target / flattened session.
pub struct Session {
    client: Arc<Client>,
    pub target: String,
    pub session: String,
    frames: Mutex<Receiver<String>>,
}

impl Session {
    pub fn post(&self, name: &str, params: Value) -> Result<(), String> {
        self.client.send(name, params, Some(&self.session))
    }

    pub fn command(&self, name: &str, params: Value) -> Result<Value, String> {
        self.client.request(name, params, Some(&self.session))
    }

    pub fn navigate(&self, url: &str) -> Result<(), String> {
        let url = crate::browser::normalize_url(url)?;
        let r = self.command("Page.navigate", json!({"url":url}))?;
        if let Some(error) = r.get("errorText").and_then(Value::as_str) {
            return Err(format!("CDP_NAVIGATION_FAILED: {error}"));
        }
        Ok(())
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("Chrome viewport must be 1..8192 CSS pixels per axis".into());
        }
        self.command("Emulation.setDeviceMetricsOverride", json!({
            "width":width,"height":height,"deviceScaleFactor":1,"mobile":false,
        }))?;
        Ok(())
    }

    pub fn eval(&self, expression: &str) -> Result<Value, String> {
        let mut result = self.command("Runtime.evaluate", json!({
            "expression":expression,"returnByValue":true,"awaitPromise":true,
        }))?;
        if let Some(error) = result.get("exceptionDetails") {
            return Err(format!("CDP_EVAL_FAILED: {error}"));
        }
        Ok(result.get_mut("result").and_then(|r| r.get_mut("value"))
            .map(Value::take).unwrap_or(Value::Null))
    }

    pub fn screenshot(&self) -> Result<Vec<u8>, String> {
        let result = self.command("Page.captureScreenshot", json!({"format":"png","captureBeyondViewport":false}))?;
        let encoded = result.get("data").and_then(Value::as_str).ok_or("CDP screenshot data missing")?;
        if encoded.len() > MAX_FRAME_BASE64 {
            return Err("Chrome screenshot is too large".into());
        }
        base64::engine::general_purpose::STANDARD.decode(encoded)
            .map_err(|e| format!("invalid Chrome screenshot: {e}"))
    }

    pub fn frame_timeout(&self, timeout: Duration) -> Result<Vec<u8>, String> {
        let encoded = self.frames.lock().unwrap().recv_timeout(timeout)
            .map_err(|e| format!("Chrome screencast frame unavailable: {e}"))?;
        base64::engine::general_purpose::STANDARD.decode(encoded)
            .map_err(|e| format!("invalid Chrome screencast frame: {e}"))
    }

    pub fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        for (kind, clicks) in [("mousePressed",1),("mouseReleased",1)] {
            self.command("Input.dispatchMouseEvent", json!({
                "type":kind,"x":x,"y":y,"button":"left","clickCount":clicks,
            }))?;
        }
        Ok(())
    }

    pub fn insert_text(&self, text: &str) -> Result<(), String> {
        if text.len() > 64 * 1024 { return Err("Chrome input text exceeds 64KiB".into()) }
        self.command("Input.insertText", json!({"text":text}))?;
        Ok(())
    }

    pub fn scroll(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<(), String> {
        self.command("Input.dispatchMouseEvent", json!({
            "type":"mouseWheel","x":x,"y":y,"deltaX":dx,"deltaY":dy
        }))?;
        Ok(())
    }

    pub fn reload(&self) -> Result<(), String> {
        self.command("Page.reload", json!({}))?;
        Ok(())
    }

    pub fn history(&self, forward: bool) -> Result<(), String> {
        let history = self.command("Page.getNavigationHistory", json!({}))?;
        let current = history["currentIndex"].as_i64().ok_or("CDP history missing currentIndex")?;
        let next = if forward {current+1} else {current-1};
        let entries = history["entries"].as_array().ok_or("CDP history entries missing")?;
        if next < 0 || next as usize >= entries.len() {
            return Err("no browser history entry in that direction".into());
        }
        let id = entries[next as usize]["id"].as_i64().ok_or("CDP history entry missing ID")?;
        self.command("Page.navigateToHistoryEntry", json!({"entryId":id}))?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.client.frames.lock().unwrap().remove(&self.session);
        let _ = self.client.send("Target.closeTarget", json!({"targetId":self.target}), None);
    }
}

pub fn persistent_profile() -> PathBuf {
    crate::app_home::app_dir().join("chrome-profile")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_reuses_only_recent_success_for_same_executable() {
        let root = std::env::temp_dir().join(format!("herdr-discovery-{}-{}",
            std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&root).unwrap();
        let path = root.join("chrome");
        fs::write(&path, b"first").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let cache = Mutex::new(None);
        assert!(discover_candidate(&path, &cache, |_| None).is_none());
        assert!(discover_candidate(&path, &cache, |_| Some("Google Chrome 135.0".into())).is_none());
        assert!(discover_candidate(&path, &cache, |_| Some("Google Chrome 158.0".into())).is_some());
        assert_eq!(discover_candidate(&path, &cache, |_| panic!("cache miss")).unwrap().version, "Google Chrome 158.0");
        cache.lock().unwrap().as_mut().unwrap().checked = Instant::now() - DISCOVERY_TTL;
        assert_eq!(discover_candidate(&path, &cache, |_| Some("Google Chrome 159.0".into())).unwrap().version, "Google Chrome 159.0");
        fs::write(&path, b"changed executable bytes").unwrap();
        assert_eq!(discover_candidate(&path, &cache, |_| Some("Google Chrome 160.0".into())).unwrap().version, "Google Chrome 160.0");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(discover_candidate(&path, &cache, |_| panic!("not executable")).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_ready_hints_are_bounded_and_only_lifecycle_events() {
        assert!(is_document_ready_event("Page.domContentEventFired"));
        assert!(is_document_ready_event("Page.loadEventFired"));
        for method in ["Page.frameNavigated", "Page.screencastFrame", "Runtime.consoleAPICalled", ""] {
            assert!(!is_document_ready_event(method));
        }
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(()).unwrap();
        assert!(matches!(tx.try_send(()), Err(mpsc::TrySendError::Full(()))));
        wait_for_document_hint(&rx);
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        tx.try_send(()).unwrap();
        wait_for_document_hint(&rx);
    }

    #[test]
    fn version_detection_is_strict() {
        assert_eq!(chrome_major("Google Chrome 143.0.12.6"), Some(143));
        assert_eq!(chrome_major("Google Chrome 136.0.0.0"), Some(136));
        assert_eq!(chrome_major("Chrome unknown"), None);
        assert_eq!(chrome_major("Chromium"), None);
    }

    #[test]
    fn profile_must_not_be_symlink_and_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("herdr-chrome-permission-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        private_profile(&dir).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let alias = dir.with_extension("link");
        let _ = fs::remove_file(&alias);
        std::os::unix::fs::symlink(&dir, &alias).unwrap();
        assert!(private_profile(&alias).is_err());
        fs::remove_file(alias).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chrome_real_cdp_fixture() {
        if std::env::var_os("HERDR_CHROME_E2E").is_none() { return }
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let keep = stop.clone();
        let server = std::thread::spawn(move || {
            while !keep.load(AtomicOrdering::Relaxed) {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        let mut request = [0u8; 4096];
                        // TCP may split a request line between reads. Read the
                        // bounded complete headers, not a single arbitrary chunk.
                        let _ = connection.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut n = 0;
                        while n < request.len() && !request[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                            match connection.read(&mut request[n..]) {
                                Ok(0) | Err(_) => break,
                                Ok(count) => n += count,
                            }
                        }
                        if !request[..n].windows(4).any(|w| w == b"\r\n\r\n") { continue }
                        // A parser-blocking script keeps readyState at "loading" while
                        // the actual target exists, exercising readiness wakeups.
                        if request[..n].starts_with(b"GET /delayed.js ") {
                            std::thread::sleep(Duration::from_millis(250));
                            let body = "window.__herdrDelayedScriptRan = true; document.title = 'Herdr delayed fixture';";
                            let _ = write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                            continue;
                        }
                        if request[..n].starts_with(b"GET /delayed ") {
                            let body = "<html><head><title>Loading</title><script src='/delayed.js'></script></head><body>Delayed document</body></html>";
                            let _ = write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                            continue;
                        }
                        let body = "<html><head><title>Herdr fixture</title></head><body><button id='click' onclick=\"document.getElementById('state').textContent='clicked'\">Click</button><span id='state'>idle</span><input id='text'><input id='otp' autocomplete='one-time-code' value='481902'><input id='card' autocomplete='cc-number' value='4111111111111111'><input id='pass' type='password' value='Secret1'><form onsubmit=\"event.preventDefault();window.__submitted=true\"><button id='disguised-submit' type='submit'>Search</button></form><a id='destructive-link' href='/delete/account'>Search</a><script>window.__submitted=false</script></body></html>";
                        let _ = write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        let profile = std::env::temp_dir().join(format!("herdr-chrome-e2e-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros()));
        let binary = discover().expect("Chrome must be installed for required CDP E2E");
        let client = Client::start(&binary, &profile).expect("Chrome CDP pipe must connect");
        let pane = Arc::new(client.open(&format!("http://{address}/")).expect("CDP target must open"));
        let mut title = Value::Null;
        for _ in 0..60 {
            title = pane.eval("document.title").unwrap_or(Value::Null);
            if title == "Herdr fixture" { break }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(title, "Herdr fixture");
        // Runtime remains usable without subscribing to context events.
        assert_eq!(pane.eval("6 * 7").unwrap(), 42);
        assert_eq!(pane.eval("Promise.resolve(43)").unwrap(), 43);
        let frame = pane.frame_timeout(Duration::from_secs(10)).expect("headless CDP must emit real screencast frames");
        assert!(frame.starts_with(b"\x89PNG\r\n\x1a\n"), "screencast must be PNG");
        let png = pane.screenshot().unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "true Page.captureScreenshot PNG");
        let rect = pane.eval("(()=>{let r=document.querySelector('#click').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()").unwrap();
        pane.click_at(rect["x"].as_f64().unwrap(), rect["y"].as_f64().unwrap()).unwrap();
        assert_eq!(pane.eval("document.querySelector('#state').textContent").unwrap(), "clicked");
        pane.eval("document.querySelector('#text').focus()").unwrap();
        pane.insert_text("Herdr CDP input").unwrap();
        assert_eq!(pane.eval("document.querySelector('#text').value").unwrap(), "Herdr CDP input");

        // API tests run against the same real Chrome target, not a mock CDP.
        let cancelled = AtomicBool::new(false);
        let query = crate::browser_api::run(&pane, "browser.query",
            &json!({"target":{"css":"button#click"}}), false, &cancelled)
            .expect("Chrome query over the real page");
        assert_eq!(query["count"], 1);
        assert_eq!(query["elements"][0]["tag"], "button");
        // Observe through the real CDP page, not a mocked DOM. Ordinary
        // text remains inspectable; credentials, OTPs and card numbers do not.
        for selector in ["#otp", "#card", "#pass"] {
            let value = crate::browser_api::run(&pane, "browser.inspect",
                &json!({"target":{"css":selector}}), false, &cancelled)
                .expect("bounded real Chrome DOM inspection");
            assert_eq!(value["elements"][0]["value"], "[REDACTED]", "{selector}");
        }
        let original = crate::browser_api::run(&pane, "browser.inspect",
            &json!({"target":{"css":"#text"}}), false, &cancelled).unwrap();
        assert_eq!(original["elements"][0]["value"], "Herdr CDP input");
        let error = crate::browser_api::run(&pane, "browser.fill",
            &json!({"target":{"css":"#otp"},"value":"123456"}), false, &cancelled)
            .expect_err("OTP must not be filled without native user approval");
        assert!(error.starts_with("APPROVAL_REQUIRED"));
        assert_eq!(pane.eval("document.getElementById('otp').value").unwrap(), "481902",
            "rejected sensitive typing must leave the original code unchanged");
        let snapshot = crate::browser_api::run(&pane, "browser.snapshot",
            &json!({"max_nodes":50}), false, &cancelled)
            .expect("Chrome DOM snapshot");
        assert_eq!(snapshot["title"], "Herdr fixture");
        assert!(snapshot["elements"].as_array().is_some_and(|v| !v.is_empty()));
        let serialized=snapshot.to_string();
        for secret in ["481902","4111111111111111","Secret1"] {
            assert!(!serialized.contains(secret),"Chrome snapshot leaked a sensitive fixture value");
        }
        assert!(snapshot["elements"].as_array().unwrap().iter()
            .any(|entry| entry["value"]=="[REDACTED]"),
            "snapshot must redacted sensitive inputs instead of bypassing inspect's serializer");
        let error = crate::browser_api::run(&pane, "browser.eval",
            &json!({"expression":"document.title"}), false, &cancelled)
            .expect_err("arbitrary JavaScript must require native user approval");
        assert!(error.starts_with("APPROVAL_REQUIRED"));
        let error = crate::browser_api::run(&pane, "browser.click",
            &json!({"target":{"css":"button#click"}}), false, &cancelled)
            .expect_err("uncertain action must require native approval");
        assert!(error.starts_with("APPROVAL_REQUIRED"));
        // The same native permission boundary must hold for all keyboard
        // activation aliases. Return and Space used to bypass the Enter gate.
        for key in ["Enter", "Return", "Space"] {
            let error = crate::browser_api::run(&pane, "browser.press",
                &json!({"key":key,"target":{"css":"#disguised-submit"}}), false, &cancelled)
                .expect_err("a keyboard shortcut cannot approve its own submission");
            assert!(error.starts_with("APPROVAL_REQUIRED"), "{key}: {error}");
        }
        assert_eq!(pane.eval("window.__submitted").unwrap(), false,
            "denied native key paths must not submit the form");
        // A label like 'Search' is not proof a button is safe: inspect its
        // actual submit semantics, and reject destructive URLs behind labels.
        for selector in ["#disguised-submit", "#destructive-link"] {
            let error = crate::browser_api::run(&pane, "browser.click",
                &json!({"target":{"css":selector}}), false, &cancelled)
                .expect_err("disguised irreversible action must require native approval");
            assert!(error.starts_with("APPROVAL_REQUIRED"), "{selector}: {error}");
        }
        assert_eq!(pane.eval("window.__submitted").unwrap(), false);
        assert_eq!(crate::browser_api::run(&pane, "browser.click",
            &json!({"target":{"css":"button#click"}}), true, &cancelled)
            .expect("native approved click")["clicked"], true);
        assert_eq!(crate::browser_api::run(&pane, "browser.fill",
            &json!({"target":{"css":"#text"},"value":"Approved replacement"}), false, &cancelled)
            .expect("native text replacement")["typed"], true);
        assert_eq!(pane.eval("document.querySelector('#text').value").unwrap(), "Approved replacement");
        let waited = crate::browser_api::run(&pane, "browser.wait",
            &json!({"condition":{"selector":"#state","text":"clicked"},"timeout_ms":1000}),
            false, &cancelled).expect("matched visible fixture");
        assert_eq!(waited["matched"], true);
        cancelled.store(true, AtomicOrdering::Release);
        assert!(crate::browser_api::run(&pane, "browser.type",
            &json!({"target":{"css":"#text"},"text":"must not type"}), true, &cancelled)
            .expect_err("cancellation gate") .starts_with("CANCELLED"));

        drop(pane);
        let delayed = client.open(&format!("http://{address}/delayed"))
            .expect("delayed document must become interactive before screencast");
        let state = delayed.eval("({href:location.href,ready:document.readyState})").unwrap();
        assert_eq!(state["href"], format!("http://{address}/delayed"));
        assert!(matches!(state["ready"].as_str(), Some("interactive" | "complete")));
        // The public open contract is an interactive document, not a particular
        // application title. Independently require that the real delayed script
        // executes; a missing or misrouted response still fails the test.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = delayed.eval("({title:document.title,ran:window.__herdrDelayedScriptRan===true})").unwrap();
            if state["ran"] == true && state["title"] == "Herdr delayed fixture" { break }
            assert!(Instant::now() < deadline, "delayed script did not execute: {state}");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(delayed.frame_timeout(Duration::from_secs(10)).unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
        // Verify the actual decoded pixels, not merely the PNG signature.
        // A static colored document avoids input focus/caret differences.
        delayed.eval("document.body.innerHTML='';document.body.style='margin:0;background:rgb(23,91,177)'").unwrap();
        delayed.eval("new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(()=>r(true))))").unwrap();
        let png = delayed.screenshot().unwrap();
        let decode = |bytes: &[u8]| image::load_from_memory_with_format(bytes,image::ImageFormat::Png).unwrap().to_rgba8();
        let pixels = decode(&png);
        assert_eq!(pixels.dimensions(), (1024,768));
        assert_eq!(pixels.get_pixel(512,384).0, [23,91,177,255]);
        delayed.resize(1200,1000).unwrap();
        let large = decode(&delayed.screenshot().unwrap());
        assert_eq!(large.dimensions(), (1200,1000));
        drop(delayed);
        drop(client);
        stop.store(true, AtomicOrdering::Relaxed);
        server.join().unwrap();
        // Chrome's helper processes can still flush Preferences/History
        // briefly after the headless browser exits. Retry only the ephemeral
        // test profile cleanup; never touch the persistent user's profile.
        let mut cleaned = false;
        for _ in 0..40 {
            match fs::remove_dir_all(&profile) {
                Ok(()) => { cleaned = true; break; }
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => panic!("cannot clean isolated Chrome test profile: {error}"),
            }
        }
        assert!(cleaned, "Chrome helper processes kept writing the test profile after shutdown");
    }
}
