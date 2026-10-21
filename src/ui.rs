//! Rendering. Only visible rows are built; description text is wrapped once
//! per assignment version and width, and styled only for the visible lines.

use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    App, DetailRender, FileRow, Local as LocalFile, Origin, Resource, SyncState, ViewKind,
    human_size,
};
use crate::html::{self, Link, Target};
use crate::model::{AssignmentDetail, AssignmentSummary, CanvasFile, Tone, fmt_num};
use crate::theme;

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn age(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (now - at).num_minutes();
    match mins {
        m if m < 1 => "just now".into(),
        m if m < 60 => format!("{m} min ago"),
        m if m < 48 * 60 => format!("{} h ago", m / 60),
        m => format!("{} days ago", m / (24 * 60)),
    }
}

fn relative(due: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (due - now).num_minutes();
    let span = mins.abs();
    let text = if span < 60 {
        format!("{span} min")
    } else if span < 48 * 60 {
        format!("{} h", span / 60)
    } else {
        format!("{} days", span / (24 * 60))
    };
    if mins >= 0 {
        format!("in {text}")
    } else {
        format!("{text} ago")
    }
}

pub fn short_date(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    if local.format("%Y").to_string() == now.with_timezone(&Local).format("%Y").to_string() {
        local.format("%a %b %-d %-I:%M %p").to_string()
    } else {
        local.format("%b %-d %Y %-I:%M %p").to_string()
    }
}

fn long_date(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%A, %B %-d, %Y at %-I:%M %p")
        .to_string()
}

fn submission_type(t: &str) -> &str {
    match t {
        "online_upload" => "File upload",
        "online_text_entry" => "Text entry",
        "online_url" => "Website URL",
        "media_recording" => "Media recording",
        "online_quiz" => "Quiz",
        "discussion_topic" => "Discussion",
        "external_tool" => "External tool",
        "on_paper" => "On paper",
        "student_annotation" => "Annotation",
        "none" => "No submission",
        other => other,
    }
}

/// Finished or not actionable: deadlines for these recede instead of alarming.
fn done(a: &AssignmentSummary, now: DateTime<Utc>) -> bool {
    matches!(a.status(now).1, Tone::Good | Tone::Neutral)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let now = Utc::now();
    let [top, main, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_top(frame, top, app, now);
    match app.view().kind.clone() {
        ViewKind::Courses => draw_courses(frame, main, app),
        ViewKind::Assignments { course_id, .. } => {
            let [tabs, body] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main);
            draw_tabs(frame, tabs, 0);
            draw_assignments(frame, body, app, &course_id, now)
        }
        ViewKind::Files { course_id, .. } => {
            let [tabs, body] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main);
            draw_tabs(frame, tabs, 1);
            draw_files(frame, body, app, &course_id, now)
        }
        ViewKind::Announcements { course_id, .. } => {
            let [tabs, body] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main);
            draw_tabs(frame, tabs, 2);
            draw_announcements(frame, body, app, &course_id, now);
        }
        ViewKind::Announcement {
            course_id,
            course_name,
            announcement_id,
            ..
        } => draw_announcement(frame, main, app, &course_id, &announcement_id, &course_name),
        ViewKind::Detail {
            course_id,
            course_name,
            assignment_id,
            ..
        } => draw_detail(
            frame,
            main,
            app,
            &course_id,
            &assignment_id,
            &course_name,
            now,
        ),
    }
    draw_footer(frame, bottom, app);
}

fn sync_text<T>(res: Option<&Resource<T>>, spinner: usize, now: DateTime<Utc>) -> (String, Style) {
    let Some(res) = res else {
        return (String::new(), Style::new());
    };
    let has_data = res.data.is_some();
    match &res.sync {
        SyncState::Syncing { .. } => {
            let saved = match (res.origin, res.fetched_at, has_data) {
                (Origin::Cache, Some(at), true) => format!(" · showing copy from {}", age(at, now)),
                _ => String::new(),
            };
            (
                format!("{} Syncing…{saved}", SPINNER[spinner % SPINNER.len()]),
                theme::syncing(),
            )
        }
        SyncState::Failed(e) => {
            let what = if has_data { "Outdated" } else { "Not loaded" };
            let hint = if e.needs_login() {
                "L sign in"
            } else {
                "r retry"
            };
            (
                format!("! {what}: {} · {hint}", e.describe()),
                theme::failed(),
            )
        }
        SyncState::Idle => match (res.origin, res.fetched_at) {
            (Origin::Network, Some(at)) => (
                format!("Updated {}", at.with_timezone(&Local).format("%-I:%M %p")),
                theme::updated(),
            ),
            (Origin::Cache, Some(at)) => (
                format!("Saved copy from {}", age(at, now)),
                theme::syncing(),
            ),
            _ => (String::new(), Style::new()),
        },
    }
}

fn draw_top(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>) {
    let mut crumbs = vec!["Canvas".to_string()];
    let (status, style) = match &app.view().kind {
        ViewKind::Courses => sync_text(Some(&app.courses), app.spinner, now),
        ViewKind::Assignments {
            course_id,
            course_name,
        } => {
            crumbs.push(course_name.clone());
            sync_text(app.assignments.get(course_id), app.spinner, now)
        }
        ViewKind::Announcements {
            course_id,
            course_name,
        } => {
            crumbs.push(course_name.clone());
            crumbs.push("Announcements".into());
            sync_text(app.announcements.get(course_id), app.spinner, now)
        }
        ViewKind::Announcement {
            course_id,
            course_name,
            announcement_id,
            title,
        } => {
            crumbs.push(course_name.clone());
            crumbs.push(title.clone());
            sync_text(
                app.announcement_details
                    .get(&(course_id.clone(), announcement_id.clone())),
                app.spinner,
                now,
            )
        }
        ViewKind::Detail {
            course_id,
            course_name,
            assignment_id,
            title,
        } => {
            crumbs.push(course_name.clone());
            crumbs.push(title.clone());
            sync_text(
                app.details.get(&(course_id.clone(), assignment_id.clone())),
                app.spinner,
                now,
            )
        }
        ViewKind::Files {
            course_id,
            course_name,
            ..
        } => {
            crumbs.push(course_name.clone());
            crumbs.push("Files".into());
            let path = app.folder_path();
            if !path.is_empty() {
                crumbs.extend(path.split(" / ").map(str::to_string));
            }
            sync_text(app.course_files.get(course_id), app.spinner, now)
        }
    };
    let width = usize::from(area.width);
    let status = truncate(&status, width.saturating_sub(12).max(10));
    let status_w = if status.is_empty() {
        0
    } else {
        status.width() + 2
    };
    // Breadcrumb: earlier levels dim, current level bold, orange separators.
    let budget = width.saturating_sub(status_w + 2);
    let mut spans = vec![Span::styled(" ", theme::bar())];
    let mut used = 1;
    let last = crumbs.len() - 1;
    for (i, crumb) in crumbs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" › ", theme::bar_separator()));
            used += 3;
        }
        let room = budget.saturating_sub(used);
        let text = truncate(crumb, room.max(1));
        used += text.width();
        let style = if i == last {
            theme::bar().add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            theme::bar_dim()
        };
        spans.push(Span::styled(text, style));
        if used >= budget {
            break;
        }
    }
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(used + status_w)),
        theme::bar(),
    ));
    if !status.is_empty() {
        spans.push(Span::styled(format!(" {status} "), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme::bar()), area);
}

fn hint_spans(hints: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(key.to_string(), theme::key()));
        spans.push(Span::styled(format!(" {label}"), theme::muted()));
    }
    spans
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let view = app.view();
    let left: Vec<Span> = if view.filtering {
        vec![
            Span::styled("/", theme::key()),
            Span::styled(view.filter.clone(), theme::bold()),
            Span::styled("▏", theme::key()),
            Span::raw("  "),
        ]
        .into_iter()
        .chain(hint_spans(&[("Enter", "keep"), ("Esc", "clear")]))
        .collect()
    } else if app.login_required {
        vec![Span::styled(
            " Signed out or session expired — press L to sign in ",
            theme::alert(),
        )]
    } else if let Some(notice) = &app.notice {
        vec![Span::styled(notice.clone(), theme::notice())]
    } else {
        let hints: &[(&str, &str)] = match view.kind {
            ViewKind::Courses => &[
                ("↑↓", "move"),
                ("Enter", "open"),
                ("e", "your files"),
                ("/", "filter"),
                ("r", "refresh"),
                ("o", "browser"),
                ("q", "quit"),
            ],
            ViewKind::Assignments { .. } | ViewKind::Announcements { .. } => &[
                ("↑↓", "move"),
                ("Enter", "open"),
                ("Tab", "switch"),
                ("Esc", "back"),
                ("/", "filter"),
                ("r", "refresh"),
                ("o", "browser"),
                ("q", "quit"),
            ],
            ViewKind::Files { .. } => &[
                ("Enter", "open"),
                ("d", "download"),
                ("D", "all"),
                ("e", "folder"),
                ("/", "search"),
                ("Tab", "switch"),
                ("Esc", "back"),
                ("o", "browser"),
                ("r", "refresh"),
            ],
            ViewKind::Announcement { .. } => &[
                ("↑↓", "scroll"),
                ("Tab", "links"),
                ("Enter", "open"),
                ("d/D", "download"),
                ("Esc", "back"),
                ("r", "refresh"),
                ("o", "browser"),
                ("q", "quit"),
            ],
            ViewKind::Detail {
                ref course_id,
                ref assignment_id,
                ..
            } if !app.links_of(course_id, assignment_id).is_empty() => {
                if view.link.is_some() {
                    &[
                        ("Tab", "next"),
                        ("Enter", "open"),
                        ("d", "download"),
                        ("D", "all files"),
                        ("e", "show folder"),
                        ("o", "browser"),
                        ("Esc", "done"),
                        ("↑↓", "scroll"),
                    ]
                } else {
                    &[
                        ("↑↓", "scroll"),
                        ("Tab", "files & links"),
                        ("D", "download files"),
                        ("Esc", "back"),
                        ("r", "refresh"),
                        ("o", "open in browser"),
                        ("q", "quit"),
                    ]
                }
            }
            ViewKind::Detail { .. } => &[
                ("↑↓", "scroll"),
                ("Esc", "back"),
                ("r", "refresh"),
                ("o", "open in browser"),
                ("q", "quit"),
            ],
        };
        let mut spans = Vec::new();
        if !view.filter.is_empty() {
            spans.push(Span::styled("filter ", theme::muted()));
            spans.push(Span::styled(view.filter.clone(), theme::code()));
            spans.push(Span::raw("  "));
        }
        spans.extend(hint_spans(hints));
        spans
    };
    let (queued, running) = app.transfer_counts();
    let (stats, stats_style) = if queued + running > 0 {
        let mut text = format!("downloading {}", running.max(1));
        if queued > 0 {
            text.push_str(&format!(" · {queued} queued"));
        }
        (text, theme::notice())
    } else {
        let text = app
            .last_stats
            .as_ref()
            .map(|(label, s)| {
                format!(
                    "{label} {} req · {:.1} KB · {:.0} ms",
                    s.requests,
                    s.bytes as f64 / 1024.0,
                    s.ms
                )
            })
            .unwrap_or_default();
        (text, theme::faint())
    };
    let width = usize::from(area.width);
    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let stats = if left_w + stats.width() + 2 <= width {
        stats
    } else {
        String::new()
    };
    let fill = width.saturating_sub(left_w + stats.width());
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(fill)));
    spans.push(Span::styled(stats, stats_style));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn placeholder<T>(frame: &mut Frame, area: Rect, res: Option<&Resource<T>>, what: &str) -> bool {
    if res.is_some_and(|r| r.data.is_some()) {
        return false;
    }
    let lines: Vec<Line> = match res.map(|r| &r.sync) {
        Some(SyncState::Failed(e)) => vec![
            Line::styled(
                format!("Could not load {what}: {}", e.describe()),
                theme::error(),
            ),
            Line::raw(""),
            Line::styled(
                if e.needs_login() {
                    "Press L to sign in, or q to quit."
                } else {
                    "Press r to retry."
                },
                theme::muted(),
            ),
        ],
        _ => vec![Line::styled(format!("Loading {what}…"), theme::muted())],
    };
    let y = area.y + area.height / 3;
    let target = Rect {
        y,
        height: area.height.saturating_sub(y - area.y),
        ..area
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), target);
    true
}

fn empty_message(frame: &mut Frame, area: Rect, filter: &str, what: &str) {
    let text = if filter.is_empty() {
        format!("No {what}.")
    } else {
        format!("No {what} match “{filter}”.")
    };
    frame.render_widget(
        Paragraph::new(Line::styled(text, theme::muted())).alignment(Alignment::Center),
        Rect {
            y: area.y + area.height / 3,
            height: 1,
            ..area
        },
    );
}

/// Keep the selection visible; returns the rows to render.
fn window(app: &mut App, rows: usize, height: usize) -> (usize, usize) {
    let view = app.stack.last_mut().expect("view");
    let height = height.max(1);
    if view.selected < view.offset {
        view.offset = view.selected;
    } else if view.selected >= view.offset + height {
        view.offset = view.selected + 1 - height;
    }
    view.offset = view.offset.min(rows.saturating_sub(height));
    (view.offset, (view.offset + height).min(rows))
}

fn draw_courses(frame: &mut Frame, area: Rect, app: &mut App) {
    if placeholder(frame, area, Some(&app.courses), "courses") {
        return;
    }
    let rows = app.visible_rows();
    if rows.is_empty() {
        empty_message(frame, area, &app.view().filter, "courses");
        return;
    }
    let (start, end) = window(app, rows.len(), usize::from(area.height.saturating_sub(1)));
    let selected = app.view().selected;
    let courses = app.courses.data.as_deref().unwrap_or(&[]);
    let built: Vec<Row> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(i, &index)| {
            let c = &courses[index];
            let row = Row::new(vec![
                // ASCII: "★" is ambiguous-width and misaligns columns in some fonts.
                Cell::from(if c.is_favorite == Some(true) {
                    "*"
                } else {
                    " "
                })
                .style(theme::favorite()),
                Cell::from(c.title().to_string()),
                Cell::from(c.course_code.clone().unwrap_or_default()).style(theme::code()),
                Cell::from(
                    c.term
                        .as_ref()
                        .and_then(|t| t.name.clone())
                        .unwrap_or_default(),
                )
                .style(theme::faint()),
            ]);
            if start + i == selected {
                row.style(theme::selected())
            } else {
                row
            }
        })
        .collect();
    let wide = area.width >= 90;
    let widths = if wide {
        vec![
            Constraint::Length(1),
            Constraint::Fill(3),
            Constraint::Fill(1),
            Constraint::Length(18),
        ]
    } else {
        vec![
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(0),
            Constraint::Length(0),
        ]
    };
    let header = Row::new(vec![
        "",
        "Course",
        if wide { "Code" } else { "" },
        if wide { "Term" } else { "" },
    ])
    .style(theme::table_header());
    frame.render_widget(
        Table::new(built, widths).header(header).column_spacing(1),
        area,
    );
}

fn draw_assignments(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    course_id: &str,
    now: DateTime<Utc>,
) {
    if placeholder(frame, area, app.assignments.get(course_id), "assignments") {
        return;
    }
    let rows = app.visible_rows();
    if rows.is_empty() {
        empty_message(frame, area, &app.view().filter, "assignments");
        return;
    }
    let (start, end) = window(app, rows.len(), usize::from(area.height.saturating_sub(1)));
    let selected = app.view().selected;
    let list = app
        .assignments
        .get(course_id)
        .and_then(|r| r.data.as_deref())
        .unwrap_or(&[]);
    let built: Vec<Row> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(i, &index)| {
            let a = &list[index];
            let (status, tone) = a.status(now);
            let due = a
                .due_at
                .map(|d| short_date(d, now))
                .unwrap_or_else(|| "No due date".into());
            let row = Row::new(vec![
                Cell::from(due).style(theme::due(a.due_at, done(a, now), now)),
                Cell::from(a.title().to_string()),
                Cell::from(status).style(theme::tone(tone)),
                Cell::from(
                    a.points_possible
                        .map(|p| format!("{} pts", fmt_num(p)))
                        .unwrap_or_default(),
                )
                .style(theme::muted()),
            ]);
            if start + i == selected {
                row.style(theme::selected())
            } else {
                row
            }
        })
        .collect();
    let widths = if area.width >= 100 {
        vec![
            Constraint::Length(20),
            Constraint::Fill(1),
            Constraint::Length(24),
            Constraint::Length(8),
        ]
    } else if area.width >= 70 {
        vec![
            Constraint::Length(20),
            Constraint::Fill(1),
            Constraint::Length(16),
            Constraint::Length(0),
        ]
    } else {
        vec![
            Constraint::Length(0),
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(0),
        ]
    };
    let header =
        Row::new(vec!["Due", "Assignment", "Status", "Points"]).style(theme::table_header());
    frame.render_widget(
        Table::new(built, widths).header(header).column_spacing(1),
        area,
    );
}

fn draw_announcements(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    course_id: &str,
    now: DateTime<Utc>,
) {
    if placeholder(
        frame,
        area,
        app.announcements.get(course_id),
        "announcements",
    ) {
        return;
    }
    let rows = app.visible_rows();
    if rows.is_empty() {
        empty_message(frame, area, &app.view().filter, "announcements");
        return;
    }
    let (start, end) = window(app, rows.len(), usize::from(area.height.saturating_sub(1)));
    let list = app
        .announcements
        .get(course_id)
        .and_then(|r| r.data.as_deref())
        .unwrap_or(&[]);
    let built: Vec<Row> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(i, &index)| {
            let a = &list[index];
            let row = Row::new(vec![
                Cell::from(
                    a.date()
                        .map(|d| short_date(d, now))
                        .unwrap_or_else(|| "Unknown date".into()),
                )
                .style(theme::muted()),
                Cell::from(html::sanitize(a.title())),
                Cell::from(html::sanitize(a.author())).style(theme::muted()),
                Cell::from(app.announcement_status(course_id, a)).style(
                    if app.announcement_status(course_id, a) == "Unread" {
                        theme::tab_active()
                    } else {
                        theme::muted()
                    },
                ),
            ]);
            if start + i == app.view().selected {
                row.style(theme::selected())
            } else {
                row
            }
        })
        .collect();
    let wide = area.width >= 80;
    let widths = [
        Constraint::Length(if area.width >= 60 { 20 } else { 0 }),
        Constraint::Fill(1),
        Constraint::Length(if wide { 20 } else { 0 }),
        Constraint::Length(7),
    ];
    frame.render_widget(
        Table::new(built, widths)
            .header(
                Row::new(["Posted", "Announcement", "Author", "Status"])
                    .style(theme::table_header()),
            )
            .column_spacing(1),
        area,
    );
}

/// `Assignments  Files` for a course, the current one highlighted.
fn draw_tabs(frame: &mut Frame, area: Rect, active: usize) {
    let tab = |label: &str, on: bool| {
        Span::styled(
            label.to_string(),
            if on {
                theme::tab_active()
            } else {
                theme::muted()
            },
        )
    };
    let spans = vec![
        Span::raw(" "),
        tab("Assignments", active == 0),
        Span::styled(" │ ", theme::faint()),
        tab("Files", active == 1),
        Span::styled(" │ ", theme::faint()),
        tab("Announcements", active == 2),
        Span::styled("   Tab", theme::key()),
        Span::styled(" switch", theme::faint()),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn parse_time(text: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// What the "On this PC" column says about a file.
fn local_text(state: &LocalFile) -> (String, Style) {
    match state {
        LocalFile::Missing => (String::new(), theme::faint()),
        LocalFile::Saved => ("saved".into(), theme::tone(Tone::Good)),
        LocalFile::Changed => ("newer on Canvas".into(), theme::tone(Tone::Warn)),
        LocalFile::Queued => ("queued".into(), theme::muted()),
        LocalFile::Running { received, total } => (
            match total {
                Some(t) if *t > 0 => format!("{}%", (received * 100 / t).min(100)),
                _ => human_size(*received),
            },
            theme::key(),
        ),
        LocalFile::Failed(_) => ("failed".into(), theme::error()),
        LocalFile::Locked => ("locked".into(), theme::faint()),
    }
}

fn draw_files(frame: &mut Frame, area: Rect, app: &mut App, course_id: &str, now: DateTime<Utc>) {
    if placeholder(frame, area, app.course_files.get(course_id), "files") {
        return;
    }
    let hidden = app
        .course_files
        .get(course_id)
        .and_then(|r| r.data.as_ref())
        .is_some_and(|d| d.files.is_none());
    if hidden {
        let lines = vec![
            Line::styled(
                "The instructor has hidden Files for this course.",
                theme::muted(),
            ),
            Line::raw(""),
            Line::styled(
                "Files linked from assignments still open from the assignment.",
                theme::faint(),
            ),
        ];
        frame.render_widget(
            Paragraph::new(lines).alignment(Alignment::Center),
            Rect {
                y: area.y + area.height / 3,
                height: area.height.saturating_sub(area.height / 3),
                ..area
            },
        );
        return;
    }
    let count = app.file_rows().len();
    if count == 0 {
        let filter = app.view().filter.clone();
        empty_message(
            frame,
            area,
            &filter,
            if filter.is_empty() {
                "files in this folder"
            } else {
                "files"
            },
        );
        return;
    }
    let (start, end) = window(app, count, usize::from(area.height.saturating_sub(1)));
    let selected = app.view().selected;
    let searching = !app.view().filter.is_empty();
    let rows = app.file_rows();
    let built: Vec<Row> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let cells = match row {
                FileRow::Folder(f) => {
                    let locked = f.locked_for_user == Some(true);
                    vec![
                        Cell::from("DIR").style(theme::accent()),
                        Cell::from(format!("{}/", html::sanitize(row.name()))).style(theme::bold()),
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(if locked { "locked" } else { "" }).style(theme::faint()),
                    ]
                }
                FileRow::File(f) => {
                    let (state, style) = local_text(&app.local_state(f));
                    let mut name = Vec::new();
                    if searching
                        && let Some(path) = f
                            .folder_id
                            .as_deref()
                            .and_then(|id| app.folder(course_id, id))
                            .and_then(|d| d.full_name.as_deref())
                            .map(|full| full.split('/').skip(1).collect::<Vec<_>>().join(" / "))
                            .filter(|p| !p.is_empty())
                    {
                        name.push(Span::styled(
                            format!("{} / ", html::sanitize(&path)),
                            theme::faint(),
                        ));
                    }
                    name.push(Span::raw(html::sanitize(f.name())));
                    vec![
                        Cell::from(f.kind()).style(theme::code()),
                        Cell::from(Line::from(name)),
                        Cell::from(f.size.map(human_size).unwrap_or_default())
                            .style(theme::muted()),
                        Cell::from(
                            parse_time(f.modified_at.as_deref().or(f.updated_at.as_deref()))
                                .map(|t| short_date(t, now))
                                .unwrap_or_default(),
                        )
                        .style(theme::faint()),
                        Cell::from(state).style(style),
                    ]
                }
            };
            let row = Row::new(cells);
            if start + i == selected {
                row.style(theme::selected())
            } else {
                row
            }
        })
        .collect();
    let widths = if area.width >= 100 {
        vec![
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Length(20),
            Constraint::Length(15),
        ]
    } else if area.width >= 70 {
        vec![
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Length(0),
            Constraint::Length(15),
        ]
    } else {
        vec![
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(0),
            Constraint::Length(0),
            Constraint::Length(9),
        ]
    };
    let header = Row::new(vec![
        "",
        if searching {
            "Name (whole course)"
        } else {
            "Name"
        },
        "Size",
        "Updated",
        "On this PC",
    ])
    .style(theme::table_header());
    frame.render_widget(
        Table::new(built, widths).header(header).column_spacing(1),
        area,
    );
}

/// One row of an assignment's files & links list: kind, name, size, state.
fn link_row(app: &App, link: &Link) -> (String, String, String, (String, Style)) {
    match &link.target {
        Target::File { id } => {
            let meta: Option<&CanvasFile> = app.file_meta(id);
            let kind = meta
                .map(|f| f.kind())
                .unwrap_or(if link.image { "IMG" } else { "FILE" });
            let name = meta
                .map(|f| f.name().to_string())
                .filter(|_| !link.image || link.text.is_empty())
                .unwrap_or_else(|| {
                    if link.text.is_empty() {
                        "file".into()
                    } else {
                        link.text.clone()
                    }
                });
            let size = meta
                .and_then(|f| f.size)
                .map(human_size)
                .unwrap_or_default();
            let state = match (meta, app.files.get(id).map(|r| &r.sync)) {
                (Some(f), _) => local_text(&app.local_state(f)),
                (None, Some(SyncState::Failed(_))) => ("no access".into(), theme::error()),
                (None, None) if matches!(app.view().kind, ViewKind::Announcement { .. }) => {
                    ("on demand".into(), theme::faint())
                }
                (None, _) => ("…".into(), theme::faint()),
            };
            (kind.into(), name, size, state)
        }
        Target::Assignment { .. } => (
            "ASGN".into(),
            link.text.clone(),
            String::new(),
            ("opens here".into(), theme::faint()),
        ),
        Target::Canvas => (
            "PAGE".into(),
            link.text.clone(),
            String::new(),
            ("Canvas page".into(), theme::faint()),
        ),
        Target::Web => {
            let host = link
                .url
                .split("://")
                .nth(1)
                .and_then(|r| r.split(['/', '?', '#']).next())
                .unwrap_or("")
                .trim_start_matches("www.")
                .to_string();
            let name = if link.text.is_empty() {
                link.url.clone()
            } else {
                link.text.clone()
            };
            ("WEB".into(), name, String::new(), (host, theme::faint()))
        }
        Target::Mail => {
            let to = link
                .url
                .trim_start_matches("mailto:")
                .split('?')
                .next()
                .unwrap_or("")
                .to_string();
            (
                "MAIL".into(),
                if link.text.is_empty() {
                    to.clone()
                } else {
                    link.text.clone()
                },
                String::new(),
                (to, theme::faint()),
            )
        }
    }
}

fn draw_links(frame: &mut Frame, area: Rect, app: &App, links: &[Link], selected: Option<usize>) {
    let [title, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let mut head = vec![Span::styled(
        format!(" Files & links ({})", links.len()),
        theme::heading(),
    )];
    if selected.is_none() {
        head.push(Span::styled("  Tab", theme::key()));
        head.push(Span::styled(" to select", theme::faint()));
    }
    let rule = usize::from(area.width)
        .saturating_sub(head.iter().map(|s| s.content.width()).sum::<usize>() + 1);
    head.push(Span::styled(
        format!(" {}", "─".repeat(rule)),
        theme::faint(),
    ));
    frame.render_widget(Paragraph::new(Line::from(head)), title);
    let height = usize::from(list.height).max(1);
    let offset = selected
        .map(|s| s.saturating_sub(height - 1))
        .unwrap_or(0)
        .min(links.len().saturating_sub(height));
    let rows: Vec<Row> = links
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, link)| {
            let (kind, name, size, (state, style)) = link_row(app, link);
            let row = Row::new(vec![
                Cell::from(format!("{:>2}", i + 1)).style(theme::key()),
                Cell::from(kind).style(theme::code()),
                Cell::from(html::sanitize(&name)),
                Cell::from(size).style(theme::muted()),
                Cell::from(html::sanitize(&state)).style(style),
            ]);
            if Some(i) == selected {
                row.style(theme::selected())
            } else {
                row
            }
        })
        .collect();
    let widths = if area.width >= 70 {
        vec![
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Length(18),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(0),
            Constraint::Length(10),
        ]
    };
    frame.render_widget(Table::new(rows, widths).column_spacing(1), list);
}

fn detail_header(d: &AssignmentDetail, course: &str, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let a = &d.base;
    let label = |s: &str| Span::styled(format!("{s:<12}"), theme::muted());
    let mut lines = vec![
        Line::styled(html::sanitize(a.title()), theme::heading()),
        Line::from(vec![
            label("Course"),
            Span::styled(html::sanitize(course), theme::code()),
        ]),
    ];
    let due = match a.due_at {
        Some(due) => Line::from(vec![
            label("Due"),
            Span::styled(long_date(due), theme::due(Some(due), done(a, now), now)),
            Span::styled(format!("  ({})", relative(due, now)), theme::muted()),
        ]),
        None => Line::from(vec![
            label("Due"),
            Span::styled("No due date", theme::faint()),
        ]),
    };
    lines.push(due);
    if a.unlock_at.is_some() || a.lock_at.is_some() {
        let mut parts = vec![];
        if let Some(u) = a.unlock_at {
            parts.push(format!("from {}", short_date(u, now)));
        }
        if let Some(l) = a.lock_at {
            parts.push(format!("until {}", short_date(l, now)));
        }
        lines.push(Line::from(vec![
            label("Available"),
            Span::raw(parts.join(" ")),
        ]));
    }
    let (status, tone) = a.status(now);
    let mut status_line = vec![label("Status"), Span::styled(status, theme::tone(tone))];
    if let Some(sub) = &a.submission {
        if let Some(at) = sub.submitted_at {
            status_line.push(Span::styled(
                format!("  · submitted {}", short_date(at, now)),
                theme::muted(),
            ));
        }
        if sub.attempt.unwrap_or(0) > 1 {
            status_line.push(Span::styled(
                format!(" · attempt {}", sub.attempt.unwrap_or(0)),
                theme::muted(),
            ));
        }
    }
    lines.push(Line::from(status_line));
    if let Some(p) = a.points_possible {
        lines.push(Line::from(vec![
            label("Points"),
            Span::styled(fmt_num(p), theme::bold()),
        ]));
    }
    if !a.submission_types.is_empty() {
        let types: Vec<&str> = a
            .submission_types
            .iter()
            .map(|t| submission_type(t))
            .collect();
        let attempts = match d.allowed_attempts {
            Some(n) if n > 0 => format!(" · {n} attempt{}", if n == 1 { "" } else { "s" }),
            _ => String::new(),
        };
        lines.push(Line::from(vec![
            label("Submit via"),
            Span::raw(types.join(", ")),
            Span::styled(attempts, theme::muted()),
        ]));
    }
    if a.locked_for_user == Some(true) {
        let why = d
            .lock_explanation
            .as_deref()
            .map(|e| html::render(e, 200, "").join(" "))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            label("Locked"),
            Span::styled(why, theme::error()),
        ]));
    }
    lines
}

/// Inline markup produced by the HTML renderer: `**bold**` and `[text][n]` links.
fn style_inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
            && end > 0
        {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut plain), base));
            }
            spans.push(Span::styled(
                after[..end].to_string(),
                base.add_modifier(ratatui::style::Modifier::BOLD),
            ));
            rest = &after[end + 2..];
            continue;
        }
        if let Some(after) = rest.strip_prefix('[')
            && let Some(close) = after.find("][")
            && !after[..close].contains(['[', ']'])
            && let Some(num_end) = after[close + 2..].find(']')
            && after[close + 2..close + 2 + num_end]
                .chars()
                .all(|c| c.is_ascii_digit())
            && num_end > 0
        {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut plain), base));
            }
            spans.push(Span::styled(
                after[..close].replace("**", ""),
                theme::link(),
            ));
            spans.push(Span::styled(
                after[close + 1..close + 3 + num_end].to_string(),
                theme::faint(),
            ));
            rest = &after[close + 3 + num_end..];
            continue;
        }
        let c = rest.chars().next().expect("non-empty");
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

/// Colour one rendered description line: headings, list markers, link
/// footnotes, table rules, inline bold and links.
pub fn style_line(line: &str) -> Line<'static> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    if let Some(level) = trimmed
        .find(|c| c != '#')
        .filter(|&n| n > 0 && trimmed[n..].starts_with(' '))
    {
        return Line::from(vec![
            Span::raw(indent.to_string()),
            Span::styled(trimmed[level + 1..].to_string(), theme::heading()),
        ]);
    }
    if trimmed.starts_with('[')
        && let Some(close) = trimmed.find("]: ")
        && trimmed[1..close].chars().all(|c| c.is_ascii_digit())
    {
        return Line::from(vec![
            Span::raw(indent.to_string()),
            Span::styled(trimmed[..close + 3].to_string(), theme::faint()),
            Span::styled(trimmed[close + 3..].to_string(), theme::link()),
        ]);
    }
    if !trimmed.is_empty() && trimmed.chars().all(|c| "─│┬┴┼├┤┌┐└┘ ".contains(c))
    {
        return Line::styled(line.to_string(), theme::faint());
    }
    let marker_len = if trimmed.starts_with("* ") || trimmed.starts_with("- ") {
        2
    } else {
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && trimmed[digits..].starts_with(". ") {
            digits + 2
        } else {
            0
        }
    };
    let mut spans = vec![Span::raw(indent.to_string())];
    if marker_len > 0 {
        spans.push(Span::styled(
            trimmed[..marker_len].to_string(),
            theme::accent(),
        ));
    }
    spans.extend(style_inline(&trimmed[marker_len..], theme::text()));
    Line::from(spans)
}

fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    course_id: &str,
    assignment_id: &str,
    course: &str,
    now: DateTime<Utc>,
) {
    let key = (course_id.to_string(), assignment_id.to_string());
    if placeholder(frame, area, app.details.get(&key), "assignment") {
        return;
    }
    // Files and links get a fixed list at the bottom, so they stay in view while reading.
    let links = app.links_of(course_id, assignment_id);
    let area = if links.is_empty() {
        area
    } else {
        let cap = if area.height >= 24 {
            6
        } else if area.height >= 16 {
            4
        } else {
            2
        };
        let rows = links.len().min(cap) as u16;
        let [text, panel] =
            Layout::vertical([Constraint::Min(4), Constraint::Length(rows + 1)]).areas(area);
        let selected = app.view().link.filter(|&i| i < links.len());
        draw_links(frame, panel, app, &links, selected);
        text
    };
    let res = &app.details[&key];
    let detail = res.data.as_ref().expect("checked");
    let text_width = area.width.saturating_sub(2).min(110);
    let stale = !matches!(&app.detail_render, Some(r) if r.key == key && r.version == res.version && r.width == text_width);
    if stale {
        let lines = html::render(
            detail.description.as_deref().unwrap_or(""),
            usize::from(text_width),
            &app.origin,
        );
        app.detail_render = Some(DetailRender {
            key: key.clone(),
            version: res.version,
            width: text_width,
            lines,
        });
    }
    let mut lines = detail_header(detail, course, now);
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "─".repeat(usize::from(text_width)),
        theme::faint(),
    ));
    let head_len = lines.len();
    let body = &app.detail_render.as_ref().expect("rendered").lines;
    let body_len = body.len().max(1);
    let height = usize::from(area.height);
    let max_scroll = (head_len + body_len).saturating_sub(height);
    let view = app.stack.last_mut().expect("view");
    view.scroll = view.scroll.min(max_scroll);
    let scroll = view.scroll;
    let mut visible: Vec<Line> = lines.into_iter().skip(scroll).take(height).collect();
    let body_start = scroll.saturating_sub(head_len);
    let room = height - visible.len();
    if body.is_empty() {
        if room > 0 && scroll <= head_len {
            visible.push(Line::styled("(No description)", theme::muted()));
        }
    } else {
        visible.extend(
            body.iter()
                .skip(body_start)
                .take(room)
                .map(|l| style_line(l)),
        );
    }
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };
    frame.render_widget(Paragraph::new(visible), inner);
}

fn draw_announcement(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    course_id: &str,
    announcement_id: &str,
    course: &str,
) {
    let key = (course_id.to_string(), announcement_id.to_string());
    if placeholder(
        frame,
        area,
        app.announcement_details.get(&key),
        "announcement",
    ) {
        return;
    }
    // Files and links get a fixed list at the bottom, so they stay in view while reading.
    let links = app.announcement_links(course_id, announcement_id);
    let area = if links.is_empty() {
        area
    } else {
        let cap = if area.height >= 24 {
            6
        } else if area.height >= 16 {
            4
        } else {
            2
        };
        let rows = links.len().min(cap) as u16;
        let [text, panel] =
            Layout::vertical([Constraint::Min(4), Constraint::Length(rows + 1)]).areas(area);
        let selected = app.view().link.filter(|&i| i < links.len());
        draw_links(frame, panel, app, &links, selected);
        text
    };
    let res = &app.announcement_details[&key];
    let detail = res.data.as_ref().expect("checked");
    let text_width = area.width.saturating_sub(2).min(110);
    let stale = !matches!(&app.announcement_render, Some(r) if r.key == key && r.version == res.version && r.width == text_width);
    if stale {
        let lines = html::render(
            // Canvas already returns a visibility explanation instead of the
            // message when access is restricted. A comment lock is not a read lock.
            detail.message.as_deref().unwrap_or(""),
            usize::from(text_width),
            &app.origin,
        );
        app.announcement_render = Some(DetailRender {
            key: key.clone(),
            version: res.version,
            width: text_width,
            lines,
        });
    }
    let a = &detail.base;
    let mut lines = vec![
        Line::styled(html::sanitize(a.title()), theme::tab_active()),
        Line::styled(html::sanitize(course), theme::muted()),
        Line::raw(format!(
            "{} · {} · {}",
            html::sanitize(a.author()),
            a.date()
                .map(long_date)
                .unwrap_or_else(|| "Date unavailable".into()),
            app.announcement_status(course_id, a)
        )),
    ];
    if let Some(n) = a.discussion_subentry_count.filter(|n| *n > 0) {
        lines.push(Line::styled(
            format!("{n} replies · open in browser to read replies"),
            theme::muted(),
        ));
    }
    if detail.locked_for_user == Some(true) {
        lines.push(Line::styled(
            html::sanitize(
                detail
                    .lock_explanation
                    .as_deref()
                    .unwrap_or("Comments are unavailable."),
            ),
            if detail.can_view() {
                theme::muted()
            } else {
                theme::failed()
            },
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "─".repeat(usize::from(text_width)),
        theme::faint(),
    ));
    let head_len = lines.len();
    let body = &app.announcement_render.as_ref().expect("rendered").lines;
    let body_len = body.len().max(1);
    let height = usize::from(area.height);
    let max_scroll = (head_len + body_len).saturating_sub(height);
    let view = app.stack.last_mut().expect("view");
    view.scroll = view.scroll.min(max_scroll);
    let scroll = view.scroll;
    let mut visible: Vec<Line> = lines.into_iter().skip(scroll).take(height).collect();
    let body_start = scroll.saturating_sub(head_len);
    let room = height - visible.len();
    if body.is_empty() {
        if room > 0 && scroll <= head_len {
            visible.push(Line::styled("(No message)", theme::muted()));
        }
    } else {
        visible.extend(
            body.iter()
                .skip(body_start)
                .take(room)
                .map(|l| style_line(l)),
        );
    }
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };
    frame.render_widget(Paragraph::new(visible), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Msg, Payload};
    use crate::cache::ResourceKey;
    use crate::canvas::FetchStats;
    use crate::model::{AssignmentSummary, Course, Submission};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let b = t.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn deliver(app: &mut App, effects: Vec<crate::app::Effect>, payload: Payload) {
        for e in effects {
            if let crate::app::Effect::Fetch { key, request } = e {
                app.update(Msg::Fetched {
                    key,
                    request,
                    result: Ok((
                        payload,
                        FetchStats {
                            requests: 1,
                            bytes: 14_200,
                            ms: 212.0,
                            ..Default::default()
                        },
                    )),
                    at: Utc::now(),
                });
                return;
            }
        }
        panic!("no fetch");
    }

    fn texts(line: &Line) -> Vec<String> {
        line.spans
            .iter()
            .filter(|s| !s.content.is_empty())
            .map(|s| s.content.to_string())
            .collect()
    }

    #[test]
    fn announcements_render_list_detail_links_and_errors_at_small_sizes() {
        let mut app = App::new("https://ufl.instructure.com", None);
        let send =
            |app: &mut App, code| app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        let effects = app.start();
        deliver(
            &mut app,
            effects,
            Payload::Courses(vec![Course {
                id: "1".into(),
                name: Some("Course".into()),
                ..Default::default()
            }]),
        );
        let effects = send(&mut app, KeyCode::Enter);
        deliver(&mut app, effects, Payload::Assignments(vec![]));
        let effects = send(&mut app, KeyCode::BackTab);
        let base = crate::announcements::Summary {
            id: "7".into(),
            title: Some("Exam moved".into()),
            read_state: Some("unread".into()),
            author: Some(crate::announcements::Author {
                display_name: Some("Professor".into()),
            }),
            ..Default::default()
        };
        deliver(
            &mut app,
            effects,
            Payload::Announcements(vec![base.clone()]),
        );
        let rendered = screen(&mut app, 100, 18);
        println!("{rendered}\n");
        assert!(
            rendered.contains("Announcements")
                && rendered.contains("Exam moved")
                && rendered.contains("Unread")
                && rendered.contains("Professor")
        );
        let effects = send(&mut app, KeyCode::Enter);
        let detail = crate::announcements::Detail {base, message:Some("<h2>New room</h2><p>Meet in room 200.</p><a href='https://example.org'>Schedule</a>".into()), locked_for_user:Some(true), lock_explanation:Some("This topic is closed for comments.".into()), ..Default::default()};
        deliver(&mut app, effects, Payload::Announcement(Box::new(detail)));
        let rendered = screen(&mut app, 100, 18);
        println!("{rendered}\n");
        assert!(rendered.contains("Meet in room 200.") && rendered.contains("Schedule"));
        assert!(rendered.contains("Read") && !rendered.contains("Unread"));
        let effects = send(&mut app, KeyCode::Char('r'));
        assert!(screen(&mut app, 100, 18).contains("Syncing"));
        let crate::app::Effect::Fetch { key, request } = effects[0].clone() else {
            panic!("expected fetch")
        };
        app.update(Msg::Fetched {
            key,
            request,
            result: Err(crate::canvas::CanvasError::Unauthenticated),
            at: Utc::now(),
        });
        let rendered = screen(&mut app, 100, 18);
        println!("{rendered}\n");
        assert!(rendered.contains("Outdated") && rendered.contains("Meet in room 200."));
        for (w, h) in [(40, 10), (20, 5), (1, 1)] {
            let _ = screen(&mut app, w, h);
        }
    }

    /// Replays private saved responses without printing announcement contents.
    #[test]
    #[ignore = "requires private saved announcement snapshots"]
    fn saved_announcement_bodies_render() {
        let config = crate::config::Config::load().unwrap();
        let mut checked = 0;
        for account in std::fs::read_dir(config.cache_root()).unwrap().flatten() {
            if !account.path().is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(account.path()).unwrap().flatten() {
                if !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("announcement-")
                {
                    continue;
                }
                let envelope: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
                let detail: crate::announcements::Detail =
                    serde_json::from_value(envelope["data"].clone()).unwrap();
                let expected =
                    html::render(detail.message.as_deref().unwrap_or(""), 98, &config.origin);
                if expected.is_empty() {
                    continue;
                }
                let id = detail.base.id.clone();
                let mut app = App::new(&config.origin, None);
                app.stack.push(crate::app::View {
                    kind: ViewKind::Announcement {
                        course_id: "1".into(),
                        course_name: "Course".into(),
                        announcement_id: id.clone(),
                        title: "Announcement".into(),
                    },
                    selected: 0,
                    offset: 0,
                    filter: String::new(),
                    filtering: false,
                    scroll: 0,
                    link: None,
                });
                app.announcement_details.insert(
                    ("1".into(), id),
                    Resource {
                        data: Some(detail),
                        ..Default::default()
                    },
                );
                let rendered = screen(&mut app, 100, 40);
                assert!(!rendered.contains("(No message)"), "saved message hidden");
                assert!(
                    app.announcement_render.as_ref().unwrap().lines == expected,
                    "saved body not rendered intact"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no saved messages to replay");
        println!(
            "Verified {checked} saved announcement bodies render intact (content not logged)."
        );
    }

    #[test]
    fn description_markup_is_styled_not_shown() {
        let h = style_line("## Overview");
        assert_eq!(texts(&h), ["Overview"]);
        let l = style_line("Read the [Lab-2 slides][1] and **submit** [note] here.");
        assert_eq!(
            texts(&l),
            [
                "Read the ",
                "Lab-2 slides",
                "[1]",
                " and ",
                "submit",
                " [note] here."
            ]
        );
        assert_eq!(
            l.spans
                .iter()
                .find(|s| s.content == "Lab-2 slides")
                .unwrap()
                .style,
            theme::link()
        );
        let f = style_line("[1]: https://ufl.instructure.com/courses/1/files/2");
        assert_eq!(
            texts(&f),
            ["[1]: ", "https://ufl.instructure.com/courses/1/files/2"]
        );
        let b = style_line("  12. Step");
        assert_eq!(texts(&b), ["  ", "12. ", "Step"]);
        assert_eq!(texts(&style_line("5 * 3 = 15")), ["5 * 3 = 15"]);
        assert_eq!(texts(&style_line("a [x] b [y][z]")), ["a [x] b [y][z]"]);
    }

    #[test]
    fn deadlines_colour_by_urgency() {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            // Colours are deliberately off; urgency is still bold.
            assert_eq!(theme::due(Some(Utc::now()), false, Utc::now()).fg, None);
            return;
        }
        let now = Utc::now();
        let hours = |h: i64| Some(now + chrono::Duration::hours(h));
        assert_eq!(theme::due(hours(-2), false, now).fg, Some(theme::RED));
        assert_eq!(theme::due(hours(5), false, now).fg, Some(theme::PEACH));
        assert_eq!(theme::due(hours(48), false, now).fg, Some(theme::AMBER));
        assert_eq!(theme::due(hours(24 * 10), false, now).fg, None);
        assert_eq!(theme::due(hours(-2), true, now).fg, Some(theme::MUTED));
    }

    #[test]
    fn renders_each_screen() {
        let now = Utc::now();
        let mut app = App::new("https://ufl.instructure.com", None);
        let effects = app.start();
        let loading = screen(&mut app, 100, 12);
        assert!(loading.contains("Loading courses"));
        assert!(loading.contains("Syncing"));
        let courses = vec![
            Course {
                id: "1".into(),
                name: Some("GPU Computing".into()),
                course_code: Some("EEL5934".into()),
                is_favorite: Some(true),
                ..Default::default()
            },
            Course {
                id: "2".into(),
                name: Some("Compilers".into()),
                course_code: Some("COP5621".into()),
                ..Default::default()
            },
        ];
        deliver(&mut app, effects, Payload::Courses(courses));
        let s = screen(&mut app, 100, 12);
        println!("{s}\n");
        assert!(s.contains("GPU Computing") && s.contains("Updated"));
        let effects = app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        let list = vec![
            AssignmentSummary {
                id: "7".into(),
                name: Some("Lab-2 Assignment".into()),
                due_at: Some(now + chrono::Duration::days(3)),
                points_possible: Some(10.0),
                submission_types: vec!["online_upload".into()],
                submission: Some(Submission::default()),
                ..Default::default()
            },
            AssignmentSummary {
                id: "8".into(),
                name: Some("Lab-1 Assignment".into()),
                due_at: Some(now - chrono::Duration::days(10)),
                points_possible: Some(10.0),
                submission_types: vec!["online_upload".into()],
                submission: Some(Submission {
                    submitted_at: Some(now),
                    workflow_state: Some("graded".into()),
                    score: Some(9.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];
        deliver(&mut app, effects, Payload::Assignments(list));
        let s = screen(&mut app, 100, 12);
        println!("{s}\n");
        assert!(
            s.contains("Lab-2 Assignment")
                && s.contains("Not submitted")
                && s.contains("Graded 9/10")
        );
        let effects = app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        let detail = crate::model::AssignmentDetail {
            base: AssignmentSummary {
                id: "7".into(),
                name: Some("Lab-2 Assignment".into()),
                due_at: Some(now + chrono::Duration::days(3)),
                points_possible: Some(10.0),
                submission_types: vec!["online_upload".into()],
                ..Default::default()
            },
            description: Some("<p>Requirements are in the <a href=\"/courses/1/files/9/download\">Lab-2 slides</a>.</p><ul><li>Implement the kernel</li><li>Report timings</li></ul>".into()),
            ..Default::default()
        };
        deliver(&mut app, effects, Payload::Detail(Box::new(detail)));
        let s = screen(&mut app, 100, 18);
        println!("{s}\n");
        assert!(
            s.contains("Due") && s.contains("Lab-2 slides") && s.contains("Implement the kernel")
        );
        // Refresh failure keeps content but marks it outdated.
        let effects = app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));
        if let Some(crate::app::Effect::Fetch { key, request }) = effects.into_iter().next() {
            assert_eq!(key, ResourceKey::Detail("1".into(), "7".into()));
            app.update(Msg::Fetched {
                key,
                request,
                result: Err(crate::canvas::CanvasError::Unauthenticated),
                at: now,
            });
        }
        let s = screen(&mut app, 100, 18);
        println!("{s}\n");
        assert!(
            s.contains("Outdated")
                && s.contains("press L to sign in")
                && s.contains("Implement the kernel")
        );
        // Narrow terminal still renders.
        let s = screen(&mut app, 40, 10);
        assert!(s.lines().count() == 10);
    }
}

/// Writes colour previews of each screen as HTML when TUICANVAS_PREVIEW is set
/// (development aid; run with `cargo test preview -- --ignored`).
#[cfg(test)]
mod preview {
    use super::*;
    use crate::app::{Effect, Msg, Payload};
    use crate::cache::ResourceKey;
    use crate::canvas::FetchStats;
    use crate::model::{AssignmentSummary, Course, Submission, Term};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::{Color, Modifier};

    fn css(c: Color, default: &str) -> String {
        match c {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Black => "#0c0c0c".into(),
            Color::White => "#f2f2f2".into(),
            Color::Gray => "#cccccc".into(),
            Color::DarkGray => "#767676".into(),
            Color::Red => "#c50f1f".into(),
            Color::Green => "#13a10e".into(),
            Color::Yellow => "#c19c00".into(),
            Color::Blue => "#0037da".into(),
            Color::Cyan => "#3a96dd".into(),
            _ => default.into(),
        }
    }

    fn html(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let b = t.backend().buffer();
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                let cell = &b[(x, y)];
                let mut fg = css(cell.fg, "#cccccc");
                let mut bg = css(cell.bg, "#0c0c0c");
                if cell.modifier.contains(Modifier::REVERSED) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let weight = if cell.modifier.contains(Modifier::BOLD) {
                    "bold"
                } else {
                    "normal"
                };
                let deco = if cell.modifier.contains(Modifier::UNDERLINED) {
                    "underline"
                } else {
                    "none"
                };
                let sym = match cell.symbol() {
                    "<" => "&lt;",
                    ">" => "&gt;",
                    "&" => "&amp;",
                    s => s,
                };
                out.push_str(&format!("<span style=\"color:{fg};background:{bg};font-weight:{weight};text-decoration:{deco}\">{sym}</span>"));
            }
            out.push('\n');
        }
        format!(
            "<pre style=\"font:14px/1.25 Cascadia Mono,Consolas,monospace;background:#0c0c0c;margin:0 0 18px;padding:6px;display:inline-block\">{out}</pre><br>"
        )
    }

    fn deliver(app: &mut App, effects: Vec<Effect>, payload: Payload) {
        for e in effects {
            if let Effect::Fetch { key, request } = e {
                app.update(Msg::Fetched {
                    key,
                    request,
                    result: Ok((
                        payload,
                        FetchStats {
                            requests: 1,
                            bytes: 7_644,
                            ms: 441.0,
                            ..Default::default()
                        },
                    )),
                    at: Utc::now(),
                });
                return;
            }
        }
    }

    #[test]
    #[ignore]
    fn preview() {
        let Some(path) = std::env::var_os("TUICANVAS_PREVIEW") else {
            return;
        };
        let now = Utc::now();
        let enter = || Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let down = || Msg::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let mut app = App::new("https://ufl.instructure.com", None);
        let effects = app.start();
        let term = Some(Term {
            name: Some("Fall 2026".into()),
        });
        let course = |id: &str, name: &str, code: &str, fav: bool| Course {
            id: id.into(),
            name: Some(name.into()),
            course_code: Some(code.into()),
            term: term.clone(),
            is_favorite: Some(fav),
            access_restricted_by_date: None,
        };
        deliver(
            &mut app,
            effects,
            Payload::Courses(vec![
                course("1", "GPU Computing", "EEL6763", true),
                course("2", "Operating Systems", "COP4600", true),
                course("3", "Compilers", "COP5621", false),
                course("4", "Technical Writing", "ENC3246", false),
            ]),
        );
        let mut page = String::from("<html><body style=\"background:#202020;padding:12px\">");
        page += &html(&mut app, 104, 9);
        let effects = app.update(enter());
        let a = |id: &str, name: &str, hours: i64, sub: Option<Submission>| AssignmentSummary {
            id: id.into(),
            name: Some(name.into()),
            due_at: Some(now + chrono::Duration::hours(hours)),
            points_possible: Some(10.0),
            submission_types: vec!["online_upload".into()],
            submission: sub,
            ..Default::default()
        };
        let graded = Submission {
            workflow_state: Some("graded".into()),
            submitted_at: Some(now),
            score: Some(9.0),
            ..Default::default()
        };
        let submitted = Submission {
            submitted_at: Some(now),
            workflow_state: Some("submitted".into()),
            ..Default::default()
        };
        let missing = Submission {
            missing: Some(true),
            workflow_state: Some("unsubmitted".into()),
            ..Default::default()
        };
        deliver(
            &mut app,
            effects,
            Payload::Assignments(vec![
                a("10", "Lab-1 Assignment", -24 * 20, Some(graded)),
                a("11", "Lab-1 Report", -24 * 6, Some(missing)),
                a("12", "Lab-2 Assignment", 7, Some(Submission::default())),
                a("13", "Quiz 3", 50, Some(submitted)),
                a(
                    "14",
                    "Lab-3 Assignment",
                    24 * 12,
                    Some(Submission::default()),
                ),
            ]),
        );
        app.update(down());
        app.update(down());
        page += &html(&mut app, 104, 9);
        // The assignment, with its files & links list. A temporary library
        // shows each local state: saved, not saved, downloading.
        let root = std::env::temp_dir().join(format!("tuicanvas-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lib = std::sync::Arc::new(crate::library::Library::open(&root, "ufl.instructure.com"));
        app.library = Some(lib.clone());
        let file =
            |id: &str, name: &str, ct: &str, size: u64, folder: &str| crate::model::CanvasFile {
                id: id.into(),
                display_name: Some(name.into()),
                content_type: Some(ct.into()),
                size: Some(size),
                updated_at: Some("2026-10-14T15:20:00Z".into()),
                url: Some(format!(
                    "https://ufl.instructure.com/files/{id}/download?verifier=v"
                )),
                folder_id: Some(folder.into()),
                ..Default::default()
            };
        let save = |f: &crate::model::CanvasFile, dir: &std::path::Path| {
            let part = lib.temp_dir().join(&f.id);
            std::fs::create_dir_all(lib.temp_dir()).unwrap();
            std::fs::write(&part, "x").unwrap();
            lib.install(&f.id, &part, f.version(), &dir.join(f.name()), false)
                .unwrap();
        };
        let effects = app.update(enter());
        let detail = crate::model::AssignmentDetail {
            base: a("12", "Lab-2 Assignment", 7, Some(Submission::default())),
            description: Some(concat!(
                "<h2>Overview</h2><p>Implement the tiled matrix multiply from lecture. The requirements are in the ",
                "<a href=\"/courses/1/files/9/download?verifier=x\">Lab-2 slides</a>; start from the ",
                "<a href=\"/courses/1/files/10/download\">starter code</a> and read ",
                "<a href=\"https://docs.nvidia.com/cuda/cuda-c-programming-guide/\">the CUDA guide</a>.</p>",
                "<p><img src=\"/courses/1/files/11/preview\" alt=\"Expected speedup\"></p>",
                "<ol><li>Implement the kernel</li><li>Report timings for three sizes, as in ",
                "<a href=\"/courses/1/assignments/10\">Lab-1</a></li></ol>",
                "<p>Questions? <a href=\"mailto:ta@ufl.edu\">Email the TA</a>.</p>"
            ).into()),
            allowed_attempts: Some(2),
            ..Default::default()
        };
        let effects = {
            let mut later = Vec::new();
            for e in effects {
                if let Effect::Fetch { key, request } = e {
                    later.extend(app.update(Msg::Fetched {
                        key,
                        request,
                        result: Ok((
                            Payload::Detail(Box::new(detail.clone())),
                            FetchStats {
                                requests: 1,
                                bytes: 4_610,
                                ms: 268.0,
                                ..Default::default()
                            },
                        )),
                        at: Utc::now(),
                    }));
                }
            }
            later
        };
        let slides = file("9", "Lab-2 slides.pdf", "application/pdf", 1_284_000, "1");
        let assignment_dir = lib.assignment_dir("EEL6763 GPU Computing", "Lab-2 Assignment");
        save(&slides, &assignment_dir);
        for e in effects {
            if let Effect::Fetch {
                key: ResourceKey::File(id),
                request,
            } = e
            {
                let meta = match id.as_str() {
                    "9" => slides.clone(),
                    "10" => file("10", "lab2-starter.zip", "application/zip", 48_200, "1"),
                    _ => file("11", "speedup.png", "image/png", 96_000, "1"),
                };
                app.update(Msg::Fetched {
                    key: ResourceKey::File(id),
                    request,
                    result: Ok((Payload::File(Box::new(meta)), FetchStats::default())),
                    at: Utc::now(),
                });
            }
        }
        app.update(Msg::Transfer {
            file_id: "11".into(),
            event: crate::app::TransferEvent::Progress {
                received: 41_000,
                total: Some(96_000),
            },
        });
        let tab = || Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.notice = None;
        page += &html(&mut app, 104, 26);
        app.update(tab());
        app.update(tab());
        page += &html(&mut app, 104, 26);

        // The course's Files tab.
        let esc = || Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.update(esc());
        app.update(esc());
        let effects = app.update(tab());
        let folder = |id: &str, full: &str, parent: Option<&str>| crate::model::CanvasFolder {
            id: id.into(),
            name: full.rsplit('/').next().map(str::to_string),
            full_name: Some(full.into()),
            parent_folder_id: parent.map(str::to_string),
            ..Default::default()
        };
        let syllabus = file(
            "20",
            "Syllabus Fall 2026.pdf",
            "application/pdf",
            212_000,
            "1",
        );
        let mut schedule = file(
            "21",
            "Course schedule.xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            18_400,
            "1",
        );
        let class_dir = lib.class_files_dir("EEL6763 GPU Computing");
        save(&syllabus, &class_dir);
        save(&schedule, &class_dir);
        schedule.updated_at = Some("2026-10-20T09:05:00Z".into());
        let listing = crate::app::CourseFilesData {
            folders: vec![
                folder("1", "course files", None),
                folder("2", "course files/Lectures", Some("1")),
                folder("3", "course files/Labs", Some("1")),
                folder("4", "course files/Exams", Some("1")),
            ],
            files: Some(vec![
                syllabus,
                schedule,
                file("22", "Lab-2 slides.pdf", "application/pdf", 1_284_000, "1"),
                file(
                    "23",
                    "CUDA cheat sheet.docx",
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                    64_000,
                    "1",
                ),
                file(
                    "24",
                    "Lecture 07 - Tiling.pptx",
                    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                    8_900_000,
                    "2",
                ),
            ]),
        };
        deliver(&mut app, effects, Payload::Files(Box::new(listing)));
        for _ in 0..5 {
            app.update(down());
        }
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::NONE,
        )));
        app.update(Msg::Transfer {
            file_id: "22".into(),
            event: crate::app::TransferEvent::Progress {
                received: 530_000,
                total: Some(1_284_000),
            },
        });
        app.notice = None;
        page += &html(&mut app, 104, 12);
        let _ = std::fs::remove_dir_all(&root);
        page += "</body></html>";
        std::fs::write(path, page).unwrap();
    }
}
