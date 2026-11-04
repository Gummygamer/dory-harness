//! The search providers and the HTTP layer, against a loopback server that answers with canned
//! replies. No network, no key, no credits.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use rillwright_agent::web::{
    Brave, SearchError, SearchProvider, Tavily,
    http::{self, Request},
};

/// A key that is easy to find in any text. No test may see it in an error or a debug print.
const SECRET: &str = "SECRET-KEY-123";

/// Answers one connection with `status` and `body` after `delay`. Sends the request head and body
/// it received on the returned channel, so a test can check what was sent.
fn serve(status: u16, body: Vec<u8>, delay: Duration) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = tx.send(read_request(&mut stream));
        thread::sleep(delay);
        let mut reply = format!(
            "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend_from_slice(&body);
        let _ = stream.write_all(&reply);
    });
    (base, rx)
}

/// Reads a request's head, and its body when it has a Content-Length.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    let head_end = loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            return String::from_utf8_lossy(&data).to_string();
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&data).to_string()
}

fn tavily_body() -> Vec<u8> {
    br#"{"query":"x","follow_up_questions":null,"answer":null,"images":[],"results":[
        {"title":"Quillfeed changelog","url":"https://quillfeed.example/changelog","content":"3.4.0 added retry_after.","score":0.81,"raw_content":null},
        {"title":"Install","url":"https://quillfeed.example/install","content":"pip install quillfeed.","score":0.4,"raw_content":null}
    ],"response_time":1.2,"request_id":"abc"}"#
        .to_vec()
}

fn brave_body() -> Vec<u8> {
    br#"{"type":"search","query":{"original":"x"},"web":{"type":"search","results":[
        {"title":"Quillfeed changelog","url":"https://quillfeed.example/changelog","description":"3.4.0 added retry_after."},
        {"title":"Install","url":"https://quillfeed.example/install","description":"pip install quillfeed."}
    ]}}"#
        .to_vec()
}

fn tavily_for(status: u16, body: Vec<u8>) -> (Tavily, mpsc::Receiver<String>) {
    let (base, rx) = serve(status, body, Duration::ZERO);
    (Tavily::new(SECRET, &base), rx)
}

fn brave_for(status: u16, body: Vec<u8>) -> (Brave, mpsc::Receiver<String>) {
    let (base, rx) = serve(status, body, Duration::ZERO);
    (Brave::new(SECRET, &base), rx)
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---- normal replies -----------------------------------------------------------------------------

#[test]
fn tavily_returns_its_hits_and_sends_the_documented_request() {
    let (p, sent) = tavily_for(200, tavily_body());
    let hits = p
        .search("quillfeed retry_after", 5, &no_cancel())
        .expect("hits");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].title, "Quillfeed changelog");
    assert_eq!(hits[0].url, "https://quillfeed.example/changelog");
    assert_eq!(hits[0].content, "3.4.0 added retry_after.");

    let request = sent.recv().unwrap();
    assert!(request.starts_with("POST /search HTTP/1.1"), "{request}");
    assert!(
        request.contains("Authorization: Bearer SECRET-KEY-123"),
        "key must be sent as a bearer token: {request}"
    );
    assert!(
        request.contains(r#""query":"quillfeed retry_after""#),
        "{request}"
    );
    assert!(request.contains(r#""max_results":5"#), "{request}");
    assert!(request.contains(r#""search_depth":"basic""#), "{request}");
    assert!(request.contains(r#""include_answer":false"#), "{request}");
}

#[test]
fn tavily_empty_results_are_ok_and_empty() {
    let (p, _) = tavily_for(200, br#"{"query":"x","results":[]}"#.to_vec());
    assert_eq!(p.search("nothing", 5, &no_cancel()), Ok(vec![]));
}

#[test]
fn tavily_drops_hits_without_a_url_and_ignores_unknown_fields() {
    let body = br#"{"results":[{"title":"no link","content":"x","favicon":"f.ico"},{"title":"ok","url":"https://a.example","content":null,"score":0.2}]}"#;
    let (p, _) = tavily_for(200, body.to_vec());
    let hits = p.search("q", 5, &no_cancel()).expect("hits");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://a.example");
    assert_eq!(hits[0].content, "", "a null excerpt is an empty excerpt");
}

#[test]
fn brave_returns_its_hits_and_sends_the_documented_request() {
    let (p, sent) = brave_for(200, brave_body());
    let hits = p.search("quill feed/retry", 3, &no_cancel()).expect("hits");
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].content, "3.4.0 added retry_after.",
        "Brave's description is the excerpt"
    );

    let request = sent.recv().unwrap();
    assert!(
        request.starts_with("GET /res/v1/web/search?q=quill%20feed%2Fretry&count=3 HTTP/1.1"),
        "{request}"
    );
    assert!(
        request.contains("X-Subscription-Token: SECRET-KEY-123"),
        "{request}"
    );
}

#[test]
fn brave_reply_without_a_web_part_has_no_results() {
    let (p, _) = brave_for(200, br#"{"type":"search"}"#.to_vec());
    assert_eq!(p.search("q", 5, &no_cancel()), Ok(vec![]));
}

// ---- status codes ----------------------------------------------------------------------------------

#[test]
fn each_status_code_becomes_the_error_the_model_can_act_on() {
    let cases: &[(u16, SearchError)] = &[
        (401, SearchError::KeyRejected),
        (403, SearchError::KeyRejected),
        (429, SearchError::RateLimited),
        (432, SearchError::QuotaUsedUp),
        (433, SearchError::QuotaUsedUp),
        (500, SearchError::ServiceFailed(500)),
        (503, SearchError::ServiceFailed(503)),
    ];
    for (status, want) in cases {
        let (p, _) = tavily_for(*status, br#"{"detail":{"error":"x"}}"#.to_vec());
        assert_eq!(
            p.search("q", 5, &no_cancel()),
            Err(want.clone()),
            "HTTP {status}"
        );
    }
}

#[test]
fn an_unexpected_status_is_a_failure_that_says_which_one() {
    let (p, _) = tavily_for(404, br#"{"detail":"not found"}"#.to_vec());
    match p.search("q", 5, &no_cancel()) {
        Err(SearchError::Failed(msg)) => assert!(msg.contains("404"), "{msg}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn brave_429_is_a_quota_error_only_when_it_says_so() {
    let (p, _) = brave_for(429, br#"{"error":{"code":"QUOTA_LIMITED"}}"#.to_vec());
    assert_eq!(
        p.search("q", 5, &no_cancel()),
        Err(SearchError::QuotaUsedUp)
    );
    let (p, _) = brave_for(429, br#"{"error":{"code":"RATE_LIMITED"}}"#.to_vec());
    assert_eq!(
        p.search("q", 5, &no_cancel()),
        Err(SearchError::RateLimited)
    );
}

// ---- replies that are not what was expected ---------------------------------------------------

#[test]
fn a_reply_that_is_not_json_is_an_error() {
    let (p, _) = tavily_for(200, b"<html><body>Welcome</body></html>".to_vec());
    match p.search("q", 5, &no_cancel()) {
        Err(SearchError::Failed(msg)) => assert!(msg.contains("did not return JSON"), "{msg}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn an_html_error_page_with_a_5xx_status_is_a_service_failure() {
    let (p, _) = tavily_for(502, b"<html><body>Bad gateway</body></html>".to_vec());
    assert_eq!(
        p.search("q", 5, &no_cancel()),
        Err(SearchError::ServiceFailed(502))
    );
}

#[test]
fn json_of_the_wrong_shape_is_an_error() {
    let (p, _) = tavily_for(200, br#"{"results":"not a list"}"#.to_vec());
    assert!(matches!(
        p.search("q", 5, &no_cancel()),
        Err(SearchError::Failed(_))
    ));
}

#[test]
fn a_body_over_the_cap_is_refused() {
    let big = vec![b' '; 2 * 1024 * 1024];
    let (p, _) = tavily_for(200, big);
    match p.search("q", 5, &no_cancel()) {
        Err(SearchError::Failed(msg)) => assert!(msg.contains("larger"), "{msg}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

// ---- time and cancellation ------------------------------------------------------------------------

#[test]
fn a_server_that_never_answers_times_out() {
    // The provider's own timeout is 15 s, so this checks the layer with a short one.
    let (base, _rx) = serve(200, tavily_body(), Duration::from_secs(5));
    let started = Instant::now();
    let r = http::send(
        Request {
            url: format!("{base}/search"),
            headers: vec![],
            body: Some("{}".into()),
            timeout: Duration::from_millis(300),
            max_bytes: 1024,
        },
        &no_cancel(),
    );
    assert!(matches!(r, Err(SearchError::Failed(_))), "{r:?}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn cancel_during_a_slow_reply_returns_at_once() {
    let (base, _rx) = serve(200, tavily_body(), Duration::from_secs(5));
    let cancel = Arc::new(AtomicBool::new(false));
    let raise = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        raise.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let r = http::send(
        Request {
            url: format!("{base}/search"),
            headers: vec![],
            body: None,
            timeout: Duration::from_secs(30),
            max_bytes: 1024,
        },
        &cancel,
    );
    assert!(matches!(r, Err(SearchError::Cancelled)), "{r:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_search_already_cancelled_sends_nothing() {
    let cancel = AtomicBool::new(true);
    let (p, sent) = tavily_for(200, tavily_body());
    assert_eq!(p.search("q", 5, &cancel), Err(SearchError::Cancelled));
    assert!(
        sent.recv_timeout(Duration::from_millis(300)).is_err(),
        "a request was sent after cancel"
    );
}

// ---- the key never reaches a message ----------------------------------------------------------

#[test]
fn no_error_text_or_debug_print_contains_the_key() {
    let mut texts = Vec::new();
    for status in [401, 403, 429, 432, 433, 404, 500, 503] {
        let (p, _) = tavily_for(status, br#"{"error":"x"}"#.to_vec());
        texts.push(format!("{:?}", p.search("q", 5, &no_cancel())));
    }
    let (p, _) = brave_for(429, br#"{"error":{"code":"QUOTA_LIMITED"}}"#.to_vec());
    texts.push(format!("{:?}", p.search("q", 5, &no_cancel())));
    // A connection that is refused, so the error comes from the transport.
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let refused = Tavily::new(SECRET, &format!("http://{closed}"));
    texts.push(format!("{:?}", refused.search("q", 5, &no_cancel())));
    texts.push(format!("{:?}", Tavily::new(SECRET, "http://127.0.0.1:1")));
    texts.push(format!("{:?}", Brave::new(SECRET, "http://127.0.0.1:1")));
    for t in &texts {
        assert!(!t.contains(SECRET), "the key leaked: {t}");
    }
}

#[test]
fn an_unreachable_service_is_a_failure_with_a_message() {
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let p = Tavily::new(SECRET, &format!("http://{closed}"));
    match p.search("q", 5, &no_cancel()) {
        Err(SearchError::Failed(msg)) => assert!(msg.contains("could not reach"), "{msg}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn messages_for_the_model_say_what_to_do() {
    assert_eq!(
        SearchError::RateLimited.to_string(),
        "the search quota is used up or rate-limited; answer without searching"
    );
    assert_eq!(
        SearchError::KeyRejected.to_string(),
        "the search key was rejected"
    );
    assert_eq!(
        SearchError::ServiceFailed(500).to_string(),
        "the search service failed (HTTP 500)"
    );
}

// ---- the quota is reported and remembered (step 9) -----------------------------------------------

#[test]
fn an_over_quota_reply_is_reported_and_recorded_and_a_later_success_clears_it() {
    use rillwright_agent::{
        builtin::web::WebSearch,
        tool::{Tool, ToolCtx},
        web::state,
        workspace::Workspace,
    };

    let dir = tempfile::tempdir().unwrap();
    let state_file = state::state_path(dir.path());

    // Over quota: the model is told, and the time is written to the state file.
    let (base, _rx) = serve(
        432,
        br#"{"detail":{"error":"plan limit"}}"#.to_vec(),
        Duration::ZERO,
    );
    let tool = WebSearch::new(Box::new(Tavily::new(SECRET, &base)), 10)
        .with_state_file(state_file.clone());
    let ctx = ToolCtx::new(Workspace::new(dir.path(), &[]).unwrap());
    let mut args = serde_json::Map::new();
    args.insert("query".into(), serde_json::Value::String("serde".into()));
    let out = tool.run(&args, &ctx);
    assert!(out.is_error);
    assert!(
        out.text.contains("the search quota is used up"),
        "{}",
        out.text
    );
    assert!(!out.text.contains(SECRET));
    let at = state::quota_used_up_at(&state_file).expect("the time is recorded");
    assert!(at.ends_with(" UTC"), "{at}");
    let stored = std::fs::read_to_string(&state_file).unwrap();
    assert!(
        !stored.contains(SECRET),
        "the state file must not hold the key: {stored}"
    );

    // A later search that succeeds clears the record, because the quota has reset.
    let (base, _rx) = serve(200, tavily_body(), Duration::ZERO);
    let tool = WebSearch::new(Box::new(Tavily::new(SECRET, &base)), 10)
        .with_state_file(state_file.clone());
    args.insert(
        "query".into(),
        serde_json::Value::String("serde other".into()),
    );
    assert!(!tool.run(&args, &ctx).is_error);
    assert_eq!(state::quota_used_up_at(&state_file), None);
}
