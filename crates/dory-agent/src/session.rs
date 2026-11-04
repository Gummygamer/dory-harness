//! Session transcripts under `<workspace>/.dory/sessions/<id>/`.
//!
//! `transcript.jsonl` has one JSON object per line: a header with the run settings, then each
//! message as it is added, each round's stats, and the end reason. Every line is flushed when it
//! is written, so a crash loses at most the line being written.
//!
//! Resuming rebuilds the messages from the transcript. A tool call whose result was never written
//! gets an "outcome unknown" result instead of being run again, since it may already have had an
//! effect (a write or a command).

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use dory_engine::chat::Message;
use serde_json::{Value, json};

use crate::agent::{AgentEvent, Reason};

pub const TRANSCRIPT_VERSION: u64 = 1;
pub const UNKNOWN_OUTCOME: &str = "[outcome unknown: the session stopped while this call was running. It was not run again. Check the state before relying on it.]";

pub struct Session {
    pub id: String,
    dir: PathBuf,
    file: File,
    lock_path: PathBuf,
}

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

struct LockGuard(Option<PathBuf>);
impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let serial = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{}-{serial}", std::process::id())
}

fn session_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".dory").join("sessions").join(id)
}

impl Session {
    /// Starts a new session. `header` holds the run settings and is the first line.
    pub fn create(root: &Path, header: Value) -> io::Result<Session> {
        let base = root.join(".dory").join("sessions");
        fs::create_dir_all(&base)?;
        let (id, dir) = loop {
            let id = new_id();
            let dir = session_dir(root, &id);
            match fs::create_dir(&dir) {
                Ok(()) => break (id, dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let lock_path = dir.join(".writer.lock");
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)?;
        drop(lock);
        let mut lock_guard = LockGuard(Some(lock_path.clone()));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join("transcript.jsonl"))?;
        let mut s = Session {
            id,
            dir,
            file,
            lock_path,
        };
        lock_guard.0 = None;
        let mut h = header;
        if let Some(obj) = h.as_object_mut() {
            obj.insert("type".into(), json!("session"));
            obj.insert("version".into(), json!(TRANSCRIPT_VERSION));
            obj.insert("id".into(), json!(s.id));
        }
        s.write_line(&h)?;
        Ok(s)
    }

    /// Reopens a session and rebuilds its messages. Also returns the header.
    pub fn open(root: &Path, id: &str) -> io::Result<(Session, Vec<Message>, Value)> {
        validate_id(id)?;
        let base = root.join(".dory").join("sessions").canonicalize()?;
        let requested_dir = session_dir(root, id);
        if !fs::symlink_metadata(&requested_dir)?.file_type().is_dir() {
            return Err(io::Error::other("session path is not a directory"));
        }
        let dir = requested_dir.canonicalize()?;
        if !dir.starts_with(&base) {
            return Err(io::Error::other(
                "session path escapes the sessions directory",
            ));
        }
        let path = dir.join("transcript.jsonl");
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(io::Error::other("transcript is not a regular file"));
        }
        let lock_path = dir.join(".writer.lock");
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    io::Error::other("session is locked by another writer")
                } else {
                    e
                }
            })?;
        drop(lock);
        let mut lock_guard = LockGuard(Some(lock_path.clone()));
        let bytes = fs::read(&path)?;
        let mut header = None;
        let mut messages = Vec::new();
        let mut offset = 0usize;
        let mut valid_end = 0usize;
        let mut truncated_tail = false;
        let mut lines = bytes.split_inclusive(|b| *b == b'\n').peekable();
        while let Some(line_bytes) = lines.next() {
            let has_newline = line_bytes.last() == Some(&b'\n');
            let line = line_bytes.strip_suffix(b"\n").unwrap_or(line_bytes);
            let parsed = serde_json::from_slice::<Value>(line);
            let v = match parsed {
                Ok(v) => v,
                Err(_e) if !has_newline && lines.peek().is_none() => {
                    let damaged = dir.join(format!("transcript.damaged-{}.tail", new_id()));
                    fs::write(&damaged, line)?;
                    let f = OpenOptions::new().write(true).open(&path)?;
                    f.set_len(offset as u64)?;
                    truncated_tail = true;
                    break;
                }
                Err(e) => {
                    return Err(io::Error::other(format!(
                        "{} has a corrupt record at byte {offset}: {e}",
                        path.display()
                    )));
                }
            };
            match v.get("type").and_then(Value::as_str) {
                Some("session") => header = Some(v),
                Some("message") => {
                    if let Some(m) = v
                        .get("message")
                        .and_then(|m| serde_json::from_value::<Message>(m.clone()).ok())
                    {
                        messages.push(m);
                    }
                }
                _ => {}
            }
            offset += line_bytes.len();
            valid_end = offset;
        }
        let header = header
            .ok_or_else(|| io::Error::other(format!("{} has no session header", path.display())))?;
        if header.get("version").and_then(Value::as_u64) != Some(TRANSCRIPT_VERSION) {
            return Err(io::Error::other(format!(
                "{} has an unsupported transcript version",
                path.display()
            )));
        }
        repair(&mut messages);
        let mut file = OpenOptions::new().append(true).open(&path)?;
        if !truncated_tail && !bytes.is_empty() && !bytes.ends_with(b"\n") {
            file.write_all(b"\n")?;
            file.flush()?;
        }
        let mut session = Session {
            id: id.to_string(),
            dir,
            file,
            lock_path,
        };
        lock_guard.0 = None;
        for message in messages.iter().skip(read_message_count(&bytes, valid_end)) {
            session.write_line(&json!({"type": "message", "message": message}))?;
        }
        Ok((session, messages, header))
    }

    /// Reads a transcript without taking its writer lock. A torn final line is ignored and
    /// unfinished tool calls are shown as unknown in the returned in-memory messages.
    pub fn read_only(root: &Path, id: &str) -> io::Result<(Vec<Message>, Value)> {
        validate_id(id)?;
        let base = root.join(".dory").join("sessions").canonicalize()?;
        let requested = session_dir(root, id);
        if !fs::symlink_metadata(&requested)?.file_type().is_dir() {
            return Err(io::Error::other("session path is not a directory"));
        }
        let dir = requested.canonicalize()?;
        if !dir.starts_with(&base) {
            return Err(io::Error::other(
                "session path escapes the sessions directory",
            ));
        }
        let path = dir.join("transcript.jsonl");
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(io::Error::other("transcript is not a regular file"));
        }
        let bytes = fs::read(&path)?;
        let mut header = None;
        let mut messages = Vec::new();
        let mut offset = 0usize;
        for line_bytes in bytes.split_inclusive(|b| *b == b'\n') {
            let has_newline = line_bytes.last() == Some(&b'\n');
            let line = line_bytes.strip_suffix(b"\n").unwrap_or(line_bytes);
            let value = match serde_json::from_slice::<Value>(line) {
                Ok(value) => value,
                Err(_) if !has_newline && offset + line_bytes.len() == bytes.len() => break,
                Err(error) => {
                    return Err(io::Error::other(format!(
                        "{} has a corrupt record at byte {offset}: {error}",
                        path.display()
                    )));
                }
            };
            match value.get("type").and_then(Value::as_str) {
                Some("session") => header = Some(value),
                Some("message") => {
                    if let Some(message) = value
                        .get("message")
                        .and_then(|m| serde_json::from_value::<Message>(m.clone()).ok())
                    {
                        messages.push(message);
                    }
                }
                _ => {}
            }
            offset += line_bytes.len();
        }
        let header = header
            .ok_or_else(|| io::Error::other(format!("{} has no session header", path.display())))?;
        if header.get("version").and_then(Value::as_u64) != Some(TRANSCRIPT_VERSION) {
            return Err(io::Error::other(format!(
                "{} has an unsupported transcript version",
                path.display()
            )));
        }
        repair(&mut messages);
        Ok((messages, header))
    }

    /// Where full command output and other large results are kept for this session.
    pub fn scratch_dir(&self) -> PathBuf {
        self.dir.join("output")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes the events that belong in the transcript. Others are ignored.
    pub fn record(&mut self, ev: &AgentEvent) -> io::Result<()> {
        match ev {
            AgentEvent::MessageAdded(m) => {
                self.write_line(&json!({"type": "message", "message": m}))
            }
            AgentEvent::RoundStats(s) => self.write_line(&json!({
                "type": "round",
                "finish": format!("{:?}", s.finish),
                "prompt_tokens": s.prompt_tokens,
                "reused_tokens": s.reused_tokens,
                "generated": s.generated,
                "prompt_ms": s.prompt_ms,
                "gen_ms": s.gen_ms,
            })),
            AgentEvent::Finished(reason) => self.finish(*reason),
            _ => Ok(()),
        }
    }

    pub fn finish(&mut self, reason: Reason) -> io::Result<()> {
        self.write_line(&json!({"type": "finished", "reason": reason.as_str()}))
    }

    fn write_line(&mut self, v: &Value) -> io::Result<()> {
        let mut line = serde_json::to_string(v).map_err(io::Error::other)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.flush()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

fn validate_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.chars().any(char::is_control)
    {
        return Err(io::Error::other("invalid session id"));
    }
    Ok(())
}

fn read_message_count(bytes: &[u8], end: usize) -> usize {
    bytes[..end.min(bytes.len())]
        .split(|b| *b == b'\n')
        .filter(|line| {
            serde_json::from_slice::<Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("type")
                        .and_then(Value::as_str)
                        .map(|t| t == "message")
                })
                .unwrap_or(false)
        })
        .count()
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    pub last_modified: Option<SystemTime>,
    pub records: usize,
}

/// Lists readable transcripts without acquiring a writer lock or opening append mode.
pub fn list(root: &Path) -> io::Result<Vec<SessionInfo>> {
    let base = root.join(".dory").join("sessions");
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&base) else {
        return Ok(out);
    };
    for entry in entries {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().to_string();
        if validate_id(&id).is_err() {
            continue;
        }
        let dir = entry.path();
        if !fs::symlink_metadata(&dir)?.file_type().is_dir() {
            continue;
        }
        let Ok(real_dir) = dir.canonicalize() else {
            continue;
        };
        let Ok(real_base) = base.canonicalize() else {
            return Ok(out);
        };
        if !real_dir.starts_with(real_base) {
            continue;
        }
        let path = real_dir.join("transcript.jsonl");
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            continue;
        }
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let bytes = fs::read(&path)?;
        out.push(SessionInfo {
            id,
            path,
            last_modified: meta.modified().ok(),
            records: bytes
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
                .count(),
        });
    }
    out.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    Ok(out)
}

/// Gives every tool call that has no result yet an "outcome unknown" result. Returns how many were
/// added. Only the last assistant message can be unfinished, so only it is checked.
pub fn repair(messages: &mut Vec<Message>) -> usize {
    let Some(last_assistant) = messages.iter().rposition(|m| m.role == "assistant") else {
        return 0;
    };
    let calls = messages[last_assistant].tool_calls.len();
    let answered = messages[last_assistant + 1..]
        .iter()
        .take_while(|m| m.role == "tool")
        .count();
    let missing = calls.saturating_sub(answered);
    for _ in 0..missing {
        messages.push(Message::new("tool", UNKNOWN_OUTCOME));
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use dory_engine::GenStats;
    use dory_engine::chat::ToolCall;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn header() -> Value {
        json!({"model": "dory", "loops": 2, "cache": "last_loop", "seed": 42})
    }

    #[test]
    fn messages_round_trip_through_the_transcript() {
        let r = root();
        let mut s = Session::create(r.path(), header()).unwrap();
        let id = s.id.clone();
        s.record(&AgentEvent::MessageAdded(Message::new("user", "task")))
            .unwrap();
        s.record(&AgentEvent::RoundStats(GenStats {
            finish: dory_engine::Finish::Stop,
            prompt_tokens: 10,
            reused_tokens: 0,
            chunks: vec![10],
            generated: 3,
            prompt_ms: 1.0,
            gen_ms: 2.0,
        }))
        .unwrap();
        s.record(&AgentEvent::MessageAdded(Message::new(
            "assistant",
            "answer",
        )))
        .unwrap();
        s.finish(Reason::Answered).unwrap();
        drop(s);

        let (_s, messages, h) = Session::open(r.path(), &id).unwrap();
        assert_eq!(
            messages,
            vec![
                Message::new("user", "task"),
                Message::new("assistant", "answer")
            ]
        );
        assert_eq!(h["model"], "dory");
    }

    #[test]
    fn a_session_has_one_writer_and_history_listing_is_read_only() {
        let r = root();
        let s = Session::create(r.path(), header()).unwrap();
        let id = s.id.clone();
        assert!(Session::open(r.path(), &id).is_err());
        let (messages, _) = Session::read_only(r.path(), &id).unwrap();
        assert!(messages.is_empty());
        let listed = list(r.path()).unwrap();
        assert_eq!(listed.len(), 1);
        drop(s);
        let (opened, _, _) = Session::open(r.path(), &id).unwrap();
        drop(opened);
        assert!(Session::open(r.path(), "../escape").is_err());
    }

    #[test]
    fn torn_final_tail_is_preserved_and_repaired_before_append() {
        let r = root();
        let mut s = Session::create(r.path(), header()).unwrap();
        let id = s.id.clone();
        s.record(&AgentEvent::MessageAdded(Message::new("user", "first")))
            .unwrap();
        let path = s.dir().join("transcript.jsonl");
        drop(s);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"message\",\"torn")
            .unwrap();
        let (mut reopened, messages, _) = Session::open(r.path(), &id).unwrap();
        assert_eq!(messages, vec![Message::new("user", "first")]);
        reopened
            .record(&AgentEvent::MessageAdded(Message::new(
                "assistant",
                "second",
            )))
            .unwrap();
        drop(reopened);
        let (_, messages, _) = Session::open(r.path(), &id).unwrap();
        assert_eq!(
            messages,
            vec![
                Message::new("user", "first"),
                Message::new("assistant", "second")
            ]
        );
        assert!(
            fs::read_dir(session_dir(r.path(), &id))
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains("damaged-"))
        );
    }

    #[test]
    fn a_line_cut_short_by_a_crash_is_skipped() {
        let r = root();
        let mut s = Session::create(r.path(), header()).unwrap();
        let id = s.id.clone();
        s.record(&AgentEvent::MessageAdded(Message::new("user", "task")))
            .unwrap();
        let path = s.dir().join("transcript.jsonl");
        drop(s);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"type\":\"message\",\"mes").unwrap();
        let (_s, messages, _) = Session::open(r.path(), &id).unwrap();
        assert_eq!(messages, vec![Message::new("user", "task")]);
    }

    #[test]
    fn an_unfinished_tool_call_is_marked_unknown_and_not_rerun() {
        let mut messages = vec![
            Message::new("user", "task"),
            Message {
                role: "assistant".into(),
                tool_calls: vec![
                    ToolCall {
                        name: "write_file".into(),
                        arguments: Default::default(),
                    },
                    ToolCall {
                        name: "run_command".into(),
                        arguments: Default::default(),
                    },
                ],
                ..Default::default()
            },
            Message::new("tool", "created x"),
        ];
        assert_eq!(repair(&mut messages), 1);
        assert_eq!(messages.last().unwrap().content, UNKNOWN_OUTCOME);
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn a_finished_turn_needs_no_repair() {
        let mut messages = vec![
            Message::new("user", "task"),
            Message::new("assistant", "done"),
        ];
        assert_eq!(repair(&mut messages), 0);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn a_missing_session_is_an_error() {
        let r = root();
        assert!(Session::open(r.path(), "nope").is_err());
    }
}
