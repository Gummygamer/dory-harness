//! Search providers: Tavily (the default) and Brave. Each turns a query into hits, and each
//! turns the service's refusals into a [`SearchError`] that the model can act on.

use std::{fmt, sync::atomic::AtomicBool, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};

use super::http::{self, Request, Response};

/// Request timeout for one search.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// Largest reply a search may return, in bytes.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// One result: a title, a link and the excerpt the service chose for the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub content: String,
}

/// Why a search failed. The `Display` text is what the model reads, so it never contains the key
/// or any header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// 401 or 403.
    KeyRejected,
    /// 429: too many requests. The quota is not necessarily used up.
    RateLimited,
    /// The plan's or key's quota is used up (Tavily 432 and 433, Brave `QUOTA_LIMITED`).
    QuotaUsedUp,
    /// 5xx.
    ServiceFailed(u16),
    /// Anything else: no connection, a timeout, a reply that is not JSON, a reply over the cap.
    Failed(String),
    /// Ctrl-C while the search was in flight.
    Cancelled,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchError::KeyRejected => write!(f, "the search key was rejected"),
            SearchError::RateLimited | SearchError::QuotaUsedUp => {
                write!(
                    f,
                    "the search quota is used up or rate-limited; answer without searching"
                )
            }
            SearchError::ServiceFailed(status) => {
                write!(f, "the search service failed (HTTP {status})")
            }
            SearchError::Failed(msg) => f.write_str(msg),
            SearchError::Cancelled => write!(f, "the search was cancelled"),
        }
    }
}

pub trait SearchProvider {
    /// The name shown to the user and written to the session header.
    fn name(&self) -> &str;
    /// Up to `n` hits for `query`. Hits without a URL are dropped.
    fn search(
        &self,
        query: &str,
        n: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<SearchHit>, SearchError>;
}

/// Maps a status to an error, or accepts it. `body` is only used for Brave's 429 codes.
fn check_status(status: u16, body: &[u8]) -> Result<(), SearchError> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(SearchError::KeyRejected),
        429 if String::from_utf8_lossy(body).contains("QUOTA_LIMITED") => {
            Err(SearchError::QuotaUsedUp)
        }
        429 => Err(SearchError::RateLimited),
        // Tavily: 432 is the key or plan limit, 433 the pay-as-you-go limit. Both mean the quota is used up.
        432 | 433 => Err(SearchError::QuotaUsedUp),
        500..=599 => Err(SearchError::ServiceFailed(status)),
        other => Err(SearchError::Failed(format!(
            "the search service replied with HTTP {other}"
        ))),
    }
}

/// The reply as JSON, or an error if the status is refused or the body is not JSON.
fn json_reply(resp: Response) -> Result<Value, SearchError> {
    check_status(resp.status, &resp.body)?;
    serde_json::from_slice(&resp.body)
        .map_err(|_| SearchError::Failed("the search service did not return JSON".into()))
}

/// Tavily's search API. The key goes in the `Authorization` header only.
pub struct Tavily {
    key: String,
    base: String,
}

impl Tavily {
    /// `base` is `https://api.tavily.com` in use; tests point it at a loopback server.
    pub fn new(key: &str, base: &str) -> Self {
        Tavily {
            key: key.to_string(),
            base: base.trim_end_matches('/').to_string(),
        }
    }
}

impl fmt::Debug for Tavily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tavily")
            .field("key", &"<redacted>")
            .field("base", &self.base)
            .finish()
    }
}

#[derive(Deserialize)]
struct TavilyReply {
    #[serde(default)]
    results: Vec<TavilyHit>,
}

#[derive(Deserialize)]
struct TavilyHit {
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
}

/// Reads Tavily's reply. Unknown fields are ignored, so new ones do not break the search.
pub fn parse_tavily(json: &Value) -> Result<Vec<SearchHit>, SearchError> {
    let reply: TavilyReply = serde_json::from_value(json.clone())
        .map_err(|_| SearchError::Failed("the search reply had an unexpected shape".into()))?;
    Ok(reply
        .results
        .into_iter()
        .filter_map(|h| {
            let url = h.url.unwrap_or_default();
            (!url.trim().is_empty()).then(|| SearchHit {
                title: h.title.unwrap_or_default(),
                url,
                content: h.content.unwrap_or_default(),
            })
        })
        .collect())
}

impl SearchProvider for Tavily {
    fn name(&self) -> &str {
        "tavily"
    }

    fn search(
        &self,
        query: &str,
        n: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let body = json!({
            "query": query,
            "max_results": n,
            "search_depth": "basic",
            "include_answer": false,
        });
        let resp = http::send(
            Request {
                url: format!("{}/search", self.base),
                headers: vec![
                    ("Authorization".into(), format!("Bearer {}", self.key)),
                    ("Content-Type".into(), "application/json".into()),
                ],
                body: Some(body.to_string()),
                timeout: TIMEOUT,
                max_bytes: MAX_BODY_BYTES,
            },
            cancel,
        )?;
        parse_tavily(&json_reply(resp)?)
    }
}

/// Brave's web search API. The key goes in the `X-Subscription-Token` header only.
pub struct Brave {
    key: String,
    base: String,
}

impl Brave {
    /// `base` is `https://api.search.brave.com` in use; tests point it at a loopback server.
    pub fn new(key: &str, base: &str) -> Self {
        Brave {
            key: key.to_string(),
            base: base.trim_end_matches('/').to_string(),
        }
    }
}

impl fmt::Debug for Brave {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Brave")
            .field("key", &"<redacted>")
            .field("base", &self.base)
            .finish()
    }
}

#[derive(Deserialize)]
struct BraveReply {
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveHit>,
}

#[derive(Deserialize)]
struct BraveHit {
    title: Option<String>,
    url: Option<String>,
    description: Option<String>,
}

/// Reads Brave's reply. A reply with no `web` part has no results.
pub fn parse_brave(json: &Value) -> Result<Vec<SearchHit>, SearchError> {
    let reply: BraveReply = serde_json::from_value(json.clone())
        .map_err(|_| SearchError::Failed("the search reply had an unexpected shape".into()))?;
    Ok(reply
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|h| {
            let url = h.url.unwrap_or_default();
            (!url.trim().is_empty()).then(|| SearchHit {
                title: h.title.unwrap_or_default(),
                url,
                content: h.description.unwrap_or_default(),
            })
        })
        .collect())
}

/// Percent-encodes everything but unreserved characters, for a query string.
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl SearchProvider for Brave {
    fn name(&self) -> &str {
        "brave"
    }

    fn search(
        &self,
        query: &str,
        n: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let resp = http::send(
            Request {
                url: format!(
                    "{}/res/v1/web/search?q={}&count={n}",
                    self.base,
                    encode_query(query)
                ),
                headers: vec![
                    ("X-Subscription-Token".into(), self.key.clone()),
                    ("Accept".into(), "application/json".into()),
                ],
                body: None,
                timeout: TIMEOUT,
                max_bytes: MAX_BODY_BYTES,
            },
            cancel,
        )?;
        parse_brave(&json_reply(resp)?)
    }
}
