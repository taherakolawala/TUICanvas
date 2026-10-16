//! Typed views of the Canvas resources the reader displays.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// Canvas IDs arrive as strings (we request canvas-string-ids), but accept numbers too.
fn de_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        S(String),
        N(u64),
    }
    Ok(match Id::deserialize(d)? {
        Id::S(s) => s,
        Id::N(n) => n.to_string(),
    })
}

/// IDs are interpolated into request paths, so only digits (and shard `~`) are accepted.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_digit() || c == '~')
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Term {
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Course {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub course_code: Option<String>,
    #[serde(default)]
    pub term: Option<Term>,
    #[serde(default)]
    pub is_favorite: Option<bool>,
    #[serde(default)]
    pub access_restricted_by_date: Option<bool>,
}

impl Course {
    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .or(self.course_code.as_deref())
            .unwrap_or("(unnamed course)")
    }

    pub fn visible(&self) -> bool {
        self.access_restricted_by_date != Some(true) && valid_id(&self.id)
    }
}

/// Favorites first (Canvas dashboard behaviour), then alphabetical.
pub fn sort_courses(courses: &mut [Course]) {
    courses.sort_by(|a, b| {
        let fav = |c: &Course| c.is_favorite != Some(true);
        fav(a)
            .cmp(&fav(b))
            .then_with(|| a.title().to_lowercase().cmp(&b.title().to_lowercase()))
    });
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    #[serde(default)]
    pub workflow_state: Option<String>,
    #[serde(default)]
    pub submitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub late: Option<bool>,
    #[serde(default)]
    pub missing: Option<bool>,
    #[serde(default)]
    pub excused: Option<bool>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub grade: Option<String>,
    #[serde(default)]
    pub attempt: Option<i64>,
    #[serde(default)]
    pub graded_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssignmentSummary {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub unlock_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub lock_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub points_possible: Option<f64>,
    #[serde(default)]
    pub submission_types: Vec<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub locked_for_user: Option<bool>,
    #[serde(default)]
    pub submission: Option<Submission>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssignmentDetail {
    #[serde(flatten)]
    pub base: AssignmentSummary,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub lock_explanation: Option<String>,
    #[serde(default)]
    pub allowed_attempts: Option<i64>,
    #[serde(default)]
    pub grading_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Warn,
    Bad,
    Neutral,
}

impl AssignmentSummary {
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or("(untitled assignment)")
    }

    fn expects_submission(&self) -> bool {
        !self.submission_types.is_empty()
            && !self
                .submission_types
                .iter()
                .all(|t| t == "none" || t == "on_paper" || t == "not_graded")
    }

    /// Short student-facing status derived only from Canvas's own fields.
    pub fn status(&self, now: DateTime<Utc>) -> (String, Tone) {
        let sub = self.submission.as_ref();
        let points = |score: f64| match self.points_possible {
            Some(p) => format!("{}/{}", fmt_num(score), fmt_num(p)),
            None => fmt_num(score),
        };
        if sub.and_then(|s| s.excused) == Some(true) {
            return ("Excused".into(), Tone::Neutral);
        }
        if let Some(s) = sub {
            if s.workflow_state.as_deref() == Some("graded") {
                if let Some(score) = s.score {
                    return (format!("Graded {}", points(score)), Tone::Good);
                }
                if let Some(grade) = s.grade.as_deref().filter(|g| !g.is_empty()) {
                    return (format!("Graded {grade}"), Tone::Good);
                }
            }
            if s.submitted_at.is_some() {
                let late = s.late == Some(true);
                return (
                    if late { "Submitted late" } else { "Submitted" }.into(),
                    if late { Tone::Warn } else { Tone::Good },
                );
            }
            if s.missing == Some(true) {
                return ("Missing".into(), Tone::Bad);
            }
        }
        if self.locked_for_user == Some(true) {
            return ("Locked".into(), Tone::Neutral);
        }
        if !self.expects_submission() {
            return ("No online submission".into(), Tone::Neutral);
        }
        match self.due_at {
            Some(due) if due < now => ("Not submitted (past due)".into(), Tone::Bad),
            _ => ("Not submitted".into(), Tone::Warn),
        }
    }
}

pub fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e12 {
        format!("{}", n as i64)
    } else {
        format!("{:.2}", n)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn parses_canvas_assignment_with_numeric_or_string_ids() {
        let a: AssignmentDetail = serde_json::from_str(
            r#"{"id": 12, "name": "Lab-2", "due_at": "2026-10-20T03:59:59Z", "points_possible": 10.0,
                "submission_types": ["online_upload"], "description": "<p>x</p>",
                "submission": {"workflow_state": "unsubmitted", "submitted_at": null, "missing": false},
                "unknown_field": {"nested": true}}"#,
        )
        .unwrap();
        assert_eq!(a.base.id, "12");
        assert_eq!(a.description.as_deref(), Some("<p>x</p>"));
        let b: Course =
            serde_json::from_str(r#"{"id": "123~4", "name": "GPU Computing"}"#).unwrap();
        assert_eq!(b.id, "123~4");
        assert!(b.visible());
    }

    #[test]
    fn status_reflects_submission_fields() {
        let now = at("2026-10-15T20:00:00Z");
        let mut a = AssignmentSummary {
            id: "1".into(),
            submission_types: vec!["online_upload".into()],
            points_possible: Some(10.0),
            due_at: Some(at("2026-10-14T03:59:00Z")),
            ..Default::default()
        };
        assert_eq!(
            a.status(now),
            ("Not submitted (past due)".into(), Tone::Bad)
        );
        a.due_at = Some(at("2026-10-20T03:59:00Z"));
        assert_eq!(a.status(now).0, "Not submitted");
        a.submission = Some(Submission {
            submitted_at: Some(now),
            late: Some(true),
            ..Default::default()
        });
        assert_eq!(a.status(now).0, "Submitted late");
        a.submission = Some(Submission {
            workflow_state: Some("graded".into()),
            score: Some(9.5),
            ..Default::default()
        });
        assert_eq!(a.status(now).0, "Graded 9.5/10");
        a.submission = Some(Submission {
            missing: Some(true),
            ..Default::default()
        });
        assert_eq!(a.status(now).0, "Missing");
        a.submission = Some(Submission {
            excused: Some(true),
            ..Default::default()
        });
        assert_eq!(a.status(now).0, "Excused");
        a.submission = None;
        a.submission_types = vec!["none".into()];
        assert_eq!(a.status(now).0, "No online submission");
    }

    #[test]
    fn favorites_sort_first() {
        let mut list = vec![
            Course {
                id: "1".into(),
                name: Some("b".into()),
                ..Default::default()
            },
            Course {
                id: "2".into(),
                name: Some("z".into()),
                is_favorite: Some(true),
                ..Default::default()
            },
            Course {
                id: "3".into(),
                name: Some("A".into()),
                ..Default::default()
            },
        ];
        sort_courses(&mut list);
        let ids: Vec<_> = list.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["2", "3", "1"]);
    }
}
