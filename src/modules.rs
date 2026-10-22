//! Course tabs (what the instructor shows), modules, and wiki pages.
//!
//! Canvas leaves hidden tabs out of `/courses/:id/tabs`, so the reader shows a
//! course section only when its tab is listed (or it is the course's home).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::canvas::{CanvasClient, CanvasError, FetchStats};
use crate::model::{de_id, de_opt_id, valid_id};

/// One course navigation tab as Canvas lists it for the student.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
    #[serde(default)]
    pub hidden: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Module {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
    /// `locked`, `unlocked`, `started` or `completed` (absent for teachers).
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub unlock_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub require_sequential_progress: Option<bool>,
    /// Canvas leaves items out of the module list for very large modules.
    #[serde(default)]
    pub items: Option<Vec<Item>>,
}

impl Module {
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("Untitled module")
    }
    pub fn items(&self) -> &[Item] {
        self.items.as_deref().unwrap_or(&[])
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub completed: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContentDetails {
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub points_possible: Option<f64>,
    #[serde(default)]
    pub locked_for_user: Option<bool>,
    #[serde(default)]
    pub lock_explanation: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// `File`, `Page`, `Discussion`, `Assignment`, `Quiz`, `SubHeader`,
    /// `ExternalUrl` or `ExternalTool`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub indent: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_id")]
    pub content_id: Option<String>,
    #[serde(default)]
    pub page_url: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub external_url: Option<String>,
    #[serde(default)]
    pub completion_requirement: Option<Requirement>,
    #[serde(default)]
    pub content_details: Option<ContentDetails>,
}

/// What a module item is, as far as the reader is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Heading,
    Assignment,
    Page,
    File,
    Quiz,
    Discussion,
    Link,
    Tool,
    Other,
}

impl Item {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or("Untitled item")
    }

    pub fn kind(&self) -> ItemKind {
        match self.kind.as_deref() {
            Some("SubHeader") => ItemKind::Heading,
            Some("Assignment") => ItemKind::Assignment,
            Some("Page") => ItemKind::Page,
            Some("File") => ItemKind::File,
            Some("Quiz") => ItemKind::Quiz,
            Some("Discussion") => ItemKind::Discussion,
            Some("ExternalUrl") => ItemKind::Link,
            Some("ExternalTool") => ItemKind::Tool,
            _ => ItemKind::Other,
        }
    }

    /// Four-letter label for the list.
    pub fn label(&self) -> &'static str {
        match self.kind() {
            ItemKind::Heading => "",
            ItemKind::Assignment => "ASGN",
            ItemKind::Page => "PAGE",
            ItemKind::File => "FILE",
            ItemKind::Quiz => "QUIZ",
            ItemKind::Discussion => "DISC",
            ItemKind::Link => "LINK",
            ItemKind::Tool => "TOOL",
            ItemKind::Other => "ITEM",
        }
    }

    /// A valid Canvas id of the item's content (assignment, file, ...).
    pub fn content(&self) -> Option<&str> {
        self.content_id.as_deref().filter(|id| valid_id(id))
    }

    /// The page's URL name, when it is safe to use in a request.
    pub fn page(&self) -> Option<&str> {
        self.page_url.as_deref().filter(|u| valid_slug(u))
    }

    pub fn due(&self) -> Option<DateTime<Utc>> {
        self.content_details.as_ref()?.due_at
    }

    pub fn locked(&self) -> bool {
        self.content_details
            .as_ref()
            .and_then(|d| d.locked_for_user)
            .unwrap_or(false)
    }

    /// `Some(done)` when the module asks for something on this item.
    pub fn requirement(&self) -> Option<(&str, bool)> {
        let r = self.completion_requirement.as_ref()?;
        let what = match r.kind.as_deref() {
            Some("must_view") => "view",
            Some("must_submit") => "submit",
            Some("must_contribute") => "contribute",
            Some("min_score") | Some("min_percentage") => "score",
            Some("must_mark_done") => "mark done",
            _ => "complete",
        };
        Some((what, r.completed == Some(true)))
    }
}

/// Canvas page URL names are slugs (`week-1-overview`). Anything else is not
/// put into a request path or a file name.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 200
        && !slug.starts_with('.')
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// Stands for the course's front page (its Canvas Home when the home is a
/// page). Never a real page name: those cannot start with a dot.
pub const FRONT_PAGE: &str = ".front";

/// A wiki page with its text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WikiPage {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub locked_for_user: Option<bool>,
    #[serde(default)]
    pub lock_explanation: Option<String>,
}

impl WikiPage {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or("Untitled page")
    }
}

fn check(id: &str) -> Result<(), CanvasError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(CanvasError::Invalid("invalid course ID"))
    }
}

fn add(total: &mut FetchStats, s: &FetchStats) {
    total.requests += s.requests;
    total.bytes += s.bytes;
    total.network_ms += s.network_ms;
    total.parse_ms += s.parse_ms;
}

/// The course's visible tabs, in Canvas's order.
pub async fn tabs(
    client: &CanvasClient,
    course: &str,
) -> Result<(Vec<Tab>, FetchStats), CanvasError> {
    check(course)?;
    let (mut list, stats): (Vec<Tab>, _) = client
        .get_all_pages(&format!("/api/v1/courses/{course}/tabs?per_page=100"), 5)
        .await?;
    list.retain(|t| t.hidden != Some(true));
    list.sort_by_key(|t| t.position.unwrap_or(i64::MAX));
    Ok((list, stats))
}

/// Every module with its items.
pub async fn list(
    client: &CanvasClient,
    course: &str,
) -> Result<(Vec<Module>, FetchStats), CanvasError> {
    check(course)?;
    let started = std::time::Instant::now();
    let (mut modules, mut stats): (Vec<Module>, _) = client
        .get_all_pages(
            &format!(
                "/api/v1/courses/{course}/modules?include[]=items&include[]=content_details&per_page=50"
            ),
            20,
        )
        .await?;
    if modules.iter().any(|m| !valid_id(&m.id)) {
        return Err(CanvasError::Invalid("invalid module ID"));
    }
    for m in modules.iter_mut().filter(|m| m.items.is_none()) {
        let (items, s): (Vec<Item>, _) = client
            .get_all_pages(
                &format!(
                    "/api/v1/courses/{course}/modules/{}/items?include[]=content_details&per_page=100",
                    m.id
                ),
                10,
            )
            .await?;
        add(&mut stats, &s);
        m.items = Some(items);
    }
    modules.sort_by_key(|m| m.position.unwrap_or(i64::MAX));
    stats.ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok((modules, stats))
}

pub async fn page(
    client: &CanvasClient,
    course: &str,
    url: &str,
) -> Result<(WikiPage, FetchStats), CanvasError> {
    check(course)?;
    let path = if url == FRONT_PAGE {
        format!("/api/v1/courses/{course}/front_page")
    } else if valid_slug(url) {
        format!("/api/v1/courses/{course}/pages/{url}")
    } else {
        return Err(CanvasError::Invalid("invalid page name"));
    };
    let (page, stats): (WikiPage, _) = client.get_json(&path).await?;
    // Canvas may answer with the page under its current name after a rename.
    if page.url.as_deref().is_some_and(|u| !valid_slug(u)) {
        return Err(CanvasError::Invalid("unexpected page"));
    }
    Ok((page, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::tests::{FakeTransport, client, json};

    #[tokio::test]
    async fn tabs_drop_hidden_and_follow_canvas_order() {
        let t = FakeTransport::new(vec![json(
            200,
            r#"[{"id":"files","position":5},{"id":"home","position":1},{"id":"modules","position":3,"hidden":true},{"id":"announcements","position":2}]"#,
        )]);
        let (tabs, _) = tabs(&client(t), "10").await.unwrap();
        let ids: Vec<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["home", "announcements", "files"]);
    }

    #[tokio::test]
    async fn modules_read_missing_items_separately_and_sort() {
        let t = FakeTransport::new(vec![
            json(
                200,
                r#"[{"id":2,"name":"Two","position":2,"items":[{"id":5,"type":"Page","title":"Intro","page_url":"intro","completion_requirement":{"type":"must_view","completed":true}}]},
                    {"id":1,"name":"One","position":1}]"#,
            ),
            json(
                200,
                r#"[{"id":7,"type":"File","title":"slides.pdf","content_id":9,"indent":1}]"#,
            ),
        ]);
        let (mods, stats) = list(&client(t.clone()), "10").await.unwrap();
        assert_eq!(mods[0].name(), "One");
        assert_eq!(mods[0].items()[0].content(), Some("9"));
        assert_eq!(mods[0].items()[0].kind(), ItemKind::File);
        assert_eq!(mods[1].items()[0].requirement(), Some(("view", true)));
        assert_eq!(mods[1].items()[0].page(), Some("intro"));
        assert_eq!(stats.requests, 2);
        assert!(
            t.requests.lock().unwrap()[1]
                .url
                .contains("/modules/1/items")
        );
    }

    #[tokio::test]
    async fn pages_need_safe_names() {
        let t = FakeTransport::new(vec![]);
        assert!(page(&client(t.clone()), "10", "../x").await.is_err());
        assert!(page(&client(t.clone()), "10", "a/b").await.is_err());
        assert!(t.requests.lock().unwrap().is_empty());
        let t = FakeTransport::new(vec![json(
            200,
            r#"{"url":"week-1","title":"Week 1","body":"<p>Hi</p>"}"#,
        )]);
        let (p, _) = page(&client(t), "10", "week-1").await.unwrap();
        let t = FakeTransport::new(vec![json(200, r#"{"url":"welcome","title":"Welcome"}"#)]);
        let (p2, _) = page(&client(t.clone()), "10", FRONT_PAGE).await.unwrap();
        assert_eq!(p2.title(), "Welcome");
        assert!(
            t.requests.lock().unwrap()[0]
                .url
                .ends_with("/courses/10/front_page")
        );
        assert_eq!(p.title(), "Week 1");
        assert!(valid_slug("week-1_notes.v2"));
        assert!(!valid_slug(".hidden"));
        assert!(!valid_slug("caf%C3%A9"));
    }

    /// Content-free live validation of the exact Rust readers, using the saved
    /// session: counts and timings only.
    #[tokio::test]
    #[ignore = "requires a valid saved Canvas session and network access"]
    async fn live_modules() {
        use std::sync::Arc;
        let config = crate::config::Config::load().unwrap();
        let session = crate::session::SessionFile::load(&config.session_path(), &config.origin)
            .unwrap()
            .expect("sign in first");
        let client = CanvasClient::new(
            &config.origin,
            &config.host,
            Arc::new(crate::transport::ReqwestTransport::new().unwrap()),
            session.jar,
            Arc::new(crate::diag::Diagnostics::new(None)),
        );
        let (courses, _) = crate::api::courses(&client).await.unwrap();
        let mut report = Vec::new();
        let mut pages_read = 0;
        for course in courses {
            let (tabs, tab_stats) = tabs(&client, &course.id).await.unwrap();
            let (mods, stats) = list(&client, &course.id).await.unwrap();
            let items: Vec<&Item> = mods.iter().flat_map(|m| m.items()).collect();
            let mut kinds = std::collections::BTreeMap::new();
            for i in &items {
                *kinds.entry(format!("{:?}", i.kind())).or_insert(0) += 1;
            }
            let page_stats = match items.iter().find_map(|i| i.page()) {
                Some(url) if pages_read < 5 => {
                    pages_read += 1;
                    let (p, s) = page(&client, &course.id, url).await.unwrap();
                    assert!(p.title.is_some());
                    Some(s)
                }
                _ => None,
            };
            let front = if course.default_view.as_deref() == Some("wiki") {
                let read = page(&client, &course.id, FRONT_PAGE).await;
                Some(match read {
                    Ok((p, s)) => {
                        serde_json::json!({"ok": true, "links": crate::html::links(p.body.as_deref().unwrap_or(""), &config.origin).len(), "ms": s.ms})
                    }
                    Err(e) => serde_json::json!({"ok": false, "error": e.describe()}),
                })
            } else {
                None
            };
            report.push(serde_json::json!({
                "tabs": tabs.iter().map(|t| t.id.as_str()).filter(|id| !id.starts_with("context_external_tool")).collect::<Vec<_>>(),
                "default_view": course.default_view,
                "modules": mods.len(), "items": items.len(), "kinds": kinds,
                "unsafe_page_names": items.iter().filter(|i| i.kind() == ItemKind::Page && i.page().is_none()).count(),
                "tabs_read": tab_stats, "modules_read": stats, "page_read": page_stats, "front_page": front,
            }));
        }
        let path = config.private_reports().join("modules-live.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        for r in &report {
            println!(
                "{}",
                serde_json::json!({"tabs": r["tabs"], "home": r["default_view"], "modules": r["modules"], "items": r["items"], "kinds": r["kinds"], "unsafe": r["unsafe_page_names"], "front": r["front_page"]})
            );
        }
        println!(
            "Validated {} courses; saved counts/timings only.",
            report.len()
        );
    }
}
