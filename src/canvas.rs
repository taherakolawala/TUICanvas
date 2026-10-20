//! Authenticated read-only Canvas client: cookies, CSRF, pagination, and
//! classification of authentication versus access failures.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::diag::{self, Diagnostics, Exchange};
use crate::session::{self, CookieJar};
use crate::transport::{HttpRequest, HttpResponse, Method, NetError, Transport};

#[derive(Clone, Debug, PartialEq)]
pub enum CanvasError {
    /// Signed out or session expired: the user must sign in again.
    Unauthenticated,
    /// Canvas rejected the request's CSRF token; also resolved by signing in again.
    Csrf,
    /// Signed in, but this account cannot see the resource.
    NoAccess(u16),
    NotFound,
    Throttled,
    Server(u16),
    Http(u16),
    Network(NetError),
    Invalid(&'static str),
    GraphQl(Vec<String>),
}

impl CanvasError {
    pub fn needs_login(&self) -> bool {
        matches!(self, CanvasError::Unauthenticated | CanvasError::Csrf)
    }

    pub fn describe(&self) -> String {
        match self {
            CanvasError::Unauthenticated => "signed out or session expired".into(),
            CanvasError::Csrf => "Canvas rejected the session token".into(),
            CanvasError::NoAccess(s) => format!("no access to this item (HTTP {s})"),
            CanvasError::NotFound => "not found".into(),
            CanvasError::Throttled => "Canvas is rate limiting requests".into(),
            CanvasError::Server(s) => format!("Canvas server error (HTTP {s})"),
            CanvasError::Http(s) => format!("unexpected response (HTTP {s})"),
            CanvasError::Network(e) => e.describe().into(),
            CanvasError::Invalid(why) => format!("unexpected response: {why}"),
            CanvasError::GraphQl(msgs) => format!(
                "GraphQL error: {}",
                msgs.first().map(String::as_str).unwrap_or("unknown")
            ),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            CanvasError::Unauthenticated => "unauthenticated",
            CanvasError::Csrf => "csrf_rejected",
            CanvasError::NoAccess(_) => "no_access",
            CanvasError::NotFound => "not_found",
            CanvasError::Throttled => "throttled",
            CanvasError::Server(_) => "server_error",
            CanvasError::Http(_) => "http_error",
            CanvasError::Network(_) => "network_error",
            CanvasError::Invalid(_) => "invalid_response",
            CanvasError::GraphQl(_) => "graphql_error",
        }
    }
}

/// Aggregate cost of one logical read (possibly several pages).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct FetchStats {
    pub requests: u32,
    pub bytes: usize,
    /// Wall time from the first request start to the last response end.
    pub ms: f64,
}

pub struct CanvasClient {
    origin: String,
    host: String,
    transport: Arc<dyn Transport>,
    jar: Mutex<CookieJar>,
    jar_changed: AtomicBool,
    pub diag: Arc<Diagnostics>,
}

const MAX_THROTTLE_RETRIES: u32 = 2;

impl CanvasClient {
    pub fn new(
        origin: &str,
        host: &str,
        transport: Arc<dyn Transport>,
        jar: CookieJar,
        diag: Arc<Diagnostics>,
    ) -> Self {
        CanvasClient {
            origin: origin.to_string(),
            host: host.to_string(),
            transport,
            jar: Mutex::new(jar),
            jar_changed: AtomicBool::new(false),
            diag,
        }
    }

    pub fn replace_jar(&self, jar: CookieJar) {
        *self.jar.lock().unwrap_or_else(|e| e.into_inner()) = jar;
        self.jar_changed.store(false, Ordering::SeqCst);
    }

    /// The jar if Canvas rotated or added cookies since the last call.
    pub fn take_changed_jar(&self) -> Option<CookieJar> {
        if self.jar_changed.swap(false, Ordering::SeqCst) {
            Some(self.jar.lock().unwrap_or_else(|e| e.into_inner()).clone())
        } else {
            None
        }
    }

    fn check_path(path: &str) -> Result<(), CanvasError> {
        let ok = (path.starts_with("/api/v1/") || path == "/api/graphql")
            && !path.contains('\\')
            && !path.contains("..")
            && !path.contains('#')
            && path.chars().all(|c| c.is_ascii_graphic());
        if ok {
            Ok(())
        } else {
            Err(CanvasError::Invalid("refused request path"))
        }
    }

    async fn exchange(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        send_csrf: bool,
    ) -> Result<HttpResponse, CanvasError> {
        Self::check_path(path)?;
        let mut attempt = 0;
        loop {
            let now = session::now_seconds();
            let (cookie, csrf) = {
                let jar = self.jar.lock().unwrap_or_else(|e| e.into_inner());
                let request_path = path.split('?').next().unwrap_or(path);
                (
                    jar.header_for(&self.host, request_path, now),
                    if send_csrf {
                        jar.csrf_token(&self.host, now)
                    } else {
                        None
                    },
                )
            };
            let mut headers = vec![
                (
                    "accept",
                    "application/json+canvas-string-ids, application/json".to_string(),
                ),
                ("x-requested-with", "XMLHttpRequest".to_string()),
            ];
            if let Some(cookie) = cookie {
                headers.push(("cookie", cookie));
            }
            if let Some(token) = csrf {
                headers.push(("x-csrf-token", token));
            }
            if body.is_some() {
                headers.push(("content-type", "application/json".to_string()));
            }
            let request = HttpRequest {
                method,
                url: format!("{}{}", self.origin, path),
                headers,
                body: body.clone(),
            };
            let started_ms = diag::epoch_ms();
            let result = self.transport.send(request).await;
            let method_name = if method == Method::Get { "GET" } else { "POST" };
            let response = match result {
                Ok(r) => r,
                Err(e) => {
                    self.diag.record(Exchange {
                        started_ms,
                        method: method_name,
                        template: diag::template(path),
                        status: 0,
                        bytes: 0,
                        ms: 0.0,
                        rate_limit_remaining: None,
                        request_cost: None,
                        outcome: "network_error",
                        error_hint: None,
                    });
                    return Err(CanvasError::Network(e));
                }
            };
            self.absorb_cookies(path, &response);
            let verdict = match classify(&response) {
                // Canvas answers a missing/invalid CSRF token on a POST with a bare
                // 422 "An error occurred."; our only POSTs are read-only GraphQL.
                Err(CanvasError::Http(422)) if method == Method::Post => Err(CanvasError::Csrf),
                other => other,
            };
            self.diag.record(Exchange {
                started_ms,
                method: method_name,
                template: diag::template(path),
                status: response.status,
                bytes: response.body.len(),
                ms: response.ms,
                rate_limit_remaining: response
                    .header("x-rate-limit-remaining")
                    .and_then(|v| v.parse().ok()),
                request_cost: response
                    .header("x-request-cost")
                    .and_then(|v| v.parse().ok()),
                outcome: verdict
                    .as_ref()
                    .err()
                    .map(CanvasError::kind)
                    .unwrap_or("ok"),
                error_hint: verdict.as_ref().err().and_then(|_| error_hint(&response)),
            });
            match verdict {
                Err(CanvasError::Throttled) if attempt < MAX_THROTTLE_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(1000 * u64::from(attempt))).await;
                }
                Err(e) => return Err(e),
                Ok(()) => return Ok(response),
            }
        }
    }

    fn absorb_cookies(&self, path: &str, response: &HttpResponse) {
        let mut changed = false;
        let now = session::now_seconds();
        let request_path = path.split('?').next().unwrap_or(path);
        let mut jar = self.jar.lock().unwrap_or_else(|e| e.into_inner());
        for header in response.headers_named("set-cookie") {
            changed |= jar.apply_set_cookie(&self.host, request_path, header, now);
        }
        if changed {
            self.jar_changed.store(true, Ordering::SeqCst);
        }
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<(T, FetchStats), CanvasError> {
        let started = Instant::now();
        let response = self.exchange(Method::Get, path, None, false).await?;
        let value = parse_json(&response.body)?;
        Ok((
            value,
            FetchStats {
                requests: 1,
                bytes: response.body.len(),
                ms: elapsed_ms(started),
            },
        ))
    }

    /// Follow `Link: rel="next"` until exhausted. A missing page is an error,
    /// never a silently partial list.
    pub async fn get_all_pages<T: DeserializeOwned>(
        &self,
        path: &str,
        max_pages: u32,
    ) -> Result<(Vec<T>, FetchStats), CanvasError> {
        let started = Instant::now();
        let mut stats = FetchStats::default();
        let mut items = Vec::new();
        let mut next = Some(path.to_string());
        while let Some(current) = next.take() {
            if stats.requests >= max_pages {
                return Err(CanvasError::Invalid("too many pages"));
            }
            let response = self.exchange(Method::Get, &current, None, false).await?;
            stats.requests += 1;
            stats.bytes += response.body.len();
            let page: Vec<T> = parse_json(&response.body)?;
            items.extend(page);
            if let Some(link) = response.header("link") {
                next = next_link(link, &self.origin)?;
            }
        }
        stats.ms = elapsed_ms(started);
        Ok((items, stats))
    }

    /// Read-only GraphQL query. Any `errors` entry fails the read so a partial
    /// response never replaces good data.
    pub async fn graphql(
        &self,
        query: &str,
        variables: Value,
    ) -> Result<(Value, FetchStats), CanvasError> {
        self.graphql_with(query, variables, true).await
    }

    pub async fn graphql_with(
        &self,
        query: &str,
        variables: Value,
        send_csrf: bool,
    ) -> Result<(Value, FetchStats), CanvasError> {
        let started = Instant::now();
        let body = serde_json::to_vec(&serde_json::json!({"query": query, "variables": variables}))
            .map_err(|_| CanvasError::Invalid("could not encode query"))?;
        let response = self
            .exchange(Method::Post, "/api/graphql", Some(body), send_csrf)
            .await?;
        let mut value: Value = parse_json(&response.body)?;
        let stats = FetchStats {
            requests: 1,
            bytes: response.body.len(),
            ms: elapsed_ms(started),
        };
        if let Some(errors) = value
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            let messages = errors
                .iter()
                .map(|e| {
                    clip(
                        e.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error"),
                        200,
                    )
                })
                .collect();
            return Err(CanvasError::GraphQl(messages));
        }
        match value.get_mut("data").map(Value::take) {
            Some(data @ Value::Object(_)) => Ok((data, stats)),
            _ => Err(CanvasError::Invalid("GraphQL response without data")),
        }
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn clip(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    match cleaned.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &cleaned[..i]),
        None => cleaned,
    }
}

pub fn parse_json<T: DeserializeOwned>(body: &[u8]) -> Result<T, CanvasError> {
    // Canvas may prefix JSON with an anti-hijacking guard.
    let body = body.strip_prefix(b"while(1);").unwrap_or(body);
    serde_json::from_slice(body).map_err(|_| CanvasError::Invalid("malformed JSON"))
}

/// Map an HTTP response to success or a failure class without reading
/// anything but status, a few headers, and Canvas's JSON error envelope.
/// Canvas's error message from a JSON error envelope, safe to log: control
/// characters removed, URLs cut, at most 120 characters.
pub fn error_hint(response: &HttpResponse) -> Option<String> {
    let envelope: Value = parse_json(&response.body).ok()?;
    let message = envelope
        .get("errors")
        .and_then(|e| e.get(0).or(Some(e)))
        .and_then(|e| e.get("message").or(Some(e)))
        .and_then(Value::as_str)
        .or_else(|| envelope.get("message").and_then(Value::as_str))
        .or_else(|| envelope.get("status").and_then(Value::as_str))?;
    let cut = message.split("http").next().unwrap_or("");
    Some(clip(cut.trim(), 120))
}

pub fn classify(response: &HttpResponse) -> Result<(), CanvasError> {
    let status = response.status;
    if (200..300).contains(&status) {
        let json = response
            .header("content-type")
            .is_some_and(|t| t.to_ascii_lowercase().contains("json"));
        return if json {
            Ok(())
        } else {
            Err(CanvasError::Invalid("non-JSON response"))
        };
    }
    let envelope: Option<Value> = parse_json(&response.body).ok();
    let field = |name: &str| {
        envelope
            .as_ref()
            .and_then(|v| v.get(name))
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    let first_message = envelope
        .as_ref()
        .and_then(|v| v.get("errors"))
        .and_then(|e| e.get(0).or(Some(e)))
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let body_text = String::from_utf8_lossy(&response.body[..response.body.len().min(512)])
        .to_ascii_lowercase();
    match status {
        300..=399 => {
            let to_login = response
                .header("location")
                .is_some_and(|l| l.contains("/login"));
            Err(if to_login {
                CanvasError::Unauthenticated
            } else {
                CanvasError::Http(status)
            })
        }
        401 => {
            let unauthorized = field("status").as_deref() == Some("unauthorized")
                || first_message.contains("not authorized");
            Err(if unauthorized {
                CanvasError::NoAccess(401)
            } else {
                CanvasError::Unauthenticated
            })
        }
        403 if body_text.contains("rate limit") => Err(CanvasError::Throttled),
        403 => Err(CanvasError::NoAccess(403)),
        404 => Err(CanvasError::NotFound),
        422 if body_text.contains("authenticity")
            || body_text.contains("csrf")
            || first_message.contains("token") =>
        {
            Err(CanvasError::Csrf)
        }
        429 => Err(CanvasError::Throttled),
        500..=599 => Err(CanvasError::Server(status)),
        _ => Err(CanvasError::Http(status)),
    }
}

/// Extract the `rel="next"` target and reduce it to a same-origin API path.
pub fn next_link(header: &str, origin: &str) -> Result<Option<String>, CanvasError> {
    for part in header.split(",<").map(|p| p.trim().trim_start_matches('<')) {
        let Some((url, params)) = part.split_once('>') else {
            continue;
        };
        let is_next = params.split(';').any(|p| {
            let p = p.trim().to_ascii_lowercase();
            p == "rel=\"next\"" || p == "rel=next"
        });
        if is_next {
            let path = url
                .strip_prefix(origin)
                .filter(|p| p.starts_with("/api/v1/"))
                .ok_or(CanvasError::Invalid("pagination left the Canvas API"))?;
            return Ok(Some(path.to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::session::StoredCookie;
    use crate::transport::BoxFuture;
    use std::collections::VecDeque;

    /// Scripted transport: returns queued responses and records requests.
    pub struct FakeTransport {
        pub responses: Mutex<VecDeque<Result<HttpResponse, NetError>>>,
        pub requests: Mutex<Vec<HttpRequest>>,
    }

    impl FakeTransport {
        pub fn new(responses: Vec<Result<HttpResponse, NetError>>) -> Arc<Self> {
            Arc::new(FakeTransport {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(vec![]),
            })
        }
    }

    impl Transport for FakeTransport {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, NetError>> {
            self.requests.lock().unwrap().push(request);
            let next = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(NetError::Other));
            Box::pin(async move { next })
        }
    }

    pub fn json(status: u16, body: &str) -> Result<HttpResponse, NetError> {
        Ok(HttpResponse {
            status,
            headers: vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            )],
            body: body.as_bytes().to_vec(),
            ms: 1.0,
        })
    }

    pub const ORIGIN: &str = "https://ufl.instructure.com";
    pub const HOST: &str = "ufl.instructure.com";

    pub fn jar() -> CookieJar {
        let mk = |name: &str, value: &str, domain: &str| StoredCookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: "/".into(),
            expires: -1.0,
            http_only: true,
            secure: true,
            same_site: "Lax".into(),
            extra: Default::default(),
        };
        CookieJar {
            cookies: vec![
                mk("canvas_session", "s1", HOST),
                mk("_csrf_token", "t%2B1", HOST),
                mk("idp", "x", "login.ufl.edu"),
            ],
        }
    }

    pub fn client(transport: Arc<FakeTransport>) -> CanvasClient {
        CanvasClient::new(
            ORIGIN,
            HOST,
            transport,
            jar(),
            Arc::new(Diagnostics::new(None)),
        )
    }

    fn header<'a>(req: &'a HttpRequest, name: &str) -> Option<&'a str> {
        req.headers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    #[tokio::test]
    async fn get_sends_canvas_cookies_only_and_no_csrf() {
        let t = FakeTransport::new(vec![json(200, "while(1);{\"id\":\"5\"}")]);
        let c = client(t.clone());
        let (v, stats): (Value, _) = c.get_json("/api/v1/users/self/profile").await.unwrap();
        assert_eq!(v["id"], "5");
        assert_eq!(stats.requests, 1);
        let reqs = t.requests.lock().unwrap();
        assert_eq!(
            reqs[0].url,
            "https://ufl.instructure.com/api/v1/users/self/profile"
        );
        assert_eq!(
            header(&reqs[0], "cookie"),
            Some("canvas_session=s1; _csrf_token=t%2B1")
        );
        assert!(header(&reqs[0], "x-csrf-token").is_none());
    }

    #[tokio::test]
    async fn graphql_sends_decoded_csrf_and_rejects_partial_errors() {
        let t = FakeTransport::new(vec![
            json(200, r#"{"data":{"a":1}}"#),
            json(
                200,
                r#"{"data":{"a":null},"errors":[{"message":"Field 'x' doesn't exist"}]}"#,
            ),
        ]);
        let c = client(t.clone());
        let (data, _) = c
            .graphql("query { a }", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(data["a"], 1);
        let err = c
            .graphql("query { x }", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            CanvasError::GraphQl(vec!["Field 'x' doesn't exist".into()])
        );
        let reqs = t.requests.lock().unwrap();
        assert_eq!(header(&reqs[0], "x-csrf-token"), Some("t+1"));
        assert_eq!(reqs[0].method, Method::Post);
    }

    #[tokio::test]
    async fn pagination_follows_next_and_refuses_foreign_links() {
        let mut first = json(200, r#"[{"id":"1"}]"#).unwrap();
        first.headers.push((
            "link".into(),
            "<https://ufl.instructure.com/api/v1/courses?page=1>; rel=\"current\",<https://ufl.instructure.com/api/v1/courses?page=2&per_page=100>; rel=\"next\"".into(),
        ));
        let t = FakeTransport::new(vec![Ok(first), json(200, r#"[{"id":"2"}]"#)]);
        let c = client(t.clone());
        let (items, stats): (Vec<Value>, _) = c
            .get_all_pages("/api/v1/courses?per_page=100", 5)
            .await
            .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(stats.requests, 2);
        assert_eq!(
            t.requests.lock().unwrap()[1].url,
            "https://ufl.instructure.com/api/v1/courses?page=2&per_page=100"
        );
        assert!(next_link("<https://evil.example/api/v1/x>; rel=\"next\"", ORIGIN).is_err());
        assert!(next_link("<https://ufl.instructure.com/login>; rel=\"next\"", ORIGIN).is_err());
    }

    #[tokio::test]
    async fn failed_page_fails_whole_list() {
        let mut first = json(200, r#"[{"id":"1"}]"#).unwrap();
        first.headers.push((
            "link".into(),
            "<https://ufl.instructure.com/api/v1/c?page=2>; rel=\"next\"".into(),
        ));
        let t = FakeTransport::new(vec![Ok(first), json(500, "{}")]);
        let c = client(t);
        let err = c.get_all_pages::<Value>("/api/v1/c", 5).await.unwrap_err();
        assert_eq!(err, CanvasError::Server(500));
    }

    #[test]
    fn classification_distinguishes_expiry_from_access() {
        let resp = |status: u16, body: &str| HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
            ..Default::default()
        };
        assert_eq!(
            classify(&resp(
                401,
                r#"{"status":"unauthenticated","errors":[{"message":"user authorization required"}]}"#
            )),
            Err(CanvasError::Unauthenticated)
        );
        assert_eq!(
            classify(&resp(
                401,
                r#"{"status":"unauthorized","errors":[{"message":"user not authorized to perform that action"}]}"#
            )),
            Err(CanvasError::NoAccess(401))
        );
        assert_eq!(classify(&resp(401, "")), Err(CanvasError::Unauthenticated));
        assert_eq!(
            classify(&resp(403, "403 Forbidden (Rate Limit Exceeded)")),
            Err(CanvasError::Throttled)
        );
        assert_eq!(classify(&resp(403, "{}")), Err(CanvasError::NoAccess(403)));
        assert_eq!(
            classify(&resp(422, "InvalidAuthenticityToken")),
            Err(CanvasError::Csrf)
        );
        let mut redirect = resp(302, "");
        redirect.headers.push((
            "location".into(),
            "https://ufl.instructure.com/login".into(),
        ));
        assert_eq!(classify(&redirect), Err(CanvasError::Unauthenticated));
        assert_eq!(
            classify(&resp(200, "<html>")),
            Err(CanvasError::Invalid("non-JSON response"))
        );
    }

    #[tokio::test]
    async fn graphql_422_is_a_csrf_rejection() {
        let t = FakeTransport::new(vec![json(
            422,
            r#"{"errors":[{"message":"An error occurred."}]}"#,
        )]);
        let c = client(t);
        let err = c
            .graphql_with("query { a }", serde_json::json!({}), false)
            .await
            .unwrap_err();
        assert_eq!(err, CanvasError::Csrf);
        assert!(err.needs_login());
    }

    #[tokio::test]
    async fn throttling_backs_off_then_succeeds() {
        let t = FakeTransport::new(vec![
            json(403, "\"403 Forbidden (Rate Limit Exceeded)\""),
            json(200, "[]"),
        ]);
        let c = client(t.clone());
        tokio::time::pause();
        let (v, _): (Value, _) = c.get_json("/api/v1/courses").await.unwrap();
        assert_eq!(v, serde_json::json!([]));
        assert_eq!(t.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rotated_cookies_are_reported_once() {
        let mut ok = json(200, "{}").unwrap();
        ok.headers.push((
            "set-cookie".into(),
            "canvas_session=s2; path=/; secure; HttpOnly".into(),
        ));
        let t = FakeTransport::new(vec![Ok(ok)]);
        let c = client(t);
        let _: (Value, _) = c.get_json("/api/v1/users/self").await.unwrap();
        let jar = c.take_changed_jar().unwrap();
        assert!(
            jar.cookies
                .iter()
                .any(|ck| ck.name == "canvas_session" && ck.value == "s2")
        );
        assert!(
            jar.cookies.iter().any(|ck| ck.name == "idp"),
            "non-Canvas cookies preserved"
        );
        assert!(c.take_changed_jar().is_none());
    }

    #[tokio::test]
    async fn refuses_paths_outside_api() {
        let t = FakeTransport::new(vec![]);
        let c = client(t.clone());
        assert!(c.get_json::<Value>("/login").await.is_err());
        assert!(c.get_json::<Value>("/api/v1/../login").await.is_err());
        assert!(t.requests.lock().unwrap().is_empty());
    }
}
