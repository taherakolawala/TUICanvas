//! Rendering. Only visible rows are built; description text is wrapped once
//! per assignment version and width, and styled only for the visible lines.

use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DetailRender, Origin, Resource, SyncState, ViewKind};
use crate::html;
use crate::model::{AssignmentDetail, AssignmentSummary, Tone, fmt_num};
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
                ("/", "filter"),
                ("r", "refresh"),
                ("o", "browser"),
                ("q", "quit"),
            ],
            ViewKind::Assignments { .. } => &[
                ("↑↓", "move"),
                ("Enter", "open"),
                ("Esc", "back"),
                ("/", "filter"),
                ("r", "refresh"),
                ("o", "browser"),
                ("q", "quit"),
            ],
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
    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let stats = if left_w + stats.width() + 2 <= width {
        stats
    } else {
        String::new()
    };
    let fill = width.saturating_sub(left_w + stats.width());
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(fill)));
    spans.push(Span::styled(stats, theme::faint()));
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
            spans.push(Span::styled(after[..close].to_string(), theme::link()));
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

    fn texts(line: &Line) -> Vec<String> {
        line.spans
            .iter()
            .filter(|s| !s.content.is_empty())
            .map(|s| s.content.to_string())
            .collect()
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
        let effects = app.update(enter());
        let detail = crate::model::AssignmentDetail {
            base: a("12", "Lab-2 Assignment", 7, Some(Submission::default())),
            description: Some("<h2>Overview</h2><p>Implement the tiled matrix multiply from lecture. The requirements are in the <a href=\"/courses/1/files/9/download\">Lab-2 slides</a>; submit a <strong>zip</strong> of your code.</p><ol><li>Implement the kernel</li><li>Report timings for three sizes</li></ol>".into()),
            allowed_attempts: Some(2),
            ..Default::default()
        };
        deliver(&mut app, effects, Payload::Detail(Box::new(detail)));
        page += &html(&mut app, 104, 22);
        app.update(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        )));
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::NONE,
        )));
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        )));
        page += &html(&mut app, 104, 9);
        page += "</body></html>";
        std::fs::write(path, page).unwrap();
    }
}
