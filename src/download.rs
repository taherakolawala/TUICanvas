//! File downloads. Canvas answers a download with a redirect to its storage
//! service (observed: `*.canvas-user-content.com` -> `inst-fs-*.inscloudgate.net`
//! -> `cdn.inst-fs-*.inscloudgate.net`), which authorises the request with a
//! signed URL. Cookies go only to the Canvas origin; redirects may only lead
//! to Canvas's storage hosts. The body is streamed to a partial file and the
//! caller installs it only once it is complete.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Hosts Canvas uses to serve file contents. Anything else is refused.
const STORAGE_SUFFIXES: [&str; 2] = [".canvas-user-content.com", ".inscloudgate.net"];
const MAX_HOPS: usize = 6;
/// Larger files are refused; Canvas files the reader handles are documents and images.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    /// The download tried to leave Canvas and its storage hosts.
    Refused(&'static str),
    NeedsLogin,
    NoAccess(u16),
    Status(u16),
    TooLarge,
    Network,
    Disk,
}

impl DownloadError {
    pub fn describe(&self) -> String {
        match self {
            DownloadError::Refused(why) => format!("download refused: {why}"),
            DownloadError::NeedsLogin => "signed out or session expired".into(),
            DownloadError::NoAccess(s) => format!("no access to this file (HTTP {s})"),
            DownloadError::Status(s) => format!("download failed (HTTP {s})"),
            DownloadError::TooLarge => "file is larger than 2 GB".into(),
            DownloadError::Network => "network error during download".into(),
            DownloadError::Disk => "could not write the file".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hop {
    /// The Canvas origin: send the session cookies.
    Canvas,
    /// A Canvas storage host: signed URL, no cookies.
    Storage,
}

/// Decide whether a URL may be fetched during a download, and with which credentials.
pub fn hop(url: &str, origin_host: &str) -> Result<(Hop, reqwest::Url), DownloadError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| DownloadError::Refused("malformed address"))?;
    if parsed.scheme() != "https" {
        return Err(DownloadError::Refused("not HTTPS"));
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some_and(|p| p != 443)
    {
        return Err(DownloadError::Refused("unexpected address form"));
    }
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if host == origin_host {
        return Ok((Hop::Canvas, parsed));
    }
    if STORAGE_SUFFIXES.iter().any(|s| host.ends_with(s)) {
        return Ok((Hop::Storage, parsed));
    }
    Err(DownloadError::Refused("left Canvas's storage hosts"))
}

pub struct Downloader {
    client: reqwest::Client,
    origin_host: String,
}

impl Downloader {
    pub fn new(origin_host: &str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .https_only(true)
            .user_agent(concat!("TUICanvas/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "Could not initialise the download client.".to_string())?;
        Ok(Downloader {
            client,
            origin_host: origin_host.to_ascii_lowercase(),
        })
    }

    /// Download `url` into `partial`, calling `progress(received, total)` as data arrives.
    /// `cookie` is sent only to the Canvas origin. Returns the size written.
    pub async fn fetch(
        &self,
        url: &str,
        cookie: Option<&str>,
        partial: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
    ) -> Result<u64, DownloadError> {
        let mut next = url.to_string();
        for _ in 0..MAX_HOPS {
            let (kind, target) = hop(&next, &self.origin_host)?;
            let mut request = self.client.get(target.clone());
            if let (Hop::Canvas, Some(cookie)) = (kind, cookie) {
                request = request.header("cookie", cookie);
            }
            let mut response = request.send().await.map_err(|_| DownloadError::Network)?;
            let status = response.status().as_u16();
            match status {
                300..=399 => {
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .ok_or(DownloadError::Status(status))?;
                    let joined = target
                        .join(location)
                        .map_err(|_| DownloadError::Refused("malformed redirect"))?;
                    if kind == Hop::Canvas && joined.path().starts_with("/login") {
                        return Err(DownloadError::NeedsLogin);
                    }
                    next = joined.to_string();
                }
                200 => {
                    let total = response.content_length();
                    if total.is_some_and(|t| t > MAX_BYTES) {
                        return Err(DownloadError::TooLarge);
                    }
                    if let Some(dir) = partial.parent() {
                        std::fs::create_dir_all(dir).map_err(|_| DownloadError::Disk)?;
                    }
                    let mut file =
                        std::fs::File::create(partial).map_err(|_| DownloadError::Disk)?;
                    let mut received = 0u64;
                    progress(0, total);
                    while let Some(chunk) =
                        response.chunk().await.map_err(|_| DownloadError::Network)?
                    {
                        received += chunk.len() as u64;
                        if received > MAX_BYTES {
                            return Err(DownloadError::TooLarge);
                        }
                        file.write_all(&chunk).map_err(|_| DownloadError::Disk)?;
                        progress(received, total);
                    }
                    file.sync_all().map_err(|_| DownloadError::Disk)?;
                    if total.is_some_and(|t| t != received) {
                        return Err(DownloadError::Network);
                    }
                    return Ok(received);
                }
                401 if kind == Hop::Canvas => return Err(DownloadError::NeedsLogin),
                401 | 403 | 404 => return Err(DownloadError::NoAccess(status)),
                _ => return Err(DownloadError::Status(status)),
            }
        }
        Err(DownloadError::Refused("too many redirects"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "ufl.instructure.com";

    #[test]
    fn cookies_only_for_canvas_and_only_storage_hosts_allowed() {
        let ok = |u: &str| hop(u, HOST).map(|(k, _)| k);
        assert_eq!(
            ok("https://ufl.instructure.com/files/1/download?verifier=x"),
            Ok(Hop::Canvas)
        );
        assert_eq!(
            ok("https://a1016-88386043.cluster30.canvas-user-content.com/courses/x/files/1/x"),
            Ok(Hop::Storage)
        );
        assert_eq!(
            ok("https://inst-fs-iad-prod.inscloudgate.net/files/blob"),
            Ok(Hop::Storage)
        );
        assert_eq!(
            ok("https://cdn.inst-fs-iad-prod.inscloudgate.net/x"),
            Ok(Hop::Storage)
        );
        assert!(
            ok("http://ufl.instructure.com/files/1").is_err(),
            "plain HTTP refused"
        );
        assert!(ok("https://evil.example/files/1").is_err());
        assert!(ok("https://ufl.instructure.com.evil.example/x").is_err());
        assert!(ok("https://user@ufl.instructure.com/x").is_err());
        assert!(ok("https://ufl.instructure.com:8443/x").is_err());
        // Another Canvas tenant is not a storage host and never gets this origin's cookies.
        assert!(ok("https://other.instructure.com/x").is_err());
    }
}
