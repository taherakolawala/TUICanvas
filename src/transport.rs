//! HTTP transport behind a small trait so the Canvas client can be tested offline.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Responses larger than this are refused rather than buffered.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default)]
pub struct HttpResponse {
    pub status: u16,
    /// Lower-case header names, in received order (Set-Cookie may repeat).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub ms: f64,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn headers_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Network failures carry no URLs or response data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    Timeout,
    Connect,
    TooLarge,
    Other,
}

impl NetError {
    pub fn describe(self) -> &'static str {
        match self {
            NetError::Timeout => "request timed out",
            NetError::Connect => "could not connect",
            NetError::TooLarge => "response too large",
            NetError::Other => "network error",
        }
    }
}

pub trait Transport: Send + Sync {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, NetError>>;
}

/// One pooled client for the life of the process.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .user_agent(concat!("TUICanvas/", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(|_| "Could not initialise the HTTPS client.".to_string())?;
        Ok(ReqwestTransport { client })
    }
}

fn net_error(e: &reqwest::Error) -> NetError {
    if e.is_timeout() {
        NetError::Timeout
    } else if e.is_connect() {
        NetError::Connect
    } else {
        NetError::Other
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, NetError>> {
        Box::pin(async move {
            let started = Instant::now();
            let mut builder = match request.method {
                Method::Get => self.client.get(&request.url),
                Method::Post => self.client.post(&request.url),
            };
            for (name, value) in &request.headers {
                builder = builder.header(*name, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let mut response = builder.send().await.map_err(|e| net_error(&e))?;
            if response
                .content_length()
                .is_some_and(|n| n as usize > MAX_BODY)
            {
                return Err(NetError::TooLarge);
            }
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(n, v)| {
                    Some((
                        n.as_str().to_ascii_lowercase(),
                        v.to_str().ok()?.to_string(),
                    ))
                })
                .collect();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| net_error(&e))? {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(NetError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(HttpResponse {
                status,
                headers,
                body,
                ms: started.elapsed().as_secs_f64() * 1000.0,
            })
        })
    }
}
