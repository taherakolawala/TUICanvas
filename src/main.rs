//! TUICanvas: a keyboard-driven terminal interface for Canvas.

mod announcements;
mod api;
mod app;
mod bench;
mod cache;
mod calendar;
mod canvas;
mod config;
mod diag;
mod download;
mod html;
mod keys;
mod library;
mod model;
mod modules;
mod procstats;
mod protect;
mod session;
mod smoke;
mod submission;
mod theme;
mod transport;
mod tui;
mod ui;
mod upload_body;

const HELP: &str = "TUICanvas — terminal Canvas interface

Usage:
  tuicanvas                 Open the reader (signs in first if needed)
  tuicanvas --diag          Also log request timings to the data folder
  tuicanvas --login         Sign in again with the browser-assisted helper
  tuicanvas --logout        Delete the saved session and this account's saved copies
  tuicanvas --smoke [--course NAME] [--assignment NAME]
                            Read-only live check; writes benchmarks/private/smoke.json
  tuicanvas --dump DIR [--course NAME] [--assignment NAME]
                            Headless walk-through; writes rendered screens to DIR
  tuicanvas --bench [N] [--cold | --warm] [--terminal] [--course NAME] [--assignment NAME]
                            Time the workflow N times (default 10) for each condition;
                            writes benchmarks/private/tui-bench*.json

Keys: arrows/jk move, Enter open, Esc back, / filter, r refresh,
      s submit files from an assignment, o open in browser, L sign in, q quit
Env:  TUICANVAS_ORIGIN (default https://ufl.instructure.com), TUICANVAS_HOME";

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let started = std::time::Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    let config = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let course = value_after(&args, "--course").unwrap_or_else(|| "GPU".into());
    let assignment = value_after(&args, "--assignment").unwrap_or_else(|| "Lab-2".into());
    let code = if args.iter().any(|a| a == "--smoke") {
        match smoke::run(&config, smoke::SmokeOptions { course, assignment }) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if args.iter().any(|a| a == "--bench") {
        let runs = value_after(&args, "--bench")
            .and_then(|n| n.parse().ok())
            .unwrap_or(10);
        let cold_only = args.iter().any(|a| a == "--cold");
        let warm_only = args.iter().any(|a| a == "--warm");
        let options = bench::BenchOptions {
            runs,
            cold: !warm_only || cold_only,
            warm: !cold_only || warm_only,
            terminal: args.iter().any(|a| a == "--terminal"),
            course,
            assignment,
        };
        match bench::run(&config, options) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if let Some(dir) = value_after(&args, "--dump") {
        match tui::dump(&config, std::path::Path::new(&dir), &course, &assignment) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if args.iter().any(|a| a == "--login") {
        match tui::run_login_helper(&config) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if args.iter().any(|a| a == "--logout") {
        logout(&config)
    } else if let Some(unknown) = args.iter().find(|a| a.starts_with('-') && *a != "--diag") {
        eprintln!("Unknown option {unknown}\n\n{HELP}");
        2
    } else {
        let diag_file = args
            .iter()
            .any(|a| a == "--diag")
            .then(|| config.data_dir.join("diagnostics.jsonl"));
        match tui::run(&config, tui::Options { diag_file, started }) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    };
    std::process::exit(code);
}

fn logout(config: &config::Config) -> i32 {
    let path = config.session_path();
    let account = session::SessionFile::load(&path, &config.origin)
        .ok()
        .flatten()
        .and_then(|s| s.user_id);
    if let Some(id) = account {
        cache::SnapshotStore::new(&config.cache_root(), &id.to_string()).clear();
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {
            println!("Signed out locally: session and saved copies removed.");
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No saved session.");
            0
        }
        Err(_) => {
            eprintln!("Could not remove the saved session.");
            1
        }
    }
}
