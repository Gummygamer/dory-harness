//! Runs one external process with bounded time, bounded output and no deadlock.
//!
//! - The child gets its own process group, so a timeout or a leftover background job can be
//!   killed together with everything it started.
//! - Stdin is written from its own thread, so a child that never reads its input cannot block us.
//! - Stdout and stderr are read on their own threads and each keeps at most `limit` bytes: the
//!   first and last halves, with a marker between them.
//! - After the child exits, its pipes are drained for at most [`DRAIN`]. If a grandchild still
//!   holds them, the group is killed and the pipes are abandoned.

use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

/// How long to wait for the pipes to close after the child has exited.
pub const DRAIN: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 8 * 1024;
const POLL: Duration = Duration::from_millis(10);
/// After the group is killed, how long to wait for the pipes to close.
const KILL_GRACE: Duration = Duration::from_millis(500);

pub struct ProcessSpec<'a> {
    pub argv: &'a [String],
    pub stdin: &'a [u8],
    pub cwd: Option<&'a Path>,
    /// `None` inherits the environment. `Some` clears it and sets exactly these variables.
    pub env: Option<Vec<(String, String)>>,
    pub timeout: Duration,
    /// Maximum bytes kept per stream.
    pub limit: usize,
}

#[derive(Debug, Default, PartialEq)]
pub struct ProcessResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    /// Either stream dropped bytes to fit the limit.
    pub truncated: bool,
    /// A grandchild held the pipes after the child exited, so the group was killed.
    pub orphans_killed: bool,
    /// Pipes that never closed in time are abandoned; their output is missing.
    pub pipes_abandoned: bool,
}

impl ProcessResult {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
}

/// Keeps the first `limit/2` and the last `limit - limit/2` bytes of a stream.
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    head_cap: usize,
    tail_cap: usize,
    total: usize,
}

impl Capture {
    fn new(limit: usize) -> Self {
        let head_cap = limit / 2;
        Capture { head: Vec::new(), tail: VecDeque::new(), head_cap, tail_cap: limit - head_cap, total: 0 }
    }

    fn push(&mut self, mut data: &[u8]) {
        self.total += data.len();
        if self.head.len() < self.head_cap {
            let take = (self.head_cap - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..take]);
            data = &data[take..];
        }
        if data.is_empty() {
            return;
        }
        self.tail.extend(data);
        if self.tail.len() > self.tail_cap {
            let excess = self.tail.len() - self.tail_cap;
            self.tail.drain(..excess);
        }
    }

    /// Returns the kept bytes and whether anything was dropped.
    fn finish(self) -> (Vec<u8>, bool) {
        let kept = self.head.len() + self.tail.len();
        if self.total == kept {
            let mut out = self.head;
            out.extend(self.tail);
            return (out, false);
        }
        let omitted = self.total - kept;
        let mut out = self.head;
        out.extend(format!("\n[... {omitted} bytes omitted ...]\n").as_bytes());
        out.extend(self.tail);
        (out, true)
    }
}

fn read_stream<R: Read + Send + 'static>(mut src: R, limit: usize) -> Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut cap = Capture::new(limit);
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => cap.push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = tx.send(cap.finish());
    });
    rx
}

/// SIGKILL to the whole process group whose id is `pid` (the child, since it leads its group).
fn kill_group(pid: u32) {
    // Safety: plain syscall. A negative pid addresses the process group; ESRCH is ignored.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

fn receive(rx: &Receiver<(Vec<u8>, bool)>, until: Instant) -> Option<(Vec<u8>, bool)> {
    rx.recv_timeout(until.saturating_duration_since(Instant::now())).ok()
}

pub fn run_process(spec: &ProcessSpec) -> io::Result<ProcessResult> {
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(dir) = spec.cwd {
        cmd.current_dir(dir);
    }
    if let Some(env) = &spec.env {
        cmd.env_clear();
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();

    if let Some(mut stdin) = child.stdin.take() {
        let input = spec.stdin.to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let stdout_rx = read_stream(child.stdout.take().expect("stdout is piped"), spec.limit);
    let stderr_rx = read_stream(child.stderr.take().expect("stderr is piped"), spec.limit);

    let start = Instant::now();
    let mut result = ProcessResult::default();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= spec.timeout {
            kill_group(pid);
            result.timed_out = true;
            break child.wait()?;
        }
        thread::sleep(POLL);
    };
    result.code = status.code();
    result.signal = status.signal();

    let drain_until = Instant::now() + DRAIN;
    let mut out = receive(&stdout_rx, drain_until);
    let mut err = receive(&stderr_rx, drain_until);
    if out.is_none() || err.is_none() {
        // Killing the group closes the pipes, so a short wait recovers what was written so far.
        kill_group(pid);
        result.orphans_killed = true;
        let grace = Instant::now() + KILL_GRACE;
        if out.is_none() {
            out = receive(&stdout_rx, grace);
        }
        if err.is_none() {
            err = receive(&stderr_rx, grace);
        }
        result.pipes_abandoned = out.is_none() || err.is_none();
    }
    let mut truncated = false;
    if let Some((bytes, t)) = out {
        result.stdout = bytes;
        truncated |= t;
    }
    if let Some((bytes, t)) = err {
        result.stderr = bytes;
        truncated |= t;
    }
    result.truncated = truncated;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn run(parts: &[&str], stdin: &[u8], timeout: Duration, limit: usize) -> ProcessResult {
        let a = argv(parts);
        run_process(&ProcessSpec { argv: &a, stdin, cwd: None, env: None, timeout, limit }).unwrap()
    }

    #[test]
    fn captures_stdout_and_exit_code() {
        let r = run(&["sh", "-c", "echo hi; exit 0"], b"", Duration::from_secs(5), 1024);
        assert_eq!(r.stdout, b"hi\n");
        assert_eq!(r.code, Some(0));
        assert!(r.success());
    }

    #[test]
    fn non_zero_exit_is_reported() {
        let r = run(&["sh", "-c", "exit 3"], b"", Duration::from_secs(5), 1024);
        assert_eq!(r.code, Some(3));
        assert!(!r.success());
    }

    #[test]
    fn stderr_is_captured_separately() {
        let r = run(&["sh", "-c", "echo oops >&2"], b"", Duration::from_secs(5), 1024);
        assert_eq!(r.stderr, b"oops\n");
        assert!(r.stdout.is_empty());
    }

    #[test]
    fn large_stdin_to_a_child_that_never_reads_does_not_block() {
        let input = vec![b'x'; 1 << 20];
        let start = Instant::now();
        let r = run(&["sleep", "0.3"], &input, Duration::from_secs(10), 1024);
        assert_eq!(r.code, Some(0));
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
    }

    #[test]
    fn background_grandchild_cannot_hold_the_harness() {
        let start = Instant::now();
        let r = run(&["sh", "-c", "sleep 100 & echo done"], b"", Duration::from_secs(10), 1024);
        assert_eq!(r.stdout, b"done\n");
        assert!(r.orphans_killed, "{r:?}");
        assert!(start.elapsed() < Duration::from_secs(6), "took {:?}", start.elapsed());
    }

    #[test]
    fn timeout_kills_the_whole_group() {
        let start = Instant::now();
        // The background job's pid is printed so the test can check it died.
        let r = run(&["sh", "-c", "sleep 100 & echo $!; sleep 100"], b"", Duration::from_millis(500), 1024);
        assert!(r.timed_out, "{r:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
        let grandchild: i32 = String::from_utf8_lossy(&r.stdout).trim().parse().expect("pid");
        // A killed process stays a zombie until its parent reaps it, so "alive" means not gone and not Z.
        let stat = std::fs::read_to_string(format!("/proc/{grandchild}/stat")).unwrap_or_default();
        let state = stat.rsplit(')').next().unwrap_or("").trim().chars().next();
        assert!(matches!(state, None | Some('Z')), "grandchild {grandchild} survived the timeout (state {state:?})");
    }

    #[test]
    fn output_is_bounded_with_a_marker() {
        let r = run(&["sh", "-c", "head -c 10485760 /dev/zero | tr '\\0' a"], b"", Duration::from_secs(20), 16 * 1024);
        assert!(r.truncated);
        assert!(r.stdout.len() <= 16 * 1024 + 64, "kept {} bytes", r.stdout.len());
        let text = String::from_utf8_lossy(&r.stdout);
        assert!(text.contains("bytes omitted"), "no marker in output");
        assert!(text.starts_with('a') && text.ends_with('a'));
    }

    #[test]
    fn small_output_is_not_marked_truncated() {
        let r = run(&["sh", "-c", "printf 0123456789"], b"", Duration::from_secs(5), 1024);
        assert_eq!(r.stdout, b"0123456789");
        assert!(!r.truncated);
    }

    #[test]
    fn capture_keeps_head_and_tail_of_a_stream_in_pieces() {
        let mut cap = Capture::new(10);
        for chunk in ["abc", "defgh", "ijklmno"] {
            cap.push(chunk.as_bytes());
        }
        let (out, truncated) = cap.finish();
        assert!(truncated);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("abcde") && text.ends_with("klmno"), "{text:?}");
        assert!(text.contains("[... 5 bytes omitted ...]"), "{text:?}");
    }

    #[test]
    fn env_is_cleared_when_requested() {
        let a = argv(&["sh", "-c", "echo ${SECRET_FOR_TEST:-unset}"]);
        let r = run_process(&ProcessSpec {
            argv: &a,
            stdin: b"",
            cwd: None,
            env: Some(vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]),
            timeout: Duration::from_secs(5),
            limit: 1024,
        })
        .unwrap();
        assert_eq!(r.stdout, b"unset\n");
    }
}
