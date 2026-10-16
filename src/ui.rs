//! Rendering. Only visible rows are built; description text is wrapped once
//! per assignment version and width.

use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DetailRender, Origin, Resource, SyncState, ViewKind};
use crate::html;
use crate::model::{AssignmentDetail, Tone, fmt_num};

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Good => Style::new().fg(Color::Green),
        Tone::Warn => Style::new().fg(Color::Yellow),
        Tone::Bad => Style::new().fg(Color::Red),
        Tone::Neutral => Style::new().fg(Color::Gray),
    }
}

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
            draw_assignments(frame, main, app, &course_id, now)
        }
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
                Style::new().fg(Color::Black).bg(Color::Yellow),
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
                Style::new().fg(Color::White).bg(Color::Red),
            )
        }
        SyncState::Idle => match (res.origin, res.fetched_at) {
            (Origin::Network, Some(at)) => (
                format!("Updated {}", at.with_timezone(&Local).format("%-I:%M %p")),
                Style::new().fg(Color::Black).bg(Color::Green),
            ),
            (Origin::Cache, Some(at)) => (
                format!("Saved copy from {}", age(at, now)),
                Style::new().fg(Color::Black).bg(Color::Yellow),
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
    };
    let width = usize::from(area.width);
    let status = truncate(&status, width.saturating_sub(12).max(10));
    let status_w = status.width() + 2;
    let left = truncate(&crumbs.join(" › "), width.saturating_sub(status_w + 1));
    let fill = width.saturating_sub(left.width() + status_w);
    let line = Line::from(vec![
        Span::styled(left, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(fill)),
        Span::styled(format!(" {status} "), style),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::new().bg(Color::DarkGray).fg(Color::White)),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let view = app.view();
    let (left, style) = if view.filtering {
        (
            format!("/{}▏  Enter keep · Esc clear", view.filter),
            Style::new().fg(Color::Cyan),
        )
    } else if app.login_required {
        (
            "Signed out or session expired — press L to sign in".to_string(),
            Style::new().fg(Color::Black).bg(Color::Yellow),
        )
    } else if let Some(notice) = &app.notice {
        (notice.clone(), Style::new().fg(Color::Cyan))
    } else {
        let hints = match view.kind {
            ViewKind::Courses => "↑↓ move  Enter open  / filter  r refresh  o browser  q quit",
            ViewKind::Assignments { .. } => {
                "↑↓ move  Enter open  Esc back  / filter  r refresh  o browser  q quit"
            }
            ViewKind::Detail { .. } => "↑↓ scroll  Esc back  r refresh  o open in browser  q quit",
        };
        let filter = if view.filter.is_empty() {
            String::new()
        } else {
            format!("[filter: {}]  ", view.filter)
        };
        (format!("{filter}{hints}"), Style::new().fg(Color::Gray))
    };
    let stats = app
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
    let width = usize::from(area.width);
    let stats = if stats.width() + 20 < width {
        stats
    } else {
        String::new()
    };
    let left = truncate(&left, width.saturating_sub(stats.width() + 1));
    let fill = width.saturating_sub(left.width() + stats.width());
    let line = Line::from(vec![
        Span::styled(left, style),
        Span::raw(" ".repeat(fill)),
        Span::styled(stats, Style::new().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn placeholder<T>(frame: &mut Frame, area: Rect, res: Option<&Resource<T>>, what: &str) -> bool {
    let res_has_data = res.is_some_and(|r| r.data.is_some());
    if res_has_data {
        return false;
    }
    let lines: Vec<Line> = match res.map(|r| &r.sync) {
        Some(SyncState::Failed(e)) => vec![
            Line::styled(
                format!("Could not load {what}: {}", e.describe()),
                Style::new().fg(Color::Red),
            ),
            Line::raw(""),
            Line::raw(if e.needs_login() {
                "Press L to sign in, or q to quit."
            } else {
                "Press r to retry."
            }),
        ],
        _ => vec![Line::raw(format!("Loading {what}…"))],
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
        Paragraph::new(text).alignment(Alignment::Center),
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

fn selected_style() -> Style {
    Style::new()
        .bg(Color::Blue)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
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
                Cell::from(if c.is_favorite == Some(true) {
                    "★"
                } else {
                    " "
                }),
                Cell::from(c.title().to_string()),
                Cell::from(c.course_code.clone().unwrap_or_default())
                    .style(Style::new().fg(Color::Gray)),
                Cell::from(
                    c.term
                        .as_ref()
                        .and_then(|t| t.name.clone())
                        .unwrap_or_default(),
                )
                .style(Style::new().fg(Color::DarkGray)),
            ]);
            if start + i == selected {
                row.style(selected_style())
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
    .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
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
                Cell::from(due),
                Cell::from(a.title().to_string()),
                Cell::from(status).style(tone_style(tone)),
                Cell::from(
                    a.points_possible
                        .map(|p| format!("{} pts", fmt_num(p)))
                        .unwrap_or_default(),
                ),
            ]);
            if start + i == selected {
                row.style(selected_style())
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
    let header = Row::new(vec!["Due", "Assignment", "Status", "Points"])
        .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    frame.render_widget(
        Table::new(built, widths).header(header).column_spacing(1),
        area,
    );
}

fn detail_header(d: &AssignmentDetail, course: &str, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let a = &d.base;
    let label = |s: &str| Span::styled(format!("{s:<12}"), Style::new().fg(Color::Gray));
    let mut lines = vec![
        Line::styled(
            html::sanitize(a.title()),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![label("Course"), Span::raw(html::sanitize(course))]),
    ];
    let due = match a.due_at {
        Some(due) => {
            let style = if due < now {
                Style::new().fg(Color::Red)
            } else {
                Style::new().fg(Color::Yellow)
            };
            Line::from(vec![
                label("Due"),
                Span::styled(
                    format!("{} ({})", long_date(due), relative(due, now)),
                    style,
                ),
            ])
        }
        None => Line::from(vec![label("Due"), Span::raw("No due date")]),
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
    let mut status_text = status;
    if let Some(sub) = &a.submission {
        if let Some(at) = sub.submitted_at {
            status_text.push_str(&format!(" · {}", short_date(at, now)));
        }
        if sub.attempt.unwrap_or(0) > 1 {
            status_text.push_str(&format!(" · attempt {}", sub.attempt.unwrap_or(0)));
        }
    }
    lines.push(Line::from(vec![
        label("Status"),
        Span::styled(status_text, tone_style(tone)),
    ]));
    if let Some(p) = a.points_possible {
        lines.push(Line::from(vec![label("Points"), Span::raw(fmt_num(p))]));
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
            Span::raw(format!("{}{attempts}", types.join(", "))),
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
            Span::styled(why, Style::new().fg(Color::Red)),
        ]));
    }
    lines
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
        Style::new().fg(Color::DarkGray),
    ));
    let body = &app.detail_render.as_ref().expect("rendered").lines;
    if body.is_empty() {
        lines.push(Line::styled(
            "(No description)",
            Style::new().fg(Color::Gray),
        ));
    } else {
        lines.extend(body.iter().map(|l| Line::raw(l.clone())));
    }
    let height = usize::from(area.height);
    let max_scroll = lines.len().saturating_sub(height);
    let view = app.stack.last_mut().expect("view");
    view.scroll = view.scroll.min(max_scroll);
    let visible: Vec<Line> = lines.into_iter().skip(view.scroll).take(height).collect();
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
                        },
                    )),
                    at: Utc::now(),
                });
                return;
            }
        }
        panic!("no fetch");
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
