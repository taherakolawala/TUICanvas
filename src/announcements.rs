//! Announcement metadata is small; bodies are read only when opened.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::canvas::{CanvasClient, CanvasError, FetchStats};
use crate::model::{CanvasFile, valid_id};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Author {
    #[serde(alias = "shortName")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    #[serde(deserialize_with = "crate::model::de_id")]
    pub id: String,
    pub title: Option<String>,
    pub posted_at: Option<DateTime<Utc>>,
    pub delayed_post_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
    pub author: Option<Author>,
    pub read_state: Option<String>,
    pub discussion_subentry_count: Option<u64>,
}

impl Summary {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or("Untitled announcement")
    }
    pub fn author(&self) -> &str {
        self.author
            .as_ref()
            .and_then(|a| a.display_name.as_deref())
            .unwrap_or("Unknown author")
    }
    pub fn date(&self) -> Option<DateTime<Utc>> {
        self.posted_at.or(self.delayed_post_at).or(self.created_at)
    }
    pub fn status(&self) -> &'static str {
        match self.read_state.as_deref() {
            Some("read") => "Read",
            Some("unread") => "Unread",
            _ => "Unknown",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Detail {
    #[serde(flatten)]
    pub base: Summary,
    pub message: Option<String>,
    #[serde(default)]
    pub attachments: Vec<CanvasFile>,
    pub locked_for_user: Option<bool>,
    pub lock_explanation: Option<String>,
    pub lock_info: Option<LockInfo>,
    pub is_announcement: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LockInfo {
    pub can_view: Option<bool>,
}

impl Detail {
    /// Topic locks can prohibit replies while allowing the message to be read.
    /// Canvas uses lock_info.can_view, not locked_for_user, for body visibility.
    pub fn can_view(&self) -> bool {
        self.lock_info
            .as_ref()
            .is_none_or(|info| info.can_view == Some(true))
    }
}

pub const QUERY: &str = "query TuiAnnouncements($courseId: ID!, $after: String) {
  course(id: $courseId) {
    discussionsConnection(first: 100, after: $after, filter: {isAnnouncement: true}) {
      nodes { _id title postedAt delayedPostAt createdAt author { shortName }
        participant { read } entryCounts { repliesCount } }
      pageInfo { hasNextPage endCursor }
    }
  }
}";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    #[serde(rename = "_id", deserialize_with = "crate::model::de_id")]
    id: String,
    title: Option<String>,
    posted_at: Option<DateTime<Utc>>,
    delayed_post_at: Option<DateTime<Utc>>,
    created_at: Option<DateTime<Utc>>,
    author: Option<Author>,
    participant: Option<Participant>,
    entry_counts: Option<Counts>,
}
#[derive(Deserialize)]
struct Participant {
    read: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    replies_count: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    nodes: Vec<Node>,
    page_info: PageInfo,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

fn check(id: &str) -> Result<(), CanvasError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(CanvasError::Invalid("invalid announcement ID"))
    }
}

fn sort(list: &mut [Summary]) {
    list.sort_by(|a, b| {
        b.date()
            .cmp(&a.date())
            .then_with(|| b.id.len().cmp(&a.id.len()).then_with(|| b.id.cmp(&a.id)))
    });
}

pub async fn list(
    client: &CanvasClient,
    course: &str,
) -> Result<(Vec<Summary>, FetchStats), CanvasError> {
    check(course)?;
    // Only schema rejection permits a REST fallback. Network, permission and
    // pagination failures keep the saved copy instead of hiding the error.
    match graphql(client, course).await {
        Err(CanvasError::GraphQl(_)) => {
            let (mut items, stats) = client.get_all_pages::<Summary>(
                &format!("/api/v1/courses/{course}/discussion_topics?only_announcements=true&per_page=100"), 20
            ).await?;
            if items.iter().any(|a| !valid_id(&a.id)) {
                return Err(CanvasError::Invalid("invalid announcement ID"));
            }
            sort(&mut items);
            Ok((items, stats))
        }
        result => result,
    }
}

async fn graphql(
    client: &CanvasClient,
    course: &str,
) -> Result<(Vec<Summary>, FetchStats), CanvasError> {
    let started = std::time::Instant::now();
    let mut stats = FetchStats::default();
    let mut items = Vec::new();
    let mut after: Option<String> = None;
    let mut cursors = std::collections::HashSet::new();
    loop {
        if stats.requests >= 20 {
            return Err(CanvasError::Invalid("too many announcement pages"));
        }
        let (data, s) = client
            .graphql(QUERY, serde_json::json!({"courseId":course,"after":after}))
            .await?;
        stats.requests += s.requests;
        stats.bytes += s.bytes;
        stats.network_ms += s.network_ms;
        stats.parse_ms += s.parse_ms;
        let parse = std::time::Instant::now();
        let page: Page = serde_json::from_value(
            data.pointer("/course/discussionsConnection")
                .cloned()
                .ok_or(CanvasError::Invalid("announcements not returned"))?,
        )
        .map_err(|_| CanvasError::Invalid("unexpected announcements shape"))?;
        for n in page.nodes {
            check(&n.id)?;
            items.push(Summary {
                id: n.id,
                title: n.title,
                posted_at: n.posted_at,
                delayed_post_at: n.delayed_post_at,
                created_at: n.created_at,
                author: n.author,
                read_state: n
                    .participant
                    .and_then(|p| p.read)
                    .map(|r| if r { "read" } else { "unread" }.into()),
                discussion_subentry_count: n.entry_counts.and_then(|c| c.replies_count),
            });
        }
        stats.parse_ms += parse.elapsed().as_secs_f64() * 1000.0;
        if !page.page_info.has_next_page {
            break;
        }
        let cursor = page
            .page_info
            .end_cursor
            .filter(|c| !c.is_empty())
            .ok_or(CanvasError::Invalid("announcement cursor missing"))?;
        if !cursors.insert(cursor.clone()) {
            return Err(CanvasError::Invalid("repeated announcement cursor"));
        }
        after = Some(cursor);
    }
    sort(&mut items);
    let mut ids = std::collections::HashSet::new();
    items.retain(|a| ids.insert(a.id.clone()));
    stats.ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok((items, stats))
}

pub async fn detail(
    client: &CanvasClient,
    course: &str,
    id: &str,
) -> Result<(Detail, FetchStats), CanvasError> {
    check(course)?;
    check(id)?;
    let (detail, stats) = client
        .get_json::<Detail>(&format!("/api/v1/courses/{course}/discussion_topics/{id}"))
        .await?;
    if detail.base.id != id || detail.is_announcement != Some(true) {
        return Err(CanvasError::Invalid("unexpected announcement"));
    }
    Ok((detail, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::tests::{FakeTransport, client, json};
    use serde_json::{Value, json as value};

    fn page(
        nodes: Value,
        more: bool,
        cursor: Option<&str>,
    ) -> Result<crate::transport::HttpResponse, crate::transport::NetError> {
        json(
            200,
            &value!({"data":{"course":{"discussionsConnection":{
                "nodes":nodes,"pageInfo":{"hasNextPage":more,"endCursor":cursor}
            }}}})
            .to_string(),
        )
    }

    #[tokio::test]
    async fn pagination_sorts_by_posting_date_and_preserves_unknown_read_state() {
        let t = FakeTransport::new(vec![
            page(
                value!([{"_id":"1","title":"Created later","postedAt":"2026-10-01T00:00:00Z","participant":{"read":true},"author":{"shortName":"Teacher"}}, {"_id":"3"}]),
                true,
                Some("next"),
            ),
            page(
                value!([{"_id":"2","title":"Published later","delayedPostAt":"2026-10-21T00:00:00Z","participant":{"read":false},"entryCounts":{"repliesCount":2}}]),
                false,
                None,
            ),
        ]);
        let (items, stats) = list(&client(t.clone()), "10").await.unwrap();
        assert_eq!(
            items.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["2", "1", "3"]
        );
        assert_eq!(items[0].status(), "Unread");
        assert_eq!(items[1].status(), "Read");
        assert_eq!(items[1].author(), "Teacher");
        assert_eq!(items[2].status(), "Unknown");
        assert_eq!(stats.requests, 2);
        let requests = t.requests.lock().unwrap();
        let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["variables"]["after"], "next");
        assert!(!body["query"].as_str().unwrap().contains("message"));
    }

    #[tokio::test]
    async fn rejects_partial_lists_missing_and_repeated_cursors() {
        for replies in [
            vec![page(value!([]), true, None)],
            vec![
                page(value!([]), true, Some("same")),
                page(value!([]), true, Some("same")),
            ],
            vec![page(value!([]), true, Some("next")), json(500, "{}")],
        ] {
            assert!(
                list(&client(FakeTransport::new(replies)), "10")
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn schema_rejection_falls_back_but_expiry_does_not() {
        let t = FakeTransport::new(vec![
            json(200, r#"{"errors":[{"message":"Field unavailable"}]}"#),
            json(200, r#"[{"id":8,"title":"REST","read_state":"unread"}]"#),
        ]);
        let (items, _) = list(&client(t.clone()), "10").await.unwrap();
        assert_eq!(items[0].id, "8");
        assert!(
            t.requests.lock().unwrap()[1]
                .url
                .contains("only_announcements=true")
        );
        let t = FakeTransport::new(vec![json(401, "{}")]);
        assert!(
            list(&client(t.clone()), "10")
                .await
                .unwrap_err()
                .needs_login()
        );
        assert_eq!(t.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn details_validate_identity_and_keep_attachments() {
        let t = FakeTransport::new(vec![json(
            200,
            r#"{"id":8,"is_announcement":true,"message":"<p>Hello</p>","attachments":[{"id":9,"display_name":"notes.pdf"}]}"#,
        )]);
        let (d, _) = detail(&client(t), "10", "8").await.unwrap();
        assert_eq!(d.attachments[0].id, "9");
        for body in [
            r#"{"id":7,"is_announcement":true}"#,
            r#"{"id":8,"is_announcement":false}"#,
        ] {
            assert!(
                detail(
                    &client(FakeTransport::new(vec![json(200, body)])),
                    "10",
                    "8"
                )
                .await
                .is_err()
            );
        }
        let t = FakeTransport::new(vec![]);
        assert!(detail(&client(t.clone()), "../", "8").await.is_err());
        assert!(t.requests.lock().unwrap().is_empty());
    }

    /// Content-free live validation of the exact Rust reader, using the saved session.
    #[tokio::test]
    #[ignore = "requires a valid saved Canvas session and network access"]
    async fn live_announcements() {
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
        for course in courses {
            let (items, stats) = list(&client, &course.id).await.unwrap();
            assert!(items.windows(2).all(|w| w[0].date() >= w[1].date()));
            let detail_stats = if let Some(a) = items.first() {
                let (d, s) = detail(&client, &course.id, &a.id).await.unwrap();
                assert_eq!(d.base.id, a.id);
                Some(s)
            } else {
                None
            };
            report.push(value!({"count":items.len(),"list":stats,"detail":detail_stats}));
        }
        let path = config.private_reports().join("announcements-live.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        println!(
            "Validated {} course lists and their first available details; saved counts/timings only.",
            report.len()
        );
    }
}
