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
use crate::app::{App, Effect, Msg, Payload};
use crate::cache::{ResourceKey, SnapshotStore};
use crate::canvas::CanvasClient;
use crate::config::Config;
use crate::diag::Diagnostics;
use crate::session::SessionFile;
use crate::transport::ReqwestTransport;
use crate::ui;

/// Starting value from the plan: at most four network operations at once.
const MAX_CONCURRENT: usize = 4;

pub struct Options {
    pub diag_file: Option<std::path::PathBuf>,
}

/// Everything network tasks need, shared across the session.
struct Executor {
    rt: tokio::runtime::Runtime,
    client: Arc<CanvasClient>,
    store: Arc<SnapshotStore>,
    session: Arc<Mutex<SessionFile>>,
    session_path: std::path::PathBuf,
    limit: Arc<Semaphore>,
    tx: Sender<Msg>,
}

impl Executor {
    fn fetch(&self, key: ResourceKey, request: u64) {
        let client = self.client.clone();
        let store = self.store.clone();
        let session = self.session.clone();
        let session_path = self.session_path.clone();
        let limit = self.limit.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let _permit = limit.acquire_owned().await;
            let result = match &key {
                ResourceKey::Courses => api::courses(&client)
                    .await
                    .map(|(v, s)| (Payload::Courses(v), s)),
                ResourceKey::Assignments(c) => api::assignment_list(&client, c)
                    .await
                    .map(|(v, s)| (Payload::Assignments(v), s)),
                ResourceKey::Detail(c, a) => api::assignment(&client, c, a)
                    .await
                    .map(|(v, s)| (Payload::Detail(Box::new(v)), s)),
            };
            let at = Utc::now();
            if let Ok((payload, _)) = &result {
                match payload {
                    Payload::Courses(v) => store.save(&key, v, at),
                    Payload::Assignments(v) => store.save(&key, v, at),
                    Payload::Detail(v) => store.save(&key, v.as_ref(), at),
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

fn persist_rotated_cookies(client: &CanvasClient, session: &Mutex<SessionFile>, path: &Path) {
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

fn setup(config: &Config, options: &Options, interactive_login: bool) -> Result<Setup, String> {
    let mut session = if interactive_login {
        load_or_login(config)?
    } else {
        SessionFile::load(&config.session_path(), &config.origin)
            .map_err(|e| e.0)?
            .ok_or("No saved session; run tuicanvas once to sign in.")?
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "Could not start the async runtime.".to_string())?;
    let diag = Arc::new(Diagnostics::new(options.diag_file.clone()));
    let transport = Arc::new(ReqwestTransport::new()?);
    let client = Arc::new(CanvasClient::new(
        &config.origin,
        &config.host,
        transport,
        session.jar.clone(),
        diag,
    ));
    // The helper records the account id after its own verified read; fall back to one read.
    let account = match session.user_id {
        Some(id) => id.to_string(),
        None => {
            let (profile, _) = rt
                .block_on(api::profile(&client))
                .map_err(|e| format!("Canvas: {}", e.describe()))?;
            session.user_id = Some(profile.id);
            profile.id.to_string()
        }
    };
    let store = Arc::new(SnapshotStore::new(&config.cache_root(), &account));
    let (tx, rx) = mpsc::channel();
    let executor = Executor {
        rt,
        client,
        store,
        session: Arc::new(Mutex::new(session)),
        session_path: config.session_path(),
        limit: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        tx,
    };
    Ok(Setup {
        executor,
        rx,
        account,
    })
}

pub fn run(config: &Config, options: Options) -> Result<(), String> {
    let Setup {
        mut executor,
        rx,
        mut account,
    } = setup(config, &options, true)?;
    let mut app = App::new(&config.origin, Some(executor.store.clone()));
    let input = Input::spawn(executor.tx.clone());
    let mut terminal =
        ratatui::try_init().map_err(|_| "Could not initialise the terminal.".to_string())?;
    if let Ok(size) = terminal.size() {
        app.size = (size.width, size.height);
    }
    let mut pending = app.start();
    let result = loop {
        let mut fatal = None;
        for effect in std::mem::take(&mut pending) {
            match effect {
                Effect::Fetch { key, request } => executor.fetch(key, request),
                Effect::OpenUrl(url) => {
                    // Only Canvas-origin URLs reach here (see App::browser_url).
                    if url.starts_with(&format!("{}/", config.origin)) {
                        app.notice = Some(match open::that_detached(&url) {
                            Ok(()) => "Opened in your browser.".into(),
                            Err(_) => "Could not open the browser.".into(),
                        });
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
            if terminal.draw(|f| ui::draw(f, &mut app)).is_err() {
                break Err("Terminal drawing failed.".to_string());
            }
            app.dirty = false;
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
        pending.extend(app.update(msg));
        // Coalesce whatever else arrived before drawing again.
        while let Ok(m) = rx.try_recv() {
            pending.extend(app.update(m));
        }
    };
    ratatui::restore();
    persist_rotated_cookies(&executor.client, &executor.session, &executor.session_path);
    result
}

/// Headless walk through the real app: courses -> course -> assignment, with
/// each screen rendered (as first shown and after sync) to text files.
pub fn dump(config: &Config, out: &Path, course: &str, assignment: &str) -> Result<(), String> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let options = Options { diag_file: None };
    let Setup { executor, rx, .. } = setup(config, &options, false)?;
    let mut app = App::new(&config.origin, Some(executor.store.clone()));
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
                    app.update(m);
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
