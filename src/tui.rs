//! Runtime: terminal setup, input thread, network task execution, sign-in
//! handoff to the login helper, and a headless render mode for verification.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::Semaphore;

use crate::api;
use crate::app::{App, CourseFilesData, Effect, Msg, Payload, TransferEvent};
use crate::cache::{ResourceKey, SnapshotStore};
use crate::canvas::CanvasClient;
use crate::config::Config;
use crate::diag::Diagnostics;
use crate::download::{DownloadError, Downloader};
use crate::library::Library;
use crate::model::CanvasFile;
use crate::session::SessionFile;
use crate::transport::ReqwestTransport;
use crate::ui;

/// Starting value from the plan: at most four network operations at once.
const MAX_CONCURRENT: usize = 4;
/// File downloads at once, separate from reads so a large download never blocks a screen.
const MAX_DOWNLOADS: usize = 2;
/// At most this often a download reports progress to the screen.
const PROGRESS_EVERY: Duration = Duration::from_millis(120);

pub struct Options {
    pub diag_file: Option<std::path::PathBuf>,
    /// When the process started, for startup timing.
    pub started: Instant,
}

/// Everything network tasks need, shared across the session.
pub(crate) struct Executor {
    rt: tokio::runtime::Runtime,
    pub(crate) client: Arc<CanvasClient>,
    pub(crate) store: Arc<SnapshotStore>,
    session: Arc<Mutex<SessionFile>>,
    session_path: std::path::PathBuf,
    limit: Arc<Semaphore>,
    downloads: Arc<Semaphore>,
    downloader: Option<Arc<Downloader>>,
    pub(crate) library: Arc<Library>,
    tx: Sender<Msg>,
}

impl Executor {
    pub(crate) fn session_handle(&self) -> Arc<Mutex<SessionFile>> {
        self.session.clone()
    }

    pub(crate) fn fetch(&self, key: ResourceKey, request: u64) {
        let client = self.client.clone();
        let store = self.store.clone();
        let session = self.session.clone();
        let session_path = self.session_path.clone();
        let limit = self.limit.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let _permit = limit.acquire_owned().await;
            let result = match &key {
                ResourceKey::Announcements(c) => crate::announcements::list(&client, c)
                    .await
                    .map(|(v, s)| (Payload::Announcements(v), s)),
                ResourceKey::Announcement(c, a) => crate::announcements::detail(&client, c, a)
                    .await
                    .map(|(v, s)| (Payload::Announcement(Box::new(v)), s)),
                ResourceKey::Courses => api::courses(&client)
                    .await
                    .map(|(v, s)| (Payload::Courses(v), s)),
                ResourceKey::Assignments(c) => api::assignment_list(&client, c)
                    .await
                    .map(|(v, s)| (Payload::Assignments(v), s)),
                ResourceKey::Detail(c, a) => api::assignment(&client, c, a)
                    .await
                    .map(|(v, s)| (Payload::Detail(Box::new(v)), s)),
                ResourceKey::Files(c) => api::course_files(&client, c).await.map(|(v, s)| {
                    let data = CourseFilesData {
                        folders: v.folders,
                        files: v.files,
                    };
                    (Payload::Files(Box::new(data)), s)
                }),
                ResourceKey::File(f) => api::file(&client, f)
                    .await
                    .map(|(v, s)| (Payload::File(Box::new(v)), s)),
            };
            let at = Utc::now();
            if let Ok((payload, _)) = &result {
                match payload {
                    Payload::Announcements(v) => store.save(&key, v, at),
                    Payload::Announcement(v) => store.save(&key, v.as_ref(), at),
                    Payload::Courses(v) => store.save(&key, v, at),
                    Payload::Assignments(v) => store.save(&key, v, at),
                    Payload::Detail(v) => store.save(&key, v.as_ref(), at),
                    Payload::Files(v) => store.save(&key, v.as_ref(), at),
                    Payload::File(v) => store.save(&key, v.as_ref(), at),
                }
            }
            persist_rotated_cookies(&client, &session, &session_path);
            let _ = tx.send(Msg::Fetched {
                key,
                request,
                result,
                at,
            });
        });
    }
}

impl Executor {
    /// Download a file into the library's partial folder, then install it at
    /// `target`. Progress, completion and failure arrive as `Msg::Transfer`.
    pub(crate) fn download(&self, file: CanvasFile, target: std::path::PathBuf, automatic: bool) {
        let tx = self.tx.clone();
        let id = file.id.clone();
        let send = move |event: TransferEvent| {
            let _ = tx.send(Msg::Transfer {
                file_id: id.clone(),
                event,
            });
        };
        let Some(downloader) = self.downloader.clone() else {
            send(TransferEvent::Failed {
                message: "downloads are unavailable".into(),
                needs_login: false,
            });
            return;
        };
        let (client, library, slots, origin) = (
            self.client.clone(),
            self.library.clone(),
            self.downloads.clone(),
            self.client.origin().to_string(),
        );
        self.rt.spawn(async move {
            let _slot = slots.acquire_owned().await;
            let Some(url) = file.url.clone() else {
                send(TransferEvent::Failed {
                    message: "file is locked on Canvas".into(),
                    needs_login: false,
                });
                return;
            };
            send(TransferEvent::Progress {
                received: 0,
                total: file.size,
            });
            let path = reqwest::Url::parse(&url)
                .map(|u| u.path().to_string())
                .unwrap_or_default();
            let cookie = client.cookie_header(&path);
            let partial = library.temp_dir().join(format!("{}.part", file.id));
            let last = Mutex::new(Instant::now());
            let progress = |received: u64, total: Option<u64>| {
                let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                if last.elapsed() >= PROGRESS_EVERY {
                    *last = Instant::now();
                    send(TransferEvent::Progress { received, total });
                }
            };
            let fetched = downloader
                .fetch(&url, cookie.as_deref(), &partial, &progress)
                .await;
            let outcome = match fetched {
                Ok(_) => {
                    mark_from_internet(&partial, &origin);
                    let (lib, part, dest, version, fid) = (
                        library.clone(),
                        partial.clone(),
                        target.clone(),
                        file.version(),
                        file.id.clone(),
                    );
                    tokio::task::spawn_blocking(move || {
                        lib.install(&fid, &part, version, &dest, automatic)
                    })
                    .await
                    .map_err(|_| DownloadError::Disk)
                    .and_then(|r| r.map_err(|_| DownloadError::Disk))
                }
                Err(e) => Err(e),
            };
            match outcome {
                Ok(()) => send(TransferEvent::Done(target)),
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    send(TransferEvent::Failed {
                        message: e.describe(),
                        needs_login: e == DownloadError::NeedsLogin,
                    });
                }
            }
        });
    }

    /// Remove auto-downloaded images unused for 30 days, off the UI thread.
    pub(crate) fn evict_unused_files(&self) {
        let library = self.library.clone();
        self.rt
            .spawn_blocking(move || library.evict_unused(Utc::now()));
    }
}

/// Tag a downloaded file the way browsers do (Mark of the Web), so Windows
/// and Office treat it as coming from the internet.
fn mark_from_internet(path: &Path, origin: &str) {
    #[cfg(windows)]
    {
        let stream = format!("{}:Zone.Identifier", path.display());
        let _ = std::fs::write(
            stream,
            format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={origin}/\r\n"),
        );
    }
    #[cfg(not(windows))]
    let _ = (path, origin);
}

/// Show a file selected in its folder, or open a folder (created if missing).
fn reveal(path: &Path) -> Result<(), ()> {
    if path.is_file() {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            return std::process::Command::new("explorer.exe")
                .raw_arg(format!("/select,\"{}\"", path.display()))
                .spawn()
                .map(|_| ())
                .map_err(|_| ());
        }
        #[cfg(not(windows))]
        return open::that_detached(path.parent().unwrap_or(path)).map_err(|_| ());
    }
    std::fs::create_dir_all(path).map_err(|_| ())?;
    open::that_detached(path).map_err(|_| ())
}

/// Addresses `o` and description links may open: web pages and email only.
fn openable(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:"))
        && !url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '"')
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the file".into())
}

pub(crate) fn persist_rotated_cookies(
    client: &CanvasClient,
    session: &Mutex<SessionFile>,
    path: &Path,
) {
    if let Some(jar) = client.take_changed_jar() {
        let mut s = session.lock().unwrap_or_else(|e| e.into_inner());
        s.jar = jar;
        let _ = s.save(path);
    }
}

/// Reads terminal input on its own thread; can be parked while the login
/// helper owns the terminal.
struct Input {
    paused: Arc<AtomicBool>,
    parked: Arc<AtomicBool>,
}

impl Input {
    fn spawn(tx: Sender<Msg>) -> Self {
        let paused = Arc::new(AtomicBool::new(false));
        let parked = Arc::new(AtomicBool::new(false));
        let (p, k) = (paused.clone(), parked.clone());
        std::thread::spawn(move || {
            loop {
                if p.load(Ordering::SeqCst) {
                    k.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                k.store(false, Ordering::SeqCst);
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => {
                        let msg = match event::read() {
                            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                                Msg::Key(key)
                            }
                            Ok(Event::Resize(w, h)) => Msg::Resize(w, h),
                            Ok(_) => continue,
                            Err(_) => break,
                        };
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        });
        Input { paused, parked }
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !self.parked.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }
}

/// Run the Python/Playwright sign-in helper in the foreground terminal.
pub fn run_login_helper(config: &Config) -> Result<(), String> {
    let (python, dir) = config.login_helper().ok_or_else(|| {
        format!(
            "Sign-in helper not found under {}. Set TUICANVAS_HOME to the project folder.",
            config.project_dir.display()
        )
    })?;
    let status = std::process::Command::new(python)
        .args(["-m", "tools.terminal_login"])
        .current_dir(dir)
        .status()
        .map_err(|_| "Could not start the sign-in helper.".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Sign-in did not complete.".to_string())
    }
}

fn load_or_login(config: &Config) -> Result<SessionFile, String> {
    match SessionFile::load(&config.session_path(), &config.origin).map_err(|e| e.0)? {
        Some(s) => Ok(s),
        None => {
            run_login_helper(config)?;
            SessionFile::load(&config.session_path(), &config.origin)
                .map_err(|e| e.0)?
                .ok_or_else(|| "Sign-in did not save a session.".to_string())
        }
    }
}

struct Setup {
    executor: Executor,
    rx: Receiver<Msg>,
    account: String,
}

/// A runtime, one pooled HTTPS client and a snapshot store rooted at `cache_root`.
pub(crate) fn build_executor(
    config: &Config,
    session: SessionFile,
    account: &str,
    cache_root: &Path,
    diag: Arc<Diagnostics>,
) -> Result<(Executor, Receiver<Msg>), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "Could not start the async runtime.".to_string())?;
    let transport = Arc::new(ReqwestTransport::new()?);
    let client = Arc::new(CanvasClient::new(
        &config.origin,
        &config.host,
        transport,
        session.jar.clone(),
        diag,
    ));
    let (tx, rx) = mpsc::channel();
    let executor = Executor {
        rt,
        client,
        store: Arc::new(SnapshotStore::new(cache_root, account)),
        session: Arc::new(Mutex::new(session)),
        session_path: config.session_path(),
        limit: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        downloads: Arc::new(Semaphore::new(MAX_DOWNLOADS)),
        downloader: Downloader::new(&config.host).ok().map(Arc::new),
        library: Arc::new(Library::open(&config.library_root(), &config.host)),
        tx,
    };
    Ok((executor, rx))
}

/// The saved session's account id, read once from Canvas if the helper did not record it.
pub(crate) fn account_of(config: &Config, session: &mut SessionFile) -> Result<String, String> {
    if let Some(id) = session.user_id {
        return Ok(id.to_string());
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Could not start the async runtime.".to_string())?;
    let client = CanvasClient::new(
        &config.origin,
        &config.host,
        Arc::new(ReqwestTransport::new()?),
        session.jar.clone(),
        Arc::new(Diagnostics::new(None)),
    );
    let (profile, _) = rt
        .block_on(api::profile(&client))
        .map_err(|e| format!("Canvas: {}", e.describe()))?;
    session.user_id = Some(profile.id);
    Ok(profile.id.to_string())
}

fn setup(config: &Config, options: &Options, interactive_login: bool) -> Result<Setup, String> {
    let mut session = if interactive_login {
        load_or_login(config)?
    } else {
        SessionFile::load(&config.session_path(), &config.origin)
            .map_err(|e| e.0)?
            .ok_or("No saved session; run tuicanvas once to sign in.")?
    };
    let account = account_of(config, &mut session)?;
    let diag = Arc::new(Diagnostics::new(options.diag_file.clone()));
    let (executor, rx) = build_executor(config, session, &account, &config.cache_root(), diag)?;
    Ok(Setup {
        executor,
        rx,
        account,
    })
}

/// Timing of what the user sees, written to the diagnostics file with --diag:
/// startup to first frame, each key to the frame it produced, and each read
/// from the action that caused it to the frame showing its data.
struct Trace {
    on: bool,
    started: Instant,
    first_frame: bool,
    /// Messages handled since the last frame: key presses and applied reads.
    keys: Vec<Instant>,
    fetched: Vec<(u64, Instant, crate::canvas::FetchStats)>,
    /// Read request id -> (what caused it, resource label).
    origins: std::collections::HashMap<u64, (Instant, &'static str)>,
    cause: Option<Instant>,
}

impl Trace {
    fn received(&mut self, msg: &Msg) {
        if !self.on {
            return;
        }
        let now = Instant::now();
        match msg {
            Msg::Key(_) => {
                self.keys.push(now);
                self.cause = Some(now);
            }
            Msg::Fetched {
                request,
                result: Ok((_, stats)),
                ..
            } => self.fetched.push((*request, now, stats.clone())),
            _ => {}
        }
    }

    fn fetch_started(&mut self, request: u64, key: &ResourceKey) {
        if self.on {
            let cause = self.cause.unwrap_or_else(Instant::now);
            self.origins.insert(request, (cause, key.label()));
        }
    }

    fn drawn(&mut self, diag: &Diagnostics, draw_ms: f64, view: &str, applied: Vec<u64>) {
        if !self.on {
            return;
        }
        let now = Instant::now();
        let ms = |since: Instant| (now - since).as_secs_f64() * 1000.0;
        if !self.first_frame {
            self.first_frame = true;
            diag.event(serde_json::json!({"event": "first_frame", "since_start_ms": ms(self.started), "draw_ms": draw_ms}));
        }
        for key_at in self.keys.drain(..) {
            diag.event(serde_json::json!({"event": "key_to_frame", "view": view, "ms": ms(key_at), "draw_ms": draw_ms}));
        }
        for (request, received_at, stats) in self.fetched.drain(..) {
            if !applied.contains(&request) {
                continue;
            }
            let Some((cause, label)) = self.origins.remove(&request) else {
                continue;
            };
            diag.event(serde_json::json!({
                "event": "fresh_frame", "resource": label,
                "action_to_frame_ms": ms(cause),
                "network_ms": stats.network_ms, "parse_ms": stats.parse_ms,
                "response_to_frame_ms": stats.parse_ms + ms(received_at),
                "requests": stats.requests, "bytes": stats.bytes, "draw_ms": draw_ms,
            }));
        }
        self.cause = None;
    }
}

pub fn run(config: &Config, options: Options) -> Result<(), String> {
    let Setup {
        mut executor,
        rx,
        mut account,
    } = setup(config, &options, true)?;
    let mut app = App::new(&config.origin, Some(executor.store.clone()));
    app.library = Some(executor.library.clone());
    executor.evict_unused_files();
    let input = Input::spawn(executor.tx.clone());
    let mut terminal =
        ratatui::try_init().map_err(|_| "Could not initialise the terminal.".to_string())?;
    if let Ok(size) = terminal.size() {
        app.size = (size.width, size.height);
    }
    let mut trace = Trace {
        on: executor.client.diag.enabled(),
        started: options.started,
        first_frame: false,
        keys: Vec::new(),
        fetched: Vec::new(),
        origins: Default::default(),
        cause: Some(options.started),
    };
    let mut pending = app.start();
    let result = loop {
        let mut fatal = None;
        for effect in std::mem::take(&mut pending) {
            match effect {
                Effect::Fetch { key, request } => {
                    trace.fetch_started(request, &key);
                    executor.fetch(key, request)
                }
                Effect::OpenUrl(url) => {
                    app.notice = Some(if !openable(&url) {
                        "That link is not a web page.".into()
                    } else {
                        match open::that_detached(&url) {
                            Ok(()) if url.starts_with("mailto:") => "Opened your email app.".into(),
                            Ok(()) => "Opened in your browser.".into(),
                            Err(_) => "Could not open the browser.".into(),
                        }
                    });
                    app.dirty = true;
                }
                Effect::Download {
                    file,
                    target,
                    automatic,
                } => executor.download(*file, target, automatic),
                Effect::OpenPath(path) => {
                    let name = file_label(&path);
                    app.notice = Some(if crate::library::is_program(&name) {
                        // Never run downloaded programs; show them instead.
                        let _ = reveal(&path);
                        format!(
                            "{name} is a program, so it was shown in its folder instead of opened."
                        )
                    } else {
                        match open::that_detached(&path) {
                            Ok(()) => format!("Opened {name}."),
                            Err(_) => format!("No app is set to open {name}."),
                        }
                    });
                    app.dirty = true;
                }
                Effect::Reveal(path) => {
                    if reveal(&path).is_err() {
                        app.notice = Some("Could not open the folder.".into());
                        app.dirty = true;
                    }
                }
                Effect::Login => {
                    input.pause();
                    ratatui::restore();
                    let outcome = run_login_helper(config).and_then(|_| {
                        SessionFile::load(&config.session_path(), &config.origin)
                            .map_err(|e| e.0)?
                            .ok_or_else(|| "Sign-in did not save a session.".to_string())
                    });
                    terminal = match ratatui::try_init() {
                        Ok(t) => t,
                        Err(_) => {
                            fatal = Some("Could not restore the terminal.".to_string());
                            break;
                        }
                    };
                    let _ = terminal.clear();
                    input.resume();
                    let msg = match outcome {
                        Ok(session) => {
                            let new_account = session.user_id.map(|u| u.to_string());
                            executor.client.replace_jar(session.jar.clone());
                            *executor.session.lock().unwrap_or_else(|e| e.into_inner()) = session;
                            if new_account.as_deref().is_some_and(|a| a != account) {
                                // A different account: start over so nothing from the previous account is shown.
                                account = new_account.unwrap_or_default();
                                executor.store =
                                    Arc::new(SnapshotStore::new(&config.cache_root(), &account));
                                app = App::new(&config.origin, Some(executor.store.clone()));
                                app.library = Some(executor.library.clone());
                            }
                            Msg::LoginFinished {
                                ok: true,
                                message: "Signed in.".into(),
                            }
                        }
                        Err(e) => Msg::LoginFinished {
                            ok: false,
                            message: e,
                        },
                    };
                    if let Ok(size) = terminal.size() {
                        app.size = (size.width, size.height);
                    }
                    pending.extend(app.update(msg));
                }
            }
        }
        if let Some(error) = fatal {
            break Err(error);
        }
        if !pending.is_empty() {
            continue;
        }
        if app.quit {
            break Ok(());
        }
        if app.dirty {
            let draw_started = Instant::now();
            if terminal.draw(|f| ui::draw(f, &mut app)).is_err() {
                break Err("Terminal drawing failed.".to_string());
            }
            app.dirty = false;
            let view = app.view().key().label();
            let applied = std::mem::take(&mut app.applied);
            trace.drawn(
                &executor.client.diag,
                draw_started.elapsed().as_secs_f64() * 1000.0,
                view,
                applied,
            );
        }
        let msg = if app.current_syncing() {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => Msg::Tick,
                Err(RecvTimeoutError::Disconnected) => break Ok(()),
            }
        } else {
            match rx.recv() {
                Ok(m) => m,
                Err(_) => break Ok(()),
            }
        };
        trace.received(&msg);
        pending.extend(app.update(msg));
        // Coalesce whatever else arrived before drawing again.
        while let Ok(m) = rx.try_recv() {
            trace.received(&m);
            pending.extend(app.update(m));
        }
    };
    ratatui::restore();
    persist_rotated_cookies(&executor.client, &executor.session, &executor.session_path);
    if let Some(usage) = crate::procstats::usage() {
        executor.client.diag.event(serde_json::json!({"event": "exit", "cpu_ms": usage.cpu_ms, "peak_memory_mb": usage.peak_memory_mb}));
    }
    result
}

/// Headless walk through the real app: courses -> course -> assignment, with
/// each screen rendered (as first shown and after sync) to text files.
pub fn dump(config: &Config, out: &Path, course: &str, assignment: &str) -> Result<(), String> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let options = Options {
        diag_file: None,
        started: Instant::now(),
    };
    let Setup {
        mut executor, rx, ..
    } = setup(config, &options, false)?;
    let mut app = App::new(&config.origin, Some(executor.store.clone()));
    // Downloads go to a scratch library, never the user's.
    let scratch = config.data_dir.join("dump-library");
    let _ = std::fs::remove_dir_all(&scratch);
    executor.library = Arc::new(Library::open(&scratch, &config.host));
    app.library = Some(executor.library.clone());
    let (w, h) = (110u16, 34u16);
    app.size = (w, h);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).map_err(|_| "backend")?;
    std::fs::create_dir_all(out).map_err(|_| "Could not create the dump folder.")?;
    let mut timings = serde_json::Map::new();

    let mut render = |app: &mut App,
                      name: &str,
                      timings: &mut serde_json::Map<String, serde_json::Value>|
     -> Result<(), String> {
        let t = Instant::now();
        terminal
            .draw(|f| ui::draw(f, app))
            .map_err(|_| "draw failed")?;
        timings.insert(
            format!("{name}_render_ms"),
            serde_json::json!((t.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0),
        );
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            text.push_str(line.trim_end());
            text.push('\n');
        }
        std::fs::write(out.join(format!("dump-{name}.txt")), text)
            .map_err(|_| "write failed".to_string())
    };

    let mut step = |app: &mut App,
                    effects: Vec<Effect>,
                    name: &str,
                    timings: &mut serde_json::Map<String, serde_json::Value>|
     -> Result<(), String> {
        let started = Instant::now();
        let mut waiting = 0;
        for e in effects {
            if let Effect::Fetch { key, request } = e {
                executor.fetch(key, request);
                waiting += 1;
            }
        }
        render(app, &format!("{name}-initial"), timings)?;
        while waiting > 0 {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(m @ Msg::Fetched { .. }) => {
                    waiting -= 1;
                    // Follow-up reads, such as the details of an assignment's files.
                    for e in app.update(m) {
                        if let Effect::Fetch { key, request } = e {
                            executor.fetch(key, request);
                            waiting += 1;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => return Err(format!("{name}: timed out waiting for Canvas")),
            }
        }
        timings.insert(
            format!("{name}_sync_ms"),
            serde_json::json!((started.elapsed().as_secs_f64() * 1000.0).round()),
        );
        render(app, &format!("{name}-synced"), timings)
    };

    let press = |code| Msg::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let effects = app.start();
    step(&mut app, effects, "1-courses", &mut timings)?;
    let find = |app: &App, needle: &str| {
        let needle = needle.to_lowercase();
        app.visible_rows()
            .iter()
            .position(|&i| match &app.view().kind {
                crate::app::ViewKind::Courses => app
                    .courses
                    .data
                    .as_ref()
                    .is_some_and(|l| l[i].title().to_lowercase().contains(&needle)),
                crate::app::ViewKind::Assignments { course_id, .. } => app
                    .assignments
                    .get(course_id)
                    .and_then(|r| r.data.as_ref())
                    .is_some_and(|l| l[i].title().to_lowercase().contains(&needle)),
                _ => false,
            })
    };
    let index = find(&app, course).ok_or("Course not found in the list.")?;
    for _ in 0..index {
        app.update(press(KeyCode::Down));
    }
    let effects = app.update(press(KeyCode::Enter));
    step(&mut app, effects, "2-assignments", &mut timings)?;
    if let Some(index) = find(&app, assignment) {
        for _ in 0..index {
            app.update(press(KeyCode::Down));
        }
    }
    let effects = app.update(press(KeyCode::Enter));
    step(&mut app, effects, "3-detail", &mut timings)?;
    let effects = app.update(press(KeyCode::Esc));
    step(&mut app, effects, "4-back-to-assignments", &mut timings)?;
    let effects = app.update(press(KeyCode::Tab));
    step(&mut app, effects, "5-files", &mut timings)?;
    // Into the folder with the most files (no read: the listing covers the course).
    let count = |folder: &str| {
        app.course_files
            .values()
            .filter_map(|r| r.data.as_ref()?.files.as_ref())
            .flatten()
            .filter(|f| f.folder_id.as_deref() == Some(folder))
            .count()
    };
    let busiest = app
        .file_rows()
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            crate::app::FileRow::Folder(f) => Some((count(&f.id), i)),
            _ => None,
        })
        .max()
        .filter(|(n, _)| *n > 0)
        .map(|(_, i)| i);
    if let Some(index) = busiest {
        for _ in 0..index {
            app.update(press(KeyCode::Down));
        }
        let effects = app.update(press(KeyCode::Enter));
        step(&mut app, effects, "6-files-folder", &mut timings)?;
    }
    // One real download (the smallest file here) through the reader's own path.
    let smallest = app
        .file_rows()
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            crate::app::FileRow::File(f) if f.url.is_some() && f.locked_for_user != Some(true) => {
                Some((f.size.unwrap_or(u64::MAX), i))
            }
            _ => None,
        })
        .min()
        .map(|(_, i)| i);
    if let Some(index) = smallest {
        app.update(press(KeyCode::Home));
        for _ in 0..index {
            app.update(press(KeyCode::Down));
        }
        let started = Instant::now();
        let mut target = None;
        for e in app.update(press(KeyCode::Char('d'))) {
            if let Effect::Download {
                file,
                target: t,
                automatic,
            } = e
            {
                target = Some(t.clone());
                executor.download(*file, t, automatic);
            }
        }
        if let Some(target) = target {
            loop {
                match rx.recv_timeout(Duration::from_secs(120)) {
                    Ok(m @ Msg::Transfer { .. }) => {
                        let finished = matches!(
                            &m,
                            Msg::Transfer {
                                event: TransferEvent::Done(_) | TransferEvent::Failed { .. },
                                ..
                            }
                        );
                        app.update(m);
                        if finished {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return Err("7-download: timed out".into()),
                }
            }
            let ms = (started.elapsed().as_secs_f64() * 1000.0).round();
            timings.insert("7-download_ms".into(), serde_json::json!(ms));
            timings.insert(
                "7-download_saved".into(),
                serde_json::json!(target.is_file()),
            );
            timings.insert(
                "7-download_marked_from_internet".into(),
                serde_json::json!(
                    std::fs::metadata(format!("{}:Zone.Identifier", target.display())).is_ok()
                ),
            );
            timings.insert(
                "7-download_notice".into(),
                serde_json::json!(app.notice.is_some()),
            );
            render(&mut app, "7-download", &mut timings)?;
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::write(
        out.join("dump-timings.json"),
        serde_json::to_string_pretty(&timings).unwrap_or_default(),
    )
    .map_err(|_| "write failed".to_string())?;
    persist_rotated_cookies(&executor.client, &executor.session, &executor.session_path);
    println!(
        "{}",
        serde_json::to_string_pretty(&timings).unwrap_or_default()
    );
    println!("Rendered screens written to {}", out.display());
    Ok(())
}
