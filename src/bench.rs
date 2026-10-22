//! `tuicanvas --bench`: repeat the selected workflow (start -> course list ->
//! course -> assignment -> back) and time what a user would see, separating
//! local work, network, parsing and drawing. Uses a private snapshot folder,
//! so the user's own saved copies are untouched. Reports contain timings and
//! sizes only.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Serialize;
use serde_json::{Value, json};

use crate::app::{App, Effect, Msg, ViewKind};
use crate::canvas::FetchStats;
use crate::config::Config;
use crate::diag::Diagnostics;
use crate::session::SessionFile;
use crate::tui::{Executor, account_of, build_executor, persist_rotated_cookies};
use crate::ui;

pub struct BenchOptions {
    pub runs: usize,
    pub cold: bool,
    pub warm: bool,
    /// Draw to the real terminal (includes terminal output cost) instead of an in-memory buffer.
    pub terminal: bool,
    pub course: String,
    pub assignment: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub step: &'static str,
    /// Action (key press, or process start for the first step) to the first frame drawn.
    pub first_frame_ms: f64,
    /// Whether that first frame already showed content (a saved copy) rather than "Loading".
    pub first_frame_has_content: bool,
    /// Action to the frame showing data from Canvas.
    pub fresh_frame_ms: f64,
    pub network_ms: f64,
    pub parse_ms: f64,
    /// Response received to fresh frame drawn: parse + apply + render.
    pub response_to_frame_ms: f64,
    pub first_draw_ms: f64,
    pub fresh_draw_ms: f64,
    pub requests: u32,
    pub bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct Run {
    pub condition: &'static str,
    pub run: usize,
    pub steps: Vec<Step>,
    /// Local key (selection move) to drawn frame, no network involved.
    pub move_key_to_frame_ms: f64,
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

fn press(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn draw<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<f64, String> {
    let started = Instant::now();
    terminal
        .draw(|f| ui::draw(f, app))
        .map_err(|_| "draw failed".to_string())?;
    app.dirty = false;
    Ok(ms(started))
}

fn current_has_data(app: &App) -> bool {
    match &app.view().kind {
        ViewKind::Courses => app.courses.data.is_some(),
        ViewKind::Assignments { course_id, .. } => app
            .assignments
            .get(course_id)
            .is_some_and(|r| r.data.is_some()),
        ViewKind::Detail {
            course_id,
            assignment_id,
            ..
        } => app
            .details
            .get(&(course_id.clone(), assignment_id.clone()))
            .is_some_and(|r| r.data.is_some()),
        ViewKind::Announcements { course_id, .. } => app
            .announcements
            .get(course_id)
            .is_some_and(|r| r.data.is_some()),
        ViewKind::Announcement {
            course_id,
            announcement_id,
            ..
        } => app
            .announcement_details
            .get(&(course_id.clone(), announcement_id.clone()))
            .is_some_and(|r| r.data.is_some()),
        ViewKind::Modules { course_id, .. } => {
            app.modules.get(course_id).is_some_and(|r| r.data.is_some())
        }
        ViewKind::WikiPage {
            course_id,
            page_url,
            ..
        } => app
            .wiki_pages
            .get(&(course_id.clone(), page_url.clone()))
            .is_some_and(|r| r.data.is_some()),
        ViewKind::CourseHome { course_id, .. } => {
            app.tabs.get(course_id).is_some_and(|r| r.data.is_some())
        }
        ViewKind::Files { course_id, .. } => app
            .course_files
            .get(course_id)
            .is_some_and(|r| r.data.is_some()),
    }
}

/// Dispatch the reads an action produced, draw the first frame, wait for the
/// reads, then draw the fresh frame.
fn step<B: Backend>(
    name: &'static str,
    terminal: &mut Terminal<B>,
    app: &mut App,
    executor: &Executor,
    rx: &Receiver<Msg>,
    effects: Vec<Effect>,
    action_at: Instant,
) -> Result<Step, String> {
    let mut waiting = 0;
    for effect in effects {
        if let Effect::Fetch { key, request } = effect {
            executor.fetch(key, request);
            waiting += 1;
        }
    }
    let first_draw_ms = draw(terminal, app)?;
    let first_frame_ms = ms(action_at);
    let first_frame_has_content = current_has_data(app);
    let mut stats = FetchStats::default();
    let mut received_at = Instant::now();
    while waiting > 0 {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Msg::Fetched {
                key,
                request,
                result,
                at,
            }) => {
                waiting -= 1;
                received_at = Instant::now();
                match &result {
                    Ok((_, s)) => {
                        stats.requests += s.requests;
                        stats.bytes += s.bytes;
                        stats.network_ms += s.network_ms;
                        stats.parse_ms += s.parse_ms;
                    }
                    Err(e) => return Err(format!("{name}: {}", e.describe())),
                }
                app.update(Msg::Fetched {
                    key,
                    request,
                    result,
                    at,
                });
            }
            Ok(_) => {}
            Err(_) => return Err(format!("{name}: timed out waiting for Canvas")),
        }
    }
    let fresh_draw_ms = draw(terminal, app)?;
    Ok(Step {
        step: name,
        first_frame_ms,
        first_frame_has_content,
        fresh_frame_ms: ms(action_at),
        network_ms: stats.network_ms,
        parse_ms: stats.parse_ms,
        response_to_frame_ms: stats.parse_ms + ms(received_at),
        first_draw_ms,
        fresh_draw_ms,
        requests: stats.requests,
        bytes: stats.bytes,
    })
}

fn find_row(app: &App, needle: &str) -> Option<usize> {
    let needle = needle.to_lowercase();
    app.visible_rows()
        .iter()
        .position(|&i| match &app.view().kind {
            ViewKind::Courses => app
                .courses
                .data
                .as_ref()
                .is_some_and(|l| l[i].title().to_lowercase().contains(&needle)),
            ViewKind::Assignments { course_id, .. } => app
                .assignments
                .get(course_id)
                .and_then(|r| r.data.as_ref())
                .is_some_and(|l| l[i].title().to_lowercase().contains(&needle)),
            ViewKind::Detail { .. }
            | ViewKind::Announcement { .. }
            | ViewKind::WikiPage { .. }
            | ViewKind::CourseHome { .. } => false,
            ViewKind::Modules { course_id, .. } => {
                matches!(app.module_rows(course_id).get(i), Some(crate::app::ModuleRow::Item(_, it))
                    if it.title().to_lowercase().contains(&needle))
            }
            ViewKind::Announcements { course_id, .. } => app
                .announcements
                .get(course_id)
                .and_then(|r| r.data.as_ref())
                .is_some_and(|l| l[i].title().to_lowercase().contains(&needle)),
            ViewKind::Files { .. } => app
                .file_rows()
                .get(i)
                .is_some_and(|r| r.name().to_lowercase().contains(&needle)),
        })
}

#[allow(clippy::too_many_arguments)]
fn run_once<B: Backend>(
    condition: &'static str,
    index: usize,
    terminal: &mut Terminal<B>,
    executor: &Executor,
    rx: &Receiver<Msg>,
    origin: &str,
    opts: &BenchOptions,
    started: Instant,
) -> Result<Run, String> {
    let mut app = App::new(origin, Some(executor.store.clone()));
    if let Ok(size) = terminal.size() {
        app.size = (size.width, size.height);
    }
    let mut steps = Vec::new();
    let effects = app.start();
    steps.push(step(
        "open course list",
        terminal,
        &mut app,
        executor,
        rx,
        effects,
        started,
    )?);

    let row = find_row(&app, &opts.course).ok_or("Course not found in the list.")?;
    for _ in 0..row {
        app.update(press(KeyCode::Down));
    }
    let at = Instant::now();
    let effects = app.update(press(KeyCode::Enter));
    steps.push(step(
        "open course",
        terminal,
        &mut app,
        executor,
        rx,
        effects,
        at,
    )?);

    // A course opens on its first Canvas tab; the timed steps use Assignments.
    for _ in 0..4 {
        if matches!(app.view().kind, ViewKind::Assignments { .. }) {
            break;
        }
        let effects = app.update(press(KeyCode::Tab));
        step(
            "switch tab",
            terminal,
            &mut app,
            executor,
            rx,
            effects,
            Instant::now(),
        )?;
    }

    // A purely local key: move the selection and back.
    let mut moves = Vec::new();
    for code in [KeyCode::Down, KeyCode::Up] {
        let at = Instant::now();
        app.update(press(code));
        draw(terminal, &mut app)?;
        moves.push(ms(at));
    }

    let row = find_row(&app, &opts.assignment).unwrap_or(0);
    for _ in 0..row {
        app.update(press(KeyCode::Down));
    }
    let at = Instant::now();
    let effects = app.update(press(KeyCode::Enter));
    steps.push(step(
        "open assignment",
        terminal,
        &mut app,
        executor,
        rx,
        effects,
        at,
    )?);

    let at = Instant::now();
    let effects = app.update(press(KeyCode::Esc));
    steps.push(step(
        "back to list",
        terminal,
        &mut app,
        executor,
        rx,
        effects,
        at,
    )?);

    Ok(Run {
        condition,
        run: index,
        steps,
        move_key_to_frame_ms: moves.iter().sum::<f64>() / moves.len() as f64,
    })
}

#[derive(Debug, Serialize)]
pub struct Spread {
    pub median: f64,
    pub min: f64,
    pub max: f64,
}

pub fn spread(values: &[f64]) -> Spread {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let median = if v.is_empty() {
        0.0
    } else if v.len() % 2 == 1 {
        v[v.len() / 2]
    } else {
        (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0
    };
    let round = |x: f64| (x * 10.0).round() / 10.0;
    Spread {
        median: round(median),
        min: round(*v.first().unwrap_or(&0.0)),
        max: round(*v.last().unwrap_or(&0.0)),
    }
}

fn summarize(runs: &[Run]) -> Value {
    let mut out = serde_json::Map::new();
    for condition in ["cold", "warm"] {
        let rs: Vec<&Run> = runs.iter().filter(|r| r.condition == condition).collect();
        if rs.is_empty() {
            continue;
        }
        // An array, so steps stay in workflow order.
        let mut steps = Vec::new();
        for (i, name) in rs[0].steps.iter().map(|s| s.step).enumerate() {
            let pick = |f: fn(&Step) -> f64| {
                spread(&rs.iter().map(|r| f(&r.steps[i])).collect::<Vec<_>>())
            };
            steps.push(json!({
                    "step": name,
                    "first_frame_ms": pick(|s| s.first_frame_ms),
                    "first_frame_has_content": rs.iter().filter(|r| r.steps[i].first_frame_has_content).count(),
                    "fresh_frame_ms": pick(|s| s.fresh_frame_ms),
                    "network_ms": pick(|s| s.network_ms),
                    "parse_ms": pick(|s| s.parse_ms),
                    "response_to_frame_ms": pick(|s| s.response_to_frame_ms),
                    "draw_ms": pick(|s| s.fresh_draw_ms),
                    "requests": rs[0].steps[i].requests,
                    "bytes": pick(|s| s.bytes as f64),
                }));
        }
        let moves: Vec<f64> = rs.iter().map(|r| r.move_key_to_frame_ms).collect();
        out.insert(
            condition.to_string(),
            json!({"runs": rs.len(), "steps": steps, "move_key_to_frame_ms": spread(&moves)}),
        );
    }
    Value::Object(out)
}

fn print_summary(summary: &Value) {
    for condition in ["cold", "warm"] {
        let Some(c) = summary.get(condition) else {
            continue;
        };
        println!("\n{condition} ({} runs) - median [min-max] ms", c["runs"]);
        println!(
            "  {:<18} {:>22} {:>22} {:>14} {:>12} {:>16}",
            "step", "to first frame", "to fresh frame", "network", "parse", "response->frame"
        );
        if let Some(steps) = c["steps"].as_array() {
            for s in steps {
                let name = s["step"].as_str().unwrap_or("");
                let f = |k: &str| format!("{} [{}-{}]", s[k]["median"], s[k]["min"], s[k]["max"]);
                println!(
                    "  {:<18} {:>22} {:>22} {:>14} {:>12} {:>16}",
                    name,
                    f("first_frame_ms"),
                    f("fresh_frame_ms"),
                    s["network_ms"]["median"].to_string(),
                    s["parse_ms"]["median"].to_string(),
                    s["response_to_frame_ms"]["median"].to_string()
                );
            }
        }
        println!(
            "  move selection (local key -> frame): {} ms median",
            c["move_key_to_frame_ms"]["median"]
        );
    }
}

fn reset_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn run_all<B: Backend>(
    terminal: &mut Terminal<B>,
    config: &Config,
    opts: &BenchOptions,
    session: &SessionFile,
    account: &str,
    cache_root: &Path,
) -> Result<Vec<Run>, String> {
    let diag = Arc::new(Diagnostics::new(None));
    let mut runs = Vec::new();
    for i in 0..opts.runs {
        if opts.cold {
            // New process state: fresh connection pool, no saved copies.
            reset_dir(cache_root);
            let started = Instant::now();
            let (executor, rx) =
                build_executor(config, session.clone(), account, cache_root, diag.clone())?;
            runs.push(run_once(
                "cold",
                i + 1,
                terminal,
                &executor,
                &rx,
                &config.origin,
                opts,
                started,
            )?);
            persist_rotated_cookies(
                &executor.client,
                &executor.session_handle(),
                &config.session_path(),
            );
        }
    }
    if opts.warm {
        reset_dir(cache_root);
        let (executor, rx) =
            build_executor(config, session.clone(), account, cache_root, diag.clone())?;
        // Unrecorded warm-up: opens the connection and fills the saved copies.
        run_once(
            "warm",
            0,
            terminal,
            &executor,
            &rx,
            &config.origin,
            opts,
            Instant::now(),
        )?;
        for i in 0..opts.runs {
            runs.push(run_once(
                "warm",
                i + 1,
                terminal,
                &executor,
                &rx,
                &config.origin,
                opts,
                Instant::now(),
            )?);
        }
        persist_rotated_cookies(
            &executor.client,
            &executor.session_handle(),
            &config.session_path(),
        );
    }
    reset_dir(cache_root);
    Ok(runs)
}

pub fn run(config: &Config, opts: BenchOptions) -> Result<(), String> {
    let load_started = Instant::now();
    let mut session = SessionFile::load(&config.session_path(), &config.origin)
        .map_err(|e| e.0)?
        .ok_or("No saved session; run tuicanvas once to sign in.")?;
    let session_load_ms = ms(load_started);
    let account = account_of(config, &mut session)?;
    let cache_root = config.data_dir.join("bench-cache");

    let runs = if opts.terminal {
        let mut terminal =
            ratatui::try_init().map_err(|_| "Could not initialise the terminal.".to_string())?;
        let result = run_all(
            &mut terminal,
            config,
            &opts,
            &session,
            &account,
            &cache_root,
        );
        ratatui::restore();
        result?
    } else {
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).map_err(|_| "backend")?;
        run_all(
            &mut terminal,
            config,
            &opts,
            &session,
            &account,
            &cache_root,
        )?
    };

    let summary = summarize(&runs);
    let usage = crate::procstats::usage();
    let report = json!({
        "version": 1,
        "observed_at": chrono::Utc::now().to_rfc3339(),
        "renderer": if opts.terminal { "terminal" } else { "in-memory buffer (110x34)" },
        "session_load_ms": (session_load_ms * 10.0).round() / 10.0,
        "process": usage,
        "summary": summary,
        "runs": runs,
    });
    let dir = config.private_reports();
    let _ = std::fs::create_dir_all(&dir);
    let name = if opts.terminal {
        "tui-bench-terminal.json"
    } else {
        "tui-bench.json"
    };
    std::fs::write(
        dir.join(name),
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    )
    .map_err(|_| "Could not write the benchmark report.".to_string())?;
    print_summary(&summary);
    if let Some(u) = usage {
        println!(
            "\nprocess: {:.0} ms CPU, {:.1} MB peak memory",
            u.cpu_ms, u.peak_memory_mb
        );
    }
    println!("session decrypt: {session_load_ms:.1} ms");
    println!("Report written to benchmarks/private/{name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn spread_reports_median_and_range() {
        let s = super::spread(&[5.0, 1.0, 3.0, 9.0]);
        assert_eq!((s.median, s.min, s.max), (4.0, 1.0, 9.0));
        let s = super::spread(&[2.0, 7.0, 4.0]);
        assert_eq!(s.median, 4.0);
    }
}
