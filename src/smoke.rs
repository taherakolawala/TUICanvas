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

        // 7. Schema check for the fields the GraphQL list relies on.
        let wanted_assignment = ["_id", "name", "dueAt", "unlockAt", "lockAt", "pointsPossible", "submissionTypes", "htmlUrl", "position", "lockInfo", "submissionsConnection"];
        let wanted_submission = ["state", "submittedAt", "late", "missing", "excused", "score", "grade", "attempt"];
        let intro = client
            .graphql("query { a: __type(name: \"Assignment\") { fields { name } } s: __type(name: \"Submission\") { fields { name } } }", json!({}))
            .await;
        let names = |data: &Value, alias: &str| -> Vec<String> {
            data.pointer(&format!("/{alias}/fields")).and_then(Value::as_array).map(|f| {
                f.iter().filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_string)).collect()
            }).unwrap_or_default()
        };
        let missing = intro.as_ref().ok().map(|(d, _)| {
            let a = names(d, "a");
            let s = names(d, "s");
            json!({
                "assignment": wanted_assignment.iter().filter(|w| !a.iter().any(|n| n == *w)).collect::<Vec<_>>(),
                "submission": wanted_submission.iter().filter(|w| !s.iter().any(|n| n == *w)).collect::<Vec<_>>(),
            })
        });
        steps.push(json!({"name": "graphql_schema_fields", "outcome": outcome(&intro), "missing": missing, "error": err_detail(&intro)}));
        // Schema details for choosing the submission filter (type names only).
        let schema = client
            .graphql(
                "query {
                  a: __type(name: \"Assignment\") { fields { name args { name defaultValue type { name kind ofType { name } } } } }
                  st: __type(name: \"SubmissionState\") { enumValues { name } }
                  f: __type(name: \"SubmissionSearchFilterInput\") { inputFields { name defaultValue type { name kind ofType { name kind ofType { name } } } } }
                }",
                json!({}),
            )
            .await;
        let schema_summary = schema.as_ref().ok().map(|(d, _)| {
            let fields = d.pointer("/a/fields").and_then(Value::as_array).cloned().unwrap_or_default();
            let submission_fields: Vec<Value> = fields
                .iter()
                .filter(|f| f.get("name").and_then(Value::as_str).is_some_and(|n| n.to_lowercase().contains("submission")))
                .cloned()
                .collect();
            json!({"assignment_submission_fields": submission_fields, "submission_states": d.pointer("/st/enumValues"), "filter_input": d.pointer("/f/inputFields")})
        });
        steps.push(json!({"name": "graphql_schema_submissions", "outcome": outcome(&schema), "schema": schema_summary, "error": err_detail(&schema)}));

        // 8. REST vs GraphQL assignment lists on every active course: same
        //    assignments, due dates, status and order? Sizes and timings.
        let mut per_course = Vec::new();
        let (mut rest_ms, mut gql_ms) = (Vec::new(), Vec::new());
        let (mut rest_bytes, mut gql_bytes) = (0usize, 0usize);
        let mut totals = serde_json::Map::new();
        let mut tie_totals: std::collections::BTreeMap<&str, u32> = Default::default();
        let now = chrono::Utc::now();
        let all_courses = courses.as_ref().map(|(l, _)| l.clone()).unwrap_or_default();
        for (index, c) in all_courses.iter().enumerate() {
            let (rest, gql) = if index % 2 == 0 {
                let r = api::assignments(&client, &c.id).await;
                (r, api::assignments_graphql(&client, &c.id).await)
            } else {
                let g = api::assignments_graphql(&client, &c.id).await;
                (api::assignments(&client, &c.id).await, g)
            };
            let mut entry = serde_json::Map::new();
            entry.insert("rest".into(), outcome(&rest));
            entry.insert("graphql".into(), outcome(&gql));
            if gql.is_err() {
                entry.insert("graphql_error".into(), err_detail(&gql));
            }
            if let (Ok((r, rs)), Ok((g, gs))) = (&rest, &gql) {
                rest_ms.push(rs.ms);
                gql_ms.push(gs.ms);
                rest_bytes += rs.bytes;
                gql_bytes += gs.bytes;
                let mut diffs: Vec<Value> = Vec::new();
                let mut bump = |field: &str, detail: Value| {
                    let n = totals.entry(field.to_string()).or_insert(json!(0));
                    *n = json!(n.as_u64().unwrap_or(0) + 1);
                    if diffs.len() < 8 {
                        diffs.push(json!({"field": field, "detail": detail}));
                    }
                };
                for a in r {
                    let Some(b) = g.iter().find(|b| b.id == a.id) else { bump("only_in_rest", Value::Null); continue };
                    if a.due_at != b.due_at {
                        let secs = match (a.due_at, b.due_at) { (Some(x), Some(y)) => json!((x - y).num_seconds()), _ => json!("one side missing") };
                        bump("due_at", secs);
                    }
                    if a.points_possible.map(|p| (p * 100.0).round()) != b.points_possible.map(|p| (p * 100.0).round()) {
                        bump("points", Value::Null);
                    }
                    let (sa, sb) = (a.status(now).0, b.status(now).0);
                    if sa != sb {
                        bump("status", json!({"rest": sa, "graphql": sb}));
                    }
                    if a.locked_for_user.unwrap_or(false) != b.locked_for_user.unwrap_or(false) {
                        bump("locked", json!({"rest": a.locked_for_user, "graphql": b.locked_for_user}));
                    }
                    let (mut ta, mut tb) = (a.submission_types.clone(), b.submission_types.clone());
                    ta.sort();
                    tb.sort();
                    if ta != tb {
                        bump("submission_types", json!({"rest": ta, "graphql": tb}));
                    }
                    if a.html_url != b.html_url {
                        bump("html_url", json!({"graphql_present": b.html_url.is_some()}));
                    }
                }
                for b in g {
                    if !r.iter().any(|a| a.id == b.id) {
                        bump("only_in_graphql", Value::Null);
                    }
                }
                let order_r: Vec<&str> = r.iter().map(|a| a.id.as_str()).collect();
                let order_g: Vec<&str> = g.iter().map(|a| a.id.as_str()).collect();
                if order_r != order_g {
                    // Classify each out-of-place pair: tie on due date, or both undated?
                    let due_of = |id: &str| r.iter().find(|a| a.id == id).and_then(|a| a.due_at);
                    let mut kinds = serde_json::Map::new();
                    for (x, y) in order_r.iter().zip(order_g.iter()) {
                        if x != y {
                            let kind = match (due_of(x), due_of(y)) {
                                (Some(p), Some(q)) if p == q => "same_due",
                                (None, None) => "both_undated",
                                (Some(_), None) | (None, Some(_)) => "dated_vs_undated",
                                _ => "different_due",
                            };
                            let n = kinds.entry(kind).or_insert(json!(0));
                            *n = json!(n.as_u64().unwrap_or(0) + 1);
                        }
                    }
                    bump("order", Value::Object(kinds));
                }
                // Which tie-break reproduces Canvas's own REST order?
                let mut resorted = r.clone();
                api::sort_by_due(&mut resorted);
                entry.insert("rest_order_reproduced".into(), json!(resorted.iter().map(|a| &a.id).eq(r.iter().map(|a| &a.id))));
                let mut reproduced = Vec::new();
                for (name, cmp) in tie_breaks() {
                    let mut trial = r.clone();
                    trial.sort_by(|a, b| due_cmp(a, b).then_with(|| cmp(a, b)));
                    if trial.iter().map(|a| &a.id).eq(r.iter().map(|a| &a.id)) {
                        reproduced.push(name);
                        *tie_totals.entry(name).or_insert(0) += 1;
                    }
                }
                entry.insert("tie_breaks_reproducing_rest".into(), json!(reproduced));
                entry.insert("assignments".into(), json!(r.len()));
                entry.insert("rest".into(), stats_json(rs));
                entry.insert("graphql".into(), stats_json(gs));
                entry.insert("differences".into(), Value::Array(diffs));
            }
            per_course.push(Value::Object(entry));
        }
        steps.push(json!({
            "name": "list_equivalence_all_courses",
            "courses": per_course.len(),
            "difference_totals": totals,
            "rest_total_bytes": rest_bytes,
            "graphql_total_bytes": gql_bytes,
            "courses_reproduced_by_tie_break": tie_totals,
            "rest_median_ms": (median(&mut rest_ms.clone()) * 10.0).round() / 10.0,
            "graphql_median_ms": (median(&mut gql_ms.clone()) * 10.0).round() / 10.0,
            "per_course": per_course,
        }));
    });

    let exchanges = diag.recent();
    report.insert("steps".into(), Value::Array(steps));
    report.insert(
        "http".into(),
        json!(exchanges.iter().map(|e| json!({
        "method": e.method, "template": e.template, "status": e.status, "bytes": e.bytes,
        "ms": (e.ms * 10.0).round() / 10.0, "outcome": e.outcome,
        "rate_limit_remaining": e.rate_limit_remaining, "request_cost": e.request_cost, "error_hint": e.error_hint,
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

type Cmp = fn(&AssignmentSummary, &AssignmentSummary) -> std::cmp::Ordering;

fn due_cmp(a: &AssignmentSummary, b: &AssignmentSummary) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.due_at, b.due_at) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn numeric_id(a: &AssignmentSummary) -> u128 {
    a.id.replace('~', "").parse().unwrap_or(u128::MAX)
}

/// Case-insensitive comparison treating digit runs as numbers ("Lab 2" < "Lab 10").
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| {
        let mut parts: Vec<(u8, String, u128)> = Vec::new();
        let mut chars = s.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                let mut n = String::new();
                while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    n.push(d);
                    chars.next();
                }
                parts.push((0, String::new(), n.parse().unwrap_or(u128::MAX)));
            } else {
                let mut t = String::new();
                while let Some(&d) = chars.peek().filter(|d| !d.is_ascii_digit()) {
                    t.extend(d.to_lowercase());
                    chars.next();
                }
                parts.push((1, t, 0));
            }
        }
        parts
    };
    key(a).cmp(&key(b))
}

fn tie_breaks() -> Vec<(&'static str, Cmp)> {
    vec![
        ("name_bytes", |a, b| a.title().cmp(b.title())),
        ("name_ci", |a, b| {
            a.title().to_lowercase().cmp(&b.title().to_lowercase())
        }),
        ("name_natural_ci", |a, b| natural(a.title(), b.title())),
        ("name_ci_then_id", |a, b| {
            a.title()
                .to_lowercase()
                .cmp(&b.title().to_lowercase())
                .then_with(|| numeric_id(a).cmp(&numeric_id(b)))
        }),
        ("icu_like", |a, b| {
            icu_key(a.title(), false).cmp(&icu_key(b.title(), false))
        }),
        ("icu_like_numeric", |a, b| {
            icu_key(a.title(), true).cmp(&icu_key(b.title(), true))
        }),
        ("alnum_ci", |a, b| alnum(a.title()).cmp(&alnum(b.title()))),
        ("id", |a, b| numeric_id(a).cmp(&numeric_id(b))),
        ("position_then_id", |a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| numeric_id(a).cmp(&numeric_id(b)))
        }),
        ("position_then_name_ci", |a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| a.title().to_lowercase().cmp(&b.title().to_lowercase()))
        }),
    ]
}

/// Approximates ICU root collation at primary strength: whitespace < punctuation
/// < digits < letters, case-insensitive; optionally digit runs as numbers.
fn icu_key(s: &str, numeric: bool) -> Vec<(u8, u128, char)> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if numeric && c.is_ascii_digit() {
            let mut n = c.to_digit(10).unwrap_or(0) as u128;
            while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                n = n.saturating_mul(10).saturating_add(d as u128);
                chars.next();
            }
            out.push((2, n, '0'));
            continue;
        }
        let class = if c.is_whitespace() {
            0
        } else if c.is_alphanumeric() {
            if c.is_numeric() { 2 } else { 3 }
        } else {
            1
        };
        out.push((class, 0, c.to_lowercase().next().unwrap_or(c)));
    }
    out
}

fn alnum(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
