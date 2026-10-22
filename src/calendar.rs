//! The home-screen calendar: what is due and scheduled in the next two weeks
//! across all courses, from Canvas's planner in one read.
//!
//! Measured on 17 courses (2026-10-22): `GET /api/v1/planner/items` for 14
//! days returned 3 items in one request (2.7 KB, about 0.8 s), with each
//! item's course and the student's submission state. The calendar-events
//! read needs one request per 10 courses and has no submission state.

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::canvas::{CanvasClient, CanvasError, FetchStats};

/// How far ahead the calendar looks.
pub const DAYS: i64 = 14;

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

/// From the start of today (local time) to `DAYS` days later.
pub fn window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let today = now.with_timezone(&Local).date_naive();
    let start = Local
        .from_local_datetime(&today.and_hms_opt(0, 0, 0).expect("midnight"))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(now);
    (start, start + Duration::days(DAYS))
}

pub fn planner_path(now: DateTime<Utc>) -> String {
    let (start, end) = window(now);
    let fmt = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    format!(
        "/api/v1/planner/items?start_date={}&end_date={}&per_page=100",
        fmt(start),
        fmt(end)
    )
}

/// Everything in the window, in date order.
pub async fn read(client: &CanvasClient) -> Result<(Vec<Item>, FetchStats), CanvasError> {
    let (raw, stats): (Vec<RawItem>, _) =
        client.get_all_pages(&planner_path(Utc::now()), 5).await?;
    let mut items: Vec<Item> = raw.into_iter().filter_map(RawItem::into_item).collect();
    items.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.title.cmp(&b.title)));
    Ok((items, stats))
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
    fn window_starts_at_local_midnight_and_spans_two_weeks() {
        let now = Utc::now();
        let (start, end) = window(now);
        assert!(start <= now && now - start < Duration::hours(25));
        assert_eq!(end - start, Duration::days(DAYS));
        let path = planner_path(now);
        assert!(
            path.starts_with("/api/v1/planner/items?start_date=")
                && path.chars().all(|c| c.is_ascii_graphic())
        );
    }

    #[test]
    fn days_are_named_relative_to_today() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(today + Duration::days(1), today), "Tomorrow");
        assert_eq!(day_label(today + Duration::days(2), today), "Sat Oct 24");
    }
}
