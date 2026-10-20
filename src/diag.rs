//! Request diagnostics: endpoint templates, status, sizes and timings only.
//! Never record URLs with identifiers, query strings, headers, or bodies.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Exchange {
    /// Milliseconds since the Unix epoch when the request started.
    pub started_ms: u128,
    pub method: &'static str,
    pub template: String,
    pub status: u16,
    pub bytes: usize,
    pub ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_remaining: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_cost: Option<f64>,
    pub outcome: &'static str,
    /// Canvas's generic error message for failed requests (sanitized, clipped, no URLs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_hint: Option<String>,
}

/// Bounded in-memory log, optionally mirrored to a JSONL file.
pub struct Diagnostics {
    recent: Mutex<Vec<Exchange>>,
    file: Option<PathBuf>,
}

const KEEP: usize = 200;

impl Diagnostics {
    pub fn new(file: Option<PathBuf>) -> Self {
        Diagnostics {
            recent: Mutex::new(Vec::new()),
            file,
        }
    }

    pub fn record(&self, exchange: Exchange) {
        if let Some(path) = &self.file
            && let Ok(line) = serde_json::to_string(&exchange)
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() >= KEEP {
            recent.remove(0);
        }
        recent.push(exchange);
    }

    pub fn enabled(&self) -> bool {
        self.file.is_some()
    }

    /// Append a timing event (no content) to the diagnostics file, if enabled.
    pub fn event(&self, mut value: serde_json::Value) {
        let Some(path) = &self.file else { return };
        if let Some(map) = value.as_object_mut() {
            map.insert("at_ms".into(), serde_json::json!(epoch_ms()));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{value}");
        }
    }

    pub fn recent(&self) -> Vec<Exchange> {
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

pub fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `/api/v1/courses/123/assignments?x=1` -> `/api/v1/courses/:id/assignments`.
pub fn template(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.split('/')
        .map(|seg| {
            let id_like = !seg.is_empty()
                && (seg.chars().all(|c| c.is_ascii_digit() || c == '~')
                    || (seg.len() >= 16 && seg.chars().all(|c| c.is_ascii_hexdigit())));
            if id_like { ":id" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    #[test]
    fn templates_strip_identifiers_and_queries() {
        assert_eq!(
            super::template("/api/v1/courses/123/assignments?per_page=100"),
            "/api/v1/courses/:id/assignments"
        );
        assert_eq!(
            super::template("/api/v1/courses/1~2/assignments/9"),
            "/api/v1/courses/:id/assignments/:id"
        );
        assert_eq!(super::template("/api/graphql"), "/api/graphql");
    }
}
