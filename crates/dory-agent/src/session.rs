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
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
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
}

fn new_id() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{secs}-{}", std::process::id())
}

fn session_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".dory").join("sessions").join(id)
}

impl Session {
    /// Starts a new session. `header` holds the run settings and is the first line.
    pub fn create(root: &Path, header: Value) -> io::Result<Session> {
        let id = new_id();
        let dir = session_dir(root, &id);
        fs::create_dir_all(&dir)?;
        let file = OpenOptions::new().create_new(true).append(true).open(dir.join("transcript.jsonl"))?;
        let mut s = Session { id, dir, file };
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
        let dir = session_dir(root, id);
        let path = dir.join("transcript.jsonl");
        let reader = BufReader::new(File::open(&path)?);
        let mut header = None;
        let mut messages = Vec::new();
        for line in reader.lines() {
            let line = line?;
            // A line cut short by a crash does not parse. It is the one being written, so skip it.
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            match v.get("type").and_then(Value::as_str) {
                Some("session") => header = Some(v),
                Some("message") => {
                    if let Some(m) = v.get("message").and_then(|m| serde_json::from_value::<Message>(m.clone()).ok()) {
                        messages.push(m);
                    }
                }
                _ => {}
            }
        }
        let header = header.ok_or_else(|| io::Error::other(format!("{} has no session header", path.display())))?;
        if header.get("version").and_then(Value::as_u64) != Some(TRANSCRIPT_VERSION) {
            return Err(io::Error::other(format!("{} has an unsupported transcript version", path.display())));
        }
        repair(&mut messages);
        let file = OpenOptions::new().append(true).open(&path)?;
        let session = Session { id: id.to_string(), dir, file };
        Ok((session, messages, header))
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
            AgentEvent::MessageAdded(m) => self.write_line(&json!({"type": "message", "message": m})),
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

/// Gives every tool call that has no result yet an "outcome unknown" result. Returns how many were
/// added. Only the last assistant message can be unfinished, so only it is checked.
pub fn repair(messages: &mut Vec<Message>) -> usize {
    let Some(last_assistant) = messages.iter().rposition(|m| m.role == "assistant") else { return 0 };
    let calls = messages[last_assistant].tool_calls.len();
    let answered = messages[last_assistant + 1..].iter().take_while(|m| m.role == "tool").count();
    let missing = calls.saturating_sub(answered);
    for _ in 0..missing {
        messages.push(Message::new("tool", UNKNOWN_OUTCOME));
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use dory_engine::chat::ToolCall;
    use dory_engine::GenStats;

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
        s.record(&AgentEvent::MessageAdded(Message::new("user", "task"))).unwrap();
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
        s.record(&AgentEvent::MessageAdded(Message::new("assistant", "answer"))).unwrap();
        s.finish(Reason::Answered).unwrap();
        drop(s);

        let (_s, messages, h) = Session::open(r.path(), &id).unwrap();
        assert_eq!(messages, vec![Message::new("user", "task"), Message::new("assistant", "answer")]);
        assert_eq!(h["model"], "dory");
    }

    #[test]
    fn a_line_cut_short_by_a_crash_is_skipped() {
        let r = root();
        let mut s = Session::create(r.path(), header()).unwrap();
        let id = s.id.clone();
        s.record(&AgentEvent::MessageAdded(Message::new("user", "task"))).unwrap();
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
                tool_calls: vec![ToolCall { name: "write_file".into(), arguments: Default::default() }, ToolCall { name: "run_command".into(), arguments: Default::default() }],
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
        let mut messages = vec![Message::new("user", "task"), Message::new("assistant", "done")];
        assert_eq!(repair(&mut messages), 0);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn a_missing_session_is_an_error() {
        let r = root();
        assert!(Session::open(r.path(), "nope").is_err());
    }
}
