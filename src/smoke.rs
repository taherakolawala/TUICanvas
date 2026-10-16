//! Read-only live verification (Milestone 1). Writes only metadata: statuses,
//! timings, sizes, counts, field presence. No names, bodies, cookies or URLs.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};

use crate::api;
use crate::canvas::{CanvasClient, CanvasError, FetchStats};
use crate::config::Config;
use crate::diag::Diagnostics;
use crate::model::AssignmentSummary;
use crate::session::{self, CookieJar, SessionFile};
use crate::transport::ReqwestTransport;

pub const DETAIL_GQL: &str = "query TuiAssignment($id: ID!) {
  assignment(id: $id) {
    _id name dueAt unlockAt lockAt pointsPossible submissionTypes description
    lockInfo { isLocked }
  }
}";

pub const LIST_GQL: &str = "query TuiAssignments($courseId: ID!) {
  course(id: $courseId) {
    assignmentsConnection(first: 100) {
      nodes {
        _id name dueAt pointsPossible submissionTypes
        lockInfo { isLocked }
        submissionsConnection(first: 1) { nodes { state submittedAt late missing score grade } }
      }
      pageInfo { hasNextPage }
    }
  }
}";

pub struct SmokeOptions {
    pub course: String,
    pub assignment: String,
}

fn outcome<T>(r: &Result<T, CanvasError>) -> Value {
    match r {
        Ok(_) => json!("ok"),
        Err(e) => json!(e.kind()),
    }
}

fn stats_json(s: &FetchStats) -> Value {
    json!({"requests": s.requests, "bytes": s.bytes, "ms": (s.ms * 10.0).round() / 10.0})
}

fn err_detail(r: &Result<impl Sized, CanvasError>) -> Value {
    match r {
        Err(CanvasError::GraphQl(msgs)) => json!(msgs),
        Err(e) => json!(e.describe()),
        Ok(_) => Value::Null,
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    if values.is_empty() {
        0.0
    } else {
        values[values.len() / 2]
    }
}

pub fn run(config: &Config, opts: SmokeOptions) -> Result<i32, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "runtime failed")?;
    let mut report = serde_json::Map::new();
    report.insert("version".into(), json!(1));
    report.insert("observed_at".into(), json!(chrono::Utc::now().to_rfc3339()));
    report.insert("origin_host".into(), json!(config.host));

    let session = match SessionFile::load(&config.session_path(), &config.origin) {
        Ok(Some(s)) => s,
        Ok(None) => {
            println!("No saved session. Sign in first (run tuicanvas, or tools/terminal_login).");
            return Ok(3);
        }
        Err(e) => return Err(e.0),
    };
    let now = session::now_seconds();
    let host_cookies = session.jar.count_for(&config.host, now);
    report.insert(
        "session".into(),
        json!({
            "found": true,
            "user_id_present": session.user_id.is_some(),
            "canvas_cookies": host_cookies,
            "age_hours": ((now - session.saved_at) / 360.0).round() / 10.0,
        }),
    );
    let diag = Arc::new(Diagnostics::new(None));
    let transport: Arc<ReqwestTransport> = Arc::new(ReqwestTransport::new()?);
    let client = CanvasClient::new(
        &config.origin,
        &config.host,
        transport.clone(),
        session.jar.clone(),
        diag.clone(),
    );
    let mut steps = Vec::new();
    let mut exit = 0;

    rt.block_on(async {
        // 1. Profile: the same minimal read the login helper uses.
        let profile = api::profile(&client).await;
        let profile_matches = match (&profile, session.user_id) {
            (Ok((p, _)), Some(saved)) => json!(p.id == saved),
            _ => Value::Null,
        };
        steps.push(json!({"name": "profile", "outcome": outcome(&profile),
            "stats": profile.as_ref().ok().map(|(_, s)| stats_json(s)), "matches_saved_user": profile_matches}));
        if let Err(e) = &profile {
            exit = if e.needs_login() { 4 } else { 1 };
            println!("profile: {}", e.describe());
            return;
        }

        // 2. Courses (paginated).
        let courses = api::courses(&client).await;
        let course = courses.as_ref().ok().and_then(|(list, _)| {
            let needle = opts.course.to_lowercase();
            list.iter().find(|c| c.title().to_lowercase().contains(&needle)
                || c.course_code.as_deref().unwrap_or("").to_lowercase().contains(&needle)).cloned()
        });
        steps.push(json!({"name": "courses", "outcome": outcome(&courses),
            "stats": courses.as_ref().ok().map(|(_, s)| stats_json(s)),
            "count": courses.as_ref().ok().map(|(l, _)| l.len()),
            "favorites": courses.as_ref().ok().map(|(l, _)| l.iter().filter(|c| c.is_favorite == Some(true)).count()),
            "selected_course_found": course.is_some(), "error": err_detail(&courses)}));
        let Some(course) = course else {
            exit = 1;
            println!("courses: {} (course matching '{}' found: no)", outcome(&courses), opts.course);
            return;
        };

        // 3. Assignment list with submissions (REST).
        let list = api::assignments(&client, &course.id).await;
        let assignment: Option<AssignmentSummary> = list.as_ref().ok().and_then(|(items, _)| {
            let needle = opts.assignment.to_lowercase();
            items.iter().find(|a| a.title().to_lowercase().contains(&needle)).or(items.first()).cloned()
        });
        if let Ok((items, stats)) = &list {
            steps.push(json!({"name": "assignments_rest", "outcome": "ok", "stats": stats_json(stats),
                "count": items.len(),
                "with_due_at": items.iter().filter(|a| a.due_at.is_some()).count(),
                "with_submission": items.iter().filter(|a| a.submission.is_some()).count(),
                "submitted": items.iter().filter(|a| a.submission.as_ref().is_some_and(|s| s.submitted_at.is_some())).count(),
                "locked_for_user": items.iter().filter(|a| a.locked_for_user == Some(true)).count(),
                "selected_matches_name": items.iter().any(|a| a.title().to_lowercase().contains(&opts.assignment.to_lowercase()))}));
        } else {
            steps.push(json!({"name": "assignments_rest", "outcome": outcome(&list), "error": err_detail(&list)}));
        }

        // 4. GraphQL list variant: is a field-selected list smaller/faster?
        let gql_list = client.graphql(LIST_GQL, json!({"courseId": course.id})).await;
        steps.push(json!({"name": "assignments_graphql", "outcome": outcome(&gql_list),
            "stats": gql_list.as_ref().ok().map(|(_, s)| stats_json(s)),
            "count": gql_list.as_ref().ok().and_then(|(d, _)| d.pointer("/course/assignmentsConnection/nodes")).and_then(Value::as_array).map(Vec::len),
            "has_next_page": gql_list.as_ref().ok().and_then(|(d, _)| d.pointer("/course/assignmentsConnection/pageInfo/hasNextPage")).cloned(),
            "error": err_detail(&gql_list)}));

        // 5. Assignment detail: REST vs GraphQL, alternated, three runs each.
        let Some(assignment) = assignment else {
            println!("assignments: none available");
            exit = 1;
            return;
        };
        let (mut rest_ms, mut gql_ms) = (Vec::new(), Vec::new());
        let (mut rest_last, mut gql_last) = (None, None);
        for _ in 0..3 {
            let t = Instant::now();
            let r = api::assignment(&client, &course.id, &assignment.id).await;
            rest_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            rest_last = Some(r);
            let t = Instant::now();
            let g = client.graphql(DETAIL_GQL, json!({"id": assignment.id})).await;
            gql_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            gql_last = Some(g);
        }
        let rest = rest_last.unwrap();
        let gql = gql_last.unwrap();
        let rest_detail = rest.as_ref().ok().map(|(d, s)| json!({
            "bytes": s.bytes,
            "has_description": d.description.as_deref().is_some_and(|x| !x.is_empty()),
            "description_chars": d.description.as_deref().map(str::len),
            "has_due_at": d.base.due_at.is_some(),
            "has_submission": d.base.submission.is_some(),
            "html_url_same_origin": d.base.html_url.as_deref().map(|u| u.starts_with(&config.origin)),
        }));
        steps.push(json!({"name": "detail_rest", "outcome": outcome(&rest), "runs_ms": rest_ms.iter().map(|m| (m * 10.0).round() / 10.0).collect::<Vec<_>>(),
            "median_ms": (median(&mut rest_ms.clone()) * 10.0).round() / 10.0, "detail": rest_detail, "error": err_detail(&rest)}));
        let gql_detail = gql.as_ref().ok().map(|(d, s)| json!({
            "bytes": s.bytes,
            "has_description": d.pointer("/assignment/description").and_then(Value::as_str).is_some_and(|x| !x.is_empty()),
            "due_matches_rest": match (&rest, d.pointer("/assignment/dueAt").and_then(Value::as_str)) {
                (Ok((r, _)), Some(due)) => json!(r.base.due_at.map(|x| x.to_rfc3339()) == chrono::DateTime::parse_from_rfc3339(due).ok().map(|x| x.with_timezone(&chrono::Utc).to_rfc3339())),
                (Ok((r, _)), None) => json!(r.base.due_at.is_none()),
                _ => Value::Null,
            },
        }));
        steps.push(json!({"name": "detail_graphql", "outcome": outcome(&gql), "runs_ms": gql_ms.iter().map(|m| (m * 10.0).round() / 10.0).collect::<Vec<_>>(),
            "median_ms": (median(&mut gql_ms.clone()) * 10.0).round() / 10.0, "detail": gql_detail, "error": err_detail(&gql)}));

        // 6. Failure classification and CSRF requirement.
        let no_csrf = client.graphql_with(DETAIL_GQL, json!({"id": assignment.id}), false).await;
        steps.push(json!({"name": "graphql_without_csrf_header", "outcome": outcome(&no_csrf), "error": err_detail(&no_csrf)}));
        let anonymous = CanvasClient::new(&config.origin, &config.host, transport.clone(), CookieJar::default(), diag.clone());
        let anon = api::profile(&anonymous).await;
        steps.push(json!({"name": "profile_without_session", "outcome": outcome(&anon), "expected": "unauthenticated"}));
        let foreign: Result<(Value, FetchStats), CanvasError> = client.get_json("/api/v1/courses/1").await;
        steps.push(json!({"name": "course_not_enrolled", "outcome": outcome(&foreign), "expected": "no_access or not_found"}));
    });

    let exchanges = diag.recent();
    report.insert("steps".into(), Value::Array(steps));
    report.insert(
        "http".into(),
        json!(exchanges.iter().map(|e| json!({
        "method": e.method, "template": e.template, "status": e.status, "bytes": e.bytes,
        "ms": (e.ms * 10.0).round() / 10.0, "outcome": e.outcome,
        "rate_limit_remaining": e.rate_limit_remaining, "request_cost": e.request_cost,
    })).collect::<Vec<_>>()),
    );

    // Persist cookies Canvas rotated during the run, preserving all other state.
    let mut saved = false;
    if let Some(jar) = client.take_changed_jar() {
        let mut updated = session.clone();
        updated.jar = jar;
        if SessionFile::parse(&updated.to_json(), &config.origin).is_ok() {
            saved = updated.save(&config.session_path()).is_ok();
        }
    }
    report.insert("rotated_cookies_saved".into(), json!(saved));

    let dir = config.private_reports();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("smoke.json");
    let text = serde_json::to_string_pretty(&Value::Object(report)).unwrap_or_default();
    std::fs::write(&path, &text).map_err(|_| "Could not write the smoke report.".to_string())?;
    println!("{text}");
    println!("Report written to benchmarks/private/smoke.json");
    Ok(exit)
}
