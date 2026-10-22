//! The home-screen calendar: a month grid of what is due and scheduled across
//! all courses, from Canvas's planner in one read per month shown.
//!
//! Measured on 17 courses (2026-10-22): `GET /api/v1/planner/items` for 14
//! days returned 3 items in one request (2.7 KB, about 0.8 s), with each
//! item's course and the student's submission state. The calendar-events
//! read needs one request per 10 courses and has no submission state.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::canvas::{CanvasClient, CanvasError, FetchStats};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Assignment,
    Quiz,
    Discussion,
    Event,
    Note,
    Page,
    Announcement,
    #[default]
    Other,
}

impl Kind {
    fn from_canvas(kind: &str) -> Kind {
        match kind {
            "assignment" => Kind::Assignment,
            "quiz" => Kind::Quiz,
            "discussion_topic" => Kind::Discussion,
            "calendar_event" => Kind::Event,
            "planner_note" => Kind::Note,
            "wiki_page" => Kind::Page,
            "announcement" => Kind::Announcement,
            _ => Kind::Other,
        }
    }

    /// Short label for the calendar.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Assignment => "Due",
            Kind::Quiz => "Quiz",
            Kind::Discussion => "Disc.",
            Kind::Event => "Event",
            Kind::Note => "Note",
            Kind::Page => "Page",
            Kind::Announcement => "News",
            Kind::Other => "Item",
        }
    }
}

/// The student's progress on an item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Progress {
    #[default]
    Open,
    Submitted,
    Graded,
    Excused,
    Missing,
    /// Marked done in the Canvas planner.
    Done,
}

impl Progress {
    pub fn finished(self) -> bool {
        matches!(
            self,
            Progress::Submitted | Progress::Graded | Progress::Excused | Progress::Done
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Progress::Open => "",
            Progress::Submitted => "submitted",
            Progress::Graded => "graded",
            Progress::Excused => "excused",
            Progress::Missing => "missing",
            Progress::Done => "done",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: Kind,
    pub id: String,
    pub course_id: Option<String>,
    pub course: Option<String>,
    pub title: String,
    /// When it is due or happens.
    pub at: Option<DateTime<Utc>>,
    pub points: Option<f64>,
    /// Canvas page, usually a path on the Canvas origin.
    pub url: Option<String>,
    pub progress: Progress,
}

impl Item {
    /// Local calendar day of the item.
    pub fn day(&self) -> Option<NaiveDate> {
        self.at.map(|at| at.with_timezone(&Local).date_naive())
    }

    /// Shown on the calendar: dated, not an announcement.
    pub fn listed(&self) -> bool {
        self.at.is_some() && self.kind != Kind::Announcement
    }
}

#[derive(Deserialize)]
struct RawSubmissions {
    #[serde(default)]
    submitted: bool,
    #[serde(default)]
    graded: bool,
    #[serde(default)]
    excused: bool,
    #[serde(default)]
    missing: bool,
}

#[derive(Deserialize)]
struct RawOverride {
    #[serde(default)]
    marked_complete: bool,
}

#[derive(Deserialize)]
struct RawPlannable {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    points_possible: Option<f64>,
}

#[derive(Deserialize)]
struct RawItem {
    #[serde(default)]
    plannable_id: Option<serde_json::Value>,
    #[serde(default)]
    plannable_type: Option<String>,
    #[serde(default)]
    plannable_date: Option<DateTime<Utc>>,
    #[serde(default)]
    course_id: Option<serde_json::Value>,
    #[serde(default)]
    context_name: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    plannable: Option<RawPlannable>,
    /// `false` when the item takes no submission, else the student's state.
    #[serde(default)]
    submissions: Option<serde_json::Value>,
    #[serde(default)]
    planner_override: Option<RawOverride>,
}

fn id_text(value: Option<serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

impl RawItem {
    fn into_item(self) -> Option<Item> {
        let id = id_text(self.plannable_id).filter(|id| crate::model::valid_id(id))?;
        let submissions: Option<RawSubmissions> = self
            .submissions
            .filter(|s| s.is_object())
            .and_then(|s| serde_json::from_value(s).ok());
        let progress = match (
            &submissions,
            self.planner_override
                .as_ref()
                .is_some_and(|o| o.marked_complete),
        ) {
            (Some(s), _) if s.excused => Progress::Excused,
            (Some(s), _) if s.graded => Progress::Graded,
            (Some(s), _) if s.submitted => Progress::Submitted,
            (_, true) => Progress::Done,
            (Some(s), _) if s.missing => Progress::Missing,
            _ => Progress::Open,
        };
        let plannable = self.plannable;
        Some(Item {
            kind: Kind::from_canvas(self.plannable_type.as_deref().unwrap_or("")),
            id,
            course_id: id_text(self.course_id).filter(|c| crate::model::valid_id(c)),
            course: self.context_name,
            title: plannable
                .as_ref()
                .and_then(|p| p.title.clone())
                .unwrap_or_else(|| "(untitled)".into()),
            at: self.plannable_date,
            points: plannable.and_then(|p| p.points_possible),
            url: self.html_url,
            progress,
        })
    }
}

/// `2026-10` for a day in October 2026: the key of that month's read.
pub fn month_key(day: NaiveDate) -> String {
    day.format("%Y-%m").to_string()
}

pub fn month_of(key: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&format!("{key}-01"), "%Y-%m-%d").ok()
}

/// The month grid around `day`: its first shown day (a Sunday on or before
/// the 1st) and how many weeks it spans (to the Saturday on or after the last day).
pub fn grid(day: NaiveDate) -> (NaiveDate, usize) {
    let first = day.with_day(1).expect("day 1 exists");
    let start = first - Duration::days(i64::from(first.weekday().num_days_from_sunday()));
    let next_month = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
    }
    .expect("next month exists");
    let last = next_month - Duration::days(1);
    let end = last + Duration::days(i64::from(6 - last.weekday().num_days_from_sunday()));
    (start, ((end - start).num_days() as usize + 1) / 7)
}

/// The same day number in another month, clamped to that month's length.
pub fn shift_month(day: NaiveDate, months: i32) -> NaiveDate {
    let total = day.year() * 12 + day.month0() as i32 + months;
    let (year, month0) = (total.div_euclid(12), total.rem_euclid(12) as u32);
    (1..=day.day())
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month0 + 1, d))
        .unwrap_or(day)
}

fn local_midnight(day: NaiveDate) -> DateTime<Utc> {
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight"))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight")))
}

/// The planner read covering the whole grid of `month` (`2026-10`).
pub fn planner_path(month: &str) -> Option<String> {
    let (start, weeks) = grid(month_of(month)?);
    let end = start + Duration::days(7 * weeks as i64);
    let fmt = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    Some(format!(
        "/api/v1/planner/items?start_date={}&end_date={}&per_page=100",
        fmt(local_midnight(start)),
        fmt(local_midnight(end))
    ))
}

/// Everything on the month's grid, in date order.
pub async fn read(
    client: &CanvasClient,
    month: &str,
) -> Result<(Vec<Item>, FetchStats), CanvasError> {
    let path = planner_path(month).ok_or(CanvasError::Invalid("invalid month"))?;
    let (raw, stats): (Vec<RawItem>, _) = client.get_all_pages(&path, 10).await?;
    let mut items: Vec<Item> = raw.into_iter().filter_map(RawItem::into_item).collect();
    items.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.title.cmp(&b.title)));
    Ok((items, stats))
}

#[derive(Deserialize)]
struct RawColors {
    #[serde(default)]
    custom_colors: std::collections::HashMap<String, String>,
}

/// The colours the student picked for courses in Canvas, by course id.
pub async fn read_colors(
    client: &CanvasClient,
) -> Result<(std::collections::HashMap<String, String>, FetchStats), CanvasError> {
    let (raw, stats): (RawColors, _) = client.get_json("/api/v1/users/self/colors").await?;
    let colors = raw
        .custom_colors
        .into_iter()
        .filter_map(|(context, color)| {
            let id = context.strip_prefix("course_")?.to_string();
            (crate::model::valid_id(&id) && parse_hex(&color).is_some()).then_some((id, color))
        })
        .collect();
    Ok((colors, stats))
}

/// `#1a2b3c` -> (26, 43, 60).
pub fn parse_hex(color: &str) -> Option<(u8, u8, u8)> {
    let hex = color.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let part = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((part(0)?, part(2)?, part(4)?))
}

/// "Today", "Tomorrow", or "Fri Oct 24".
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    match (day - today).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        _ => day.format("%a %b %-d").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_items_map_progress_and_kind() {
        let raw: Vec<RawItem> = serde_json::from_value(serde_json::json!([
            {"plannable_id": "7", "plannable_type": "assignment", "plannable_date": "2026-10-23T03:59:00Z",
             "course_id": "10", "context_name": "GPU", "html_url": "/courses/10/assignments/7",
             "plannable": {"title": "Lab-3", "points_possible": 100.0},
             "submissions": {"submitted": true, "graded": false, "missing": false, "excused": false}},
            {"plannable_id": 8, "plannable_type": "quiz", "plannable_date": "2026-10-24T03:59:00Z",
             "course_id": 10, "plannable": {"title": "Quiz 4"},
             "submissions": {"submitted": false, "missing": true}},
            {"plannable_id": "9", "plannable_type": "calendar_event", "plannable_date": "2026-10-25T14:00:00Z",
             "plannable": {"title": "Exam review"}, "submissions": false},
            {"plannable_id": "11", "plannable_type": "planner_note", "plannable_date": "2026-10-25T14:00:00Z",
             "plannable": {"title": "Note"}, "planner_override": {"marked_complete": true}},
            {"plannable_id": "../x", "plannable_type": "assignment"}
        ]))
        .unwrap();
        let items: Vec<Item> = raw.into_iter().filter_map(RawItem::into_item).collect();
        assert_eq!(items.len(), 4, "unsafe ids are dropped");
        assert_eq!(
            (items[0].kind, items[0].progress),
            (Kind::Assignment, Progress::Submitted)
        );
        assert_eq!(items[0].course_id.as_deref(), Some("10"));
        assert_eq!(
            (items[1].kind, items[1].progress),
            (Kind::Quiz, Progress::Missing)
        );
        assert_eq!(
            items[1].course_id.as_deref(),
            Some("10"),
            "numeric ids accepted"
        );
        assert_eq!(
            (items[2].kind, items[2].progress),
            (Kind::Event, Progress::Open)
        );
        assert_eq!(items[3].progress, Progress::Done);
    }

    #[test]
    fn month_grids_start_on_sunday_and_cover_whole_weeks() {
        let oct = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let (start, weeks) = grid(oct);
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 9, 27).unwrap());
        assert_eq!(weeks, 5);
        let (start, weeks) = grid(NaiveDate::from_ymd_opt(2026, 8, 3).unwrap());
        assert_eq!(
            (start, weeks),
            (NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(), 6)
        );
        assert_eq!(month_key(oct), "2026-10");
        assert_eq!(
            shift_month(NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(), 1),
            NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
        );
        assert_eq!(
            shift_month(oct, -10),
            NaiveDate::from_ymd_opt(2025, 12, 22).unwrap()
        );
        let path = planner_path("2026-10").unwrap();
        assert!(
            path.starts_with("/api/v1/planner/items?start_date=2026-09-2")
                && path.chars().all(|c| c.is_ascii_graphic()),
            "{path}"
        );
        assert!(planner_path("../x").is_none());
        assert_eq!(parse_hex("#FA4616"), Some((250, 70, 22)));
        assert_eq!(parse_hex("red"), None);
    }

    #[test]
    fn days_are_named_relative_to_today() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(today + Duration::days(1), today), "Tomorrow");
        assert_eq!(day_label(today + Duration::days(2), today), "Sat Oct 24");
    }
}
