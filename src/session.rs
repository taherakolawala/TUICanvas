//! Session state shared with the Python login helper, plus a small cookie jar.
//!
//! The file format is the helper's DPAPI-protected JSON: `{version, origin,
//! user_id, saved_at, browser_state}` where `browser_state` is a Playwright
//! storage state. Unknown fields are preserved so the helper can reuse the
//! state (including remembered-device cookies) for the next browser login.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError(pub String);

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn root_path() -> String {
    "/".into()
}
fn session_expiry() -> f64 {
    -1.0
}
fn yes() -> bool {
    true
}
fn lax() -> String {
    "Lax".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    /// Leading dot = domain cookie; otherwise host-only (Playwright convention).
    pub domain: String,
    #[serde(default = "root_path")]
    pub path: String,
    /// Seconds since the Unix epoch; negative means a session cookie.
    #[serde(default = "session_expiry")]
    pub expires: f64,
    #[serde(rename = "httpOnly", default)]
    pub http_only: bool,
    #[serde(default = "yes")]
    pub secure: bool,
    #[serde(rename = "sameSite", default = "lax")]
    pub same_site: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl StoredCookie {
    fn host_only(&self) -> bool {
        !self.domain.starts_with('.')
    }

    fn bare_domain(&self) -> &str {
        self.domain.trim_start_matches('.')
    }

    fn domain_matches(&self, host: &str) -> bool {
        let domain = self.bare_domain();
        if domain.is_empty() {
            return false;
        }
        if self.host_only() {
            host.eq_ignore_ascii_case(domain)
        } else {
            host.eq_ignore_ascii_case(domain)
                || (host.len() > domain.len()
                    && host
                        .to_ascii_lowercase()
                        .ends_with(&format!(".{}", domain.to_ascii_lowercase())))
        }
    }

    fn expired(&self, now: f64) -> bool {
        self.expires >= 0.0 && self.expires <= now
    }

    fn applies(&self, host: &str, path: &str, now: f64) -> bool {
        self.domain_matches(host) && path_matches(path, &self.path) && !self.expired(now)
    }
}

/// RFC 6265 section 5.1.4 path matching.
fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || (request.starts_with(cookie)
            && (cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/')))
}

fn default_path(request_path: &str) -> String {
    match request_path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => request_path[..i].to_string(),
    }
}

pub fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CookieJar {
    pub cookies: Vec<StoredCookie>,
}

impl CookieJar {
    /// The Cookie header for an HTTPS request, most specific paths first.
    pub fn header_for(&self, host: &str, path: &str, now: f64) -> Option<String> {
        let mut matching: Vec<&StoredCookie> = self
            .cookies
            .iter()
            .filter(|c| c.applies(host, path, now))
            .collect();
        if matching.is_empty() {
            return None;
        }
        matching.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        let header = matching
            .iter()
            // Values with separators or control characters cannot be sent safely.
            .filter(|c| valid_token(&c.name) && valid_value(&c.value))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        (!header.is_empty()).then_some(header)
    }

    /// Canvas mirrors its CSRF token in the `_csrf_token` cookie (URL-encoded).
    pub fn csrf_token(&self, host: &str, now: f64) -> Option<String> {
        let cookie = self
            .cookies
            .iter()
            .filter(|c| c.name == "_csrf_token" && c.applies(host, "/", now))
            .max_by_key(|c| c.path.len())?;
        let token = percent_decode(&cookie.value)?;
        (!token.is_empty() && !token.chars().any(|c| c.is_control())).then_some(token)
    }

    pub fn count_for(&self, host: &str, now: f64) -> usize {
        self.cookies
            .iter()
            .filter(|c| c.applies(host, "/", now))
            .count()
    }

    /// Apply one Set-Cookie header received from `host`. Returns whether the jar changed.
    pub fn apply_set_cookie(
        &mut self,
        host: &str,
        request_path: &str,
        header: &str,
        now: f64,
    ) -> bool {
        let Ok(parsed) = cookie::Cookie::parse(header.to_string()) else {
            return false;
        };
        if !valid_token(parsed.name()) || !valid_value(parsed.value()) {
            return false;
        }
        let domain = match parsed.domain() {
            Some(raw) => {
                let d = raw.trim_start_matches('.').to_ascii_lowercase();
                let host_l = host.to_ascii_lowercase();
                let matches = host_l == d || host_l.ends_with(&format!(".{d}"));
                // Reject cross-site and single-label (suffix-like) domains.
                if !matches || !d.contains('.') {
                    return false;
                }
                format!(".{d}")
            }
            None => host.to_ascii_lowercase(),
        };
        let path = parsed
            .path()
            .filter(|p| p.starts_with('/'))
            .map(str::to_string)
            .unwrap_or_else(|| default_path(request_path));
        let expires = if let Some(age) = parsed.max_age() {
            let secs = age.whole_seconds();
            if secs <= 0 { 0.0 } else { now + secs as f64 }
        } else if let Some(at) = parsed.expires_datetime() {
            (at.unix_timestamp() as f64).max(0.0)
        } else {
            -1.0
        };
        let key_matches = |c: &StoredCookie| {
            c.name == parsed.name() && c.domain.eq_ignore_ascii_case(&domain) && c.path == path
        };
        let previous = self.cookies.iter().position(key_matches);
        if expires >= 0.0 && expires <= now {
            return previous.map(|i| self.cookies.remove(i)).is_some();
        }
        let same_site = match parsed.same_site() {
            Some(cookie::SameSite::Strict) => "Strict",
            Some(cookie::SameSite::None) => "None",
            _ => "Lax",
        }
        .to_string();
        let mut stored = StoredCookie {
            name: parsed.name().to_string(),
            value: parsed.value().to_string(),
            domain,
            path,
            expires,
            http_only: parsed.http_only().unwrap_or(false),
            secure: parsed.secure().unwrap_or(false),
            same_site,
            extra: Map::new(),
        };
        match previous {
            Some(i) => {
                stored.extra = self.cookies[i].extra.clone();
                let changed = self.cookies[i] != stored;
                self.cookies[i] = stored;
                changed
            }
            None => {
                self.cookies.push(stored);
                true
            }
        }
    }
}

fn valid_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b))
}

fn valid_value(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_graphic() && b != b';' && b != b',' && b != b'\\' && b != b'"')
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The decrypted session payload.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionFile {
    pub origin: String,
    pub user_id: Option<u64>,
    pub saved_at: f64,
    pub jar: CookieJar,
    /// Remaining Playwright storage-state fields (e.g. `origins`), preserved verbatim.
    browser_rest: Map<String, Value>,
    /// Remaining top-level fields, preserved verbatim.
    top_rest: Map<String, Value>,
}

impl SessionFile {
    pub fn parse(bytes: &[u8], expected_origin: &str) -> Result<Self, SessionError> {
        let invalid = || SessionError("Saved session is invalid; sign in again.".into());
        let mut top: Map<String, Value> = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if top.remove("version") != Some(Value::from(1)) {
            return Err(invalid());
        }
        let origin = match top.remove("origin") {
            Some(Value::String(o)) if o == expected_origin => o,
            _ => {
                return Err(SessionError(
                    "Saved session belongs to a different Canvas origin.".into(),
                ));
            }
        };
        let user_id = match top.remove("user_id") {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        };
        let saved_at = top
            .remove("saved_at")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let Some(Value::Object(mut browser)) = top.remove("browser_state") else {
            return Err(invalid());
        };
        let cookies = match browser.remove("cookies") {
            Some(v @ Value::Array(_)) => serde_json::from_value(v).map_err(|_| invalid())?,
            _ => return Err(invalid()),
        };
        Ok(SessionFile {
            origin,
            user_id,
            saved_at,
            jar: CookieJar { cookies },
            browser_rest: browser,
            top_rest: top,
        })
    }

    pub fn to_json(&self) -> Vec<u8> {
        let mut browser = self.browser_rest.clone();
        browser.insert(
            "cookies".into(),
            serde_json::to_value(&self.jar.cookies).unwrap_or(Value::Array(vec![])),
        );
        let mut top = self.top_rest.clone();
        top.insert("version".into(), Value::from(1));
        top.insert("origin".into(), Value::from(self.origin.clone()));
        top.insert(
            "user_id".into(),
            self.user_id.map(Value::from).unwrap_or(Value::Null),
        );
        top.insert("saved_at".into(), Value::from(self.saved_at));
        top.insert("browser_state".into(), Value::Object(browser));
        serde_json::to_vec(&Value::Object(top)).unwrap_or_default()
    }

    pub fn load(path: &Path, origin: &str) -> Result<Option<Self>, SessionError> {
        let sealed = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(SessionError("Saved session could not be read.".into())),
        };
        let plain = protect::unprotect(&sealed).map_err(SessionError)?;
        Self::parse(&plain, origin).map(Some)
    }

    pub fn save(&mut self, path: &Path) -> Result<(), SessionError> {
        self.saved_at = now_seconds();
        let sealed = protect::protect(&self.to_json()).map_err(SessionError)?;
        let fail = || SessionError("Session could not be saved.".into());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| fail())?;
        }
        let tmp = path.with_extension("dpapi.tmp");
        std::fs::write(&tmp, sealed).map_err(|_| fail())?;
        std::fs::rename(&tmp, path).map_err(|_| fail())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "ufl.instructure.com";

    fn cookie(name: &str, value: &str, domain: &str, path: &str, expires: f64) -> StoredCookie {
        StoredCookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: path.into(),
            expires,
            http_only: true,
            secure: true,
            same_site: "Lax".into(),
            extra: Map::new(),
        }
    }

    #[test]
    fn header_scopes_domain_path_and_expiry() {
        let jar = CookieJar {
            cookies: vec![
                cookie("canvas_session", "a", HOST, "/", -1.0),
                cookie("parent", "b", ".instructure.com", "/", -1.0),
                cookie("idp", "secret", "login.ufl.edu", "/", -1.0),
                cookie("lookalike", "x", "evilufl.instructure.com", "/", -1.0),
                cookie("old", "c", HOST, "/", 10.0),
                cookie("deep", "d", HOST, "/api/v1", -1.0),
                cookie("other_path", "e", HOST, "/api/v1x", -1.0),
            ],
        };
        let header = jar.header_for(HOST, "/api/v1/courses", 100.0).unwrap();
        assert_eq!(header, "deep=d; canvas_session=a; parent=b");
        assert_eq!(
            jar.header_for("login.ufl.edu", "/", 100.0).unwrap(),
            "idp=secret"
        );
        assert!(jar.header_for("example.com", "/", 100.0).is_none());
    }

    #[test]
    fn host_only_cookie_does_not_reach_subdomain() {
        let jar = CookieJar {
            cookies: vec![cookie("s", "1", "instructure.com", "/", -1.0)],
        };
        assert!(jar.header_for(HOST, "/", 0.0).is_none());
    }

    #[test]
    fn csrf_token_is_decoded_and_scoped() {
        let jar = CookieJar {
            cookies: vec![
                cookie("_csrf_token", "ab%2Bc%3D%3D", HOST, "/", -1.0),
                cookie("_csrf_token", "wrong", "elsewhere.edu", "/", -1.0),
            ],
        };
        assert_eq!(jar.csrf_token(HOST, 0.0).unwrap(), "ab+c==");
        let bad = CookieJar {
            cookies: vec![cookie("_csrf_token", "a%0Db", HOST, "/", -1.0)],
        };
        assert!(bad.csrf_token(HOST, 0.0).is_none());
    }

    #[test]
    fn set_cookie_rotates_adds_and_deletes() {
        let mut jar = CookieJar {
            cookies: vec![cookie("canvas_session", "old", HOST, "/", -1.0)],
        };
        jar.cookies[0]
            .extra
            .insert("partitionKey".into(), Value::from("k"));
        assert!(jar.apply_set_cookie(
            HOST,
            "/api/v1/x",
            "canvas_session=new; path=/; secure; HttpOnly",
            100.0
        ));
        assert_eq!(jar.cookies.len(), 1);
        assert_eq!(jar.cookies[0].value, "new");
        assert_eq!(
            jar.cookies[0].extra.get("partitionKey"),
            Some(&Value::from("k"))
        );
        assert!(!jar.apply_set_cookie(
            HOST,
            "/api/v1/x",
            "canvas_session=new; path=/; secure; HttpOnly",
            100.0
        ));

        assert!(jar.apply_set_cookie(HOST, "/api/v1/x", "temp=1; Max-Age=60", 100.0));
        let temp = jar.cookies.iter().find(|c| c.name == "temp").unwrap();
        assert_eq!(temp.path, "/api/v1");
        assert_eq!(temp.expires, 160.0);

        assert!(jar.apply_set_cookie(HOST, "/", "canvas_session=; path=/; Max-Age=0", 100.0));
        assert!(jar.cookies.iter().all(|c| c.name != "canvas_session"));
    }

    #[test]
    fn set_cookie_rejects_foreign_domains() {
        let mut jar = CookieJar::default();
        assert!(!jar.apply_set_cookie(HOST, "/", "x=1; Domain=ufl.edu", 0.0));
        assert!(!jar.apply_set_cookie(HOST, "/", "x=1; Domain=com", 0.0));
        assert!(!jar.apply_set_cookie(HOST, "/", "x=1\u{7}; path=/", 0.0));
        assert!(jar.apply_set_cookie(HOST, "/", "x=1; Domain=.instructure.com", 0.0));
        assert_eq!(jar.cookies[0].domain, ".instructure.com");
    }

    #[test]
    fn session_file_round_trip_preserves_unknown_fields() {
        let raw = serde_json::json!({
            "version": 1, "origin": "https://ufl.instructure.com", "user_id": 42, "saved_at": 1.5,
            "extra_top": true,
            "browser_state": {
                "cookies": [{"name": "canvas_session", "value": "v", "domain": HOST, "path": "/",
                             "expires": -1, "httpOnly": true, "secure": true, "sameSite": "Lax",
                             "partitionKey": "p"}],
                "origins": [{"origin": "https://ufl.instructure.com", "localStorage": []}]
            }
        });
        let bytes = serde_json::to_vec(&raw).unwrap();
        let session = SessionFile::parse(&bytes, "https://ufl.instructure.com").unwrap();
        assert_eq!(session.user_id, Some(42));
        assert_eq!(session.jar.cookies.len(), 1);
        let back: Value = serde_json::from_slice(&session.to_json()).unwrap();
        assert_eq!(back["extra_top"], Value::from(true));
        assert_eq!(
            back["browser_state"]["origins"],
            raw["browser_state"]["origins"]
        );
        assert_eq!(
            back["browser_state"]["cookies"][0]["partitionKey"],
            Value::from("p")
        );
        assert_eq!(
            back["browser_state"]["cookies"][0]["expires"],
            Value::from(-1.0)
        );
        assert!(SessionFile::parse(&bytes, "https://other.instructure.com").is_err());
        assert!(SessionFile::parse(b"{\"version\":2}", "https://ufl.instructure.com").is_err());
    }
}
