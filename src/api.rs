//! The reviewed set of read operations the reader performs.

use crate::canvas::{CanvasClient, CanvasError, FetchStats};
use crate::model::{self, AssignmentDetail, AssignmentSummary, Course};

pub const PROFILE: &str = "/api/v1/users/self/profile";

pub fn courses_path() -> String {
    "/api/v1/courses?enrollment_state=active&include[]=term&include[]=favorites&per_page=100".into()
}

pub fn assignments_path(course_id: &str) -> String {
    format!(
        "/api/v1/courses/{course_id}/assignments?include[]=submission&order_by=due_at&per_page=100"
    )
}

pub fn assignment_path(course_id: &str, assignment_id: &str) -> String {
    format!("/api/v1/courses/{course_id}/assignments/{assignment_id}?include[]=submission")
}

fn check(id: &str) -> Result<(), CanvasError> {
    if model::valid_id(id) {
        Ok(())
    } else {
        Err(CanvasError::Invalid("invalid identifier"))
    }
}

pub async fn courses(client: &CanvasClient) -> Result<(Vec<Course>, FetchStats), CanvasError> {
    let (mut list, stats): (Vec<Course>, _) = client.get_all_pages(&courses_path(), 10).await?;
    list.retain(Course::visible);
    model::sort_courses(&mut list);
    Ok((list, stats))
}

/// The list read the reader uses: field-selected GraphQL (about 12x less data
/// and half the median latency of REST on this account, with identical
/// assignments, due dates and status on every course), falling back to the
/// documented REST list if Canvas rejects the query itself.
pub async fn assignment_list(
    client: &CanvasClient,
    course_id: &str,
) -> Result<(Vec<AssignmentSummary>, FetchStats), CanvasError> {
    match assignments_graphql(client, course_id).await {
        Err(CanvasError::GraphQl(_) | CanvasError::Invalid(_) | CanvasError::Csrf) => {
            assignments(client, course_id).await
        }
        other => other,
    }
}

pub async fn assignments(
    client: &CanvasClient,
    course_id: &str,
) -> Result<(Vec<AssignmentSummary>, FetchStats), CanvasError> {
    check(course_id)?;
    let (mut list, stats): (Vec<AssignmentSummary>, _) = client
        .get_all_pages(&assignments_path(course_id), 20)
        .await?;
    list.retain(|a| model::valid_id(&a.id));
    Ok((list, stats))
}

pub async fn assignment(
    client: &CanvasClient,
    course_id: &str,
    assignment_id: &str,
) -> Result<(AssignmentDetail, FetchStats), CanvasError> {
    check(course_id)?;
    check(assignment_id)?;
    client
        .get_json(&assignment_path(course_id, assignment_id))
        .await
}

#[derive(serde::Deserialize)]
pub struct Profile {
    #[serde(deserialize_with = "de_u64")]
    pub id: u64,
}

fn de_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        S(String),
        N(u64),
    }
    match N::deserialize(d)? {
        N::N(n) => Ok(n),
        N::S(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

pub async fn profile(client: &CanvasClient) -> Result<(Profile, FetchStats), CanvasError> {
    client.get_json(PROFILE).await
}

/// Field-selected assignment list (no descriptions). Candidate replacement
/// for the REST list; adopted only if it matches REST on every course.
pub const ASSIGNMENTS_GQL: &str = "query TuiAssignments($courseId: ID!, $after: String) {
  course(id: $courseId) {
    assignmentsConnection(first: 100, after: $after) {
      nodes {
        _id name dueAt unlockAt lockAt pointsPossible submissionTypes htmlUrl position
        lockInfo { isLocked }
        # The filter defaults to submitted/pending_review/graded, which hides the
        # unsubmitted placeholder that carries the student's \"missing\" flag.
        submissionsConnection(first: 1, filter: {states: [unsubmitted, submitted, pending_review, graded, ungraded]}) {
          nodes { state submittedAt late missing excused score grade attempt }
        }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlSubmission {
    state: Option<String>,
    submitted_at: Option<chrono::DateTime<chrono::Utc>>,
    late: Option<bool>,
    missing: Option<bool>,
    excused: Option<bool>,
    score: Option<f64>,
    grade: Option<String>,
    attempt: Option<i64>,
}

#[derive(serde::Deserialize)]
struct GqlLock {
    #[serde(rename = "isLocked")]
    is_locked: Option<bool>,
}

#[derive(serde::Deserialize)]
struct GqlNodes<T> {
    nodes: Vec<T>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlAssignment {
    #[serde(rename = "_id")]
    id: String,
    name: Option<String>,
    due_at: Option<chrono::DateTime<chrono::Utc>>,
    unlock_at: Option<chrono::DateTime<chrono::Utc>>,
    lock_at: Option<chrono::DateTime<chrono::Utc>>,
    points_possible: Option<f64>,
    submission_types: Option<Vec<String>>,
    html_url: Option<String>,
    position: Option<i64>,
    lock_info: Option<GqlLock>,
    submissions_connection: Option<GqlNodes<GqlSubmission>>,
}

impl GqlAssignment {
    fn into_summary(self) -> AssignmentSummary {
        let submission = self
            .submissions_connection
            .and_then(|c| c.nodes.into_iter().next())
            .map(|s| crate::model::Submission {
                workflow_state: s.state,
                submitted_at: s.submitted_at,
                late: s.late,
                missing: s.missing,
                excused: s.excused,
                score: s.score,
                grade: s.grade,
                attempt: s.attempt,
                graded_at: None,
            });
        AssignmentSummary {
            id: self.id,
            name: self.name,
            due_at: self.due_at,
            unlock_at: self.unlock_at,
            lock_at: self.lock_at,
            points_possible: self.points_possible,
            submission_types: self.submission_types.unwrap_or_default(),
            html_url: self.html_url,
            locked_for_user: self.lock_info.and_then(|l| l.is_locked),
            submission,
            position: self.position,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPage {
    nodes: Vec<GqlAssignment>,
    page_info: GqlPageInfo,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

/// Same ordering as the REST list (`order_by=due_at`): by due date with undated
/// last, then case-insensitive name, then id. Measured against Canvas's own
/// order on 17 courses: identical on 16; the other differs only among undated items.
pub fn sort_by_due(list: &mut [AssignmentSummary]) {
    use std::cmp::Ordering;
    fn nulls_last<T: Ord>(a: &Option<T>, b: &Option<T>) -> Ordering {
        match (a, b) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
    let numeric = |id: &str| id.replace('~', "").parse::<u128>().unwrap_or(u128::MAX);
    list.sort_by(|a, b| {
        nulls_last(&a.due_at, &b.due_at)
            .then_with(|| a.title().to_lowercase().cmp(&b.title().to_lowercase()))
            .then_with(|| numeric(&a.id).cmp(&numeric(&b.id)))
    });
}

pub async fn assignments_graphql(
    client: &CanvasClient,
    course_id: &str,
) -> Result<(Vec<AssignmentSummary>, FetchStats), CanvasError> {
    check(course_id)?;
    let started = std::time::Instant::now();
    let mut stats = FetchStats::default();
    let mut list = Vec::new();
    let mut after: Option<String> = None;
    loop {
        if stats.requests >= 20 {
            return Err(CanvasError::Invalid("too many pages"));
        }
        let (data, page_stats) = client
            .graphql(
                ASSIGNMENTS_GQL,
                serde_json::json!({"courseId": course_id, "after": after}),
            )
            .await?;
        stats.requests += 1;
        stats.bytes += page_stats.bytes;
        let page: GqlPage = data
            .pointer("/course/assignmentsConnection")
            .cloned()
            .ok_or(CanvasError::Invalid("course not returned"))
            .and_then(|v| {
                serde_json::from_value(v)
                    .map_err(|_| CanvasError::Invalid("unexpected GraphQL shape"))
            })?;
        list.extend(page.nodes.into_iter().map(GqlAssignment::into_summary));
        match (page.page_info.has_next_page, page.page_info.end_cursor) {
            (true, Some(cursor)) => after = Some(cursor),
            (true, None) => return Err(CanvasError::Invalid("pagination cursor missing")),
            (false, _) => break,
        }
    }
    list.retain(|a| model::valid_id(&a.id));
    sort_by_due(&mut list);
    stats.ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok((list, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::tests::{FakeTransport, client, json};

    #[tokio::test]
    async fn list_falls_back_to_rest_when_query_is_rejected() {
        let t = FakeTransport::new(vec![
            json(
                200,
                r#"{"errors":[{"message":"Field 'position' doesn't exist on type 'Assignment'"}]}"#,
            ),
            json(
                200,
                r#"[{"id":"5","name":"Lab","due_at":null,"submission_types":["none"]}]"#,
            ),
        ]);
        let c = client(t.clone());
        let (list, _) = assignment_list(&c, "1").await.unwrap();
        assert_eq!(list[0].id, "5");
        assert!(
            t.requests.lock().unwrap()[1]
                .url
                .contains("/api/v1/courses/1/assignments")
        );
    }

    #[tokio::test]
    async fn list_does_not_mask_expired_sessions() {
        let t = FakeTransport::new(vec![json(
            401,
            r#"{"errors":[{"message":"user authorization required"}]}"#,
        )]);
        let c = client(t.clone());
        assert_eq!(
            assignment_list(&c, "1").await.unwrap_err(),
            CanvasError::Unauthenticated
        );
        assert_eq!(t.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn graphql_list_maps_fields_and_follows_cursor() {
        let page1 = r#"{"data":{"course":{"assignmentsConnection":{"nodes":[
            {"_id":"2","name":"B","dueAt":"2026-10-20T23:59:00-04:00","pointsPossible":10,"submissionTypes":["online_upload"],
             "htmlUrl":"https://ufl.instructure.com/courses/1/assignments/2","lockInfo":{"isLocked":false},
             "submissionsConnection":{"nodes":[{"state":"submitted","submittedAt":"2026-10-19T12:00:00Z","late":false,"missing":false,"excused":null,"score":null,"grade":null,"attempt":1}]}}],
            "pageInfo":{"hasNextPage":true,"endCursor":"abc"}}}}}"#;
        let page2 = r#"{"data":{"course":{"assignmentsConnection":{"nodes":[
            {"_id":"1","name":"A","dueAt":"2026-10-18T03:59:00Z","pointsPossible":5,"submissionTypes":["none"],"htmlUrl":null,
             "lockInfo":null,"submissionsConnection":{"nodes":[]}}],
            "pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#;
        let t = FakeTransport::new(vec![json(200, page1), json(200, page2)]);
        let c = client(t.clone());
        let (list, stats) = assignments_graphql(&c, "1").await.unwrap();
        assert_eq!(stats.requests, 2);
        assert_eq!(
            list.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["1", "2"]
        );
        let b = &list[1];
        assert_eq!(b.due_at.unwrap().to_rfc3339(), "2026-10-21T03:59:00+00:00");
        assert_eq!(
            b.submission.as_ref().unwrap().workflow_state.as_deref(),
            Some("submitted")
        );
        assert!(list[0].submission.is_none());
        let body = String::from_utf8(t.requests.lock().unwrap()[1].body.clone().unwrap()).unwrap();
        assert!(body.contains("\"after\":\"abc\""));
    }
}
