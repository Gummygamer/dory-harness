//! The HTTP side of the search providers.
//!
//! The request runs on a worker thread, and the caller checks the cancel flag every 100 ms, so
//! Ctrl-C returns at once. An abandoned worker ends at its own timeout. The body is capped while
//! it is read, so a large reply is never held in memory. Errors never include the request's
//! headers, so the search key cannot reach an error message.

use std::{
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use super::SearchError;

/// How often a waiting caller checks the cancel flag.
const POLL: Duration = Duration::from_millis(100);

/// A finished HTTP exchange. Error statuses are returned as they are, so the provider can read
/// the reply (or refuse it) with the right message.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// One request. `body: None` sends a GET.
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout: Duration,
    pub max_bytes: usize,
}

/// Sends the request and waits for the reply, returning early with `Cancelled` when `cancel` is
/// raised.
pub fn send(req: Request, cancel: &AtomicBool) -> Result<Response, SearchError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(SearchError::Cancelled);
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // The receiver may be gone if the caller was cancelled; the result is then dropped.
        let _ = tx.send(perform(&req));
    });
    loop {
        match rx.recv_timeout(POLL) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::SeqCst) {
                    return Err(SearchError::Cancelled);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(SearchError::Failed("the search request stopped unexpectedly".into()));
            }
        }
    }
}

fn perform(req: &Request) -> Result<Response, SearchError> {
    let agent = ureq::AgentBuilder::new().timeout(req.timeout).build();
    let mut call = match &req.body {
        Some(_) => agent.post(&req.url),
        None => agent.get(&req.url),
    };
    for (name, value) in &req.headers {
        call = call.set(name, value);
    }
    let result = match &req.body {
        Some(body) => call.send_string(body),
        None => call.call(),
    };
    match result {
        Ok(response) => {
            let status = response.status();
            read_capped(response, status, req.max_bytes)
        }
        // A status error still carries a reply, which the provider may need to read.
        Err(ureq::Error::Status(status, response)) => read_capped(response, status, req.max_bytes),
        Err(ureq::Error::Transport(transport)) => {
            Err(SearchError::Failed(format!("could not reach the search service ({:?})", transport.kind())))
        }
    }
}

/// Reads at most `max` bytes. One byte past the cap is read to detect the overflow, then reading
/// stops, so an oversized reply costs no more than the cap plus one byte.
fn read_capped(response: ureq::Response, status: u16, max: usize) -> Result<Response, SearchError> {
    let mut body = Vec::new();
    let mut reader = response.into_reader().take(max as u64 + 1);
    if let Err(e) = reader.read_to_end(&mut body) {
        return Err(SearchError::Failed(format!("the search reply could not be read ({:?})", e.kind())));
    }
    if body.len() > max {
        return Err(SearchError::Failed(format!("the search reply is larger than {max} bytes")));
    }
    Ok(Response { status, body })
}
