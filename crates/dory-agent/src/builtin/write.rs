//! Write tools: `write_file` and `edit_file`.
//!
//! Both check the file against what the agent last read or wrote, so a change made by someone
//! else (or by a command) is never overwritten unseen. Both write through [`atomic_write`]: the
//! new content goes to a temporary file in the same folder, which then replaces the old file in
//! one rename. A failure leaves the old file as it was.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use similar::{ChangeTag, TextDiff};

use dory_engine::chat::ToolDef;

use super::{MAX_OUTPUT_BYTES, cut_bytes, def, parse_args};
use crate::{
    tool::{FileLog, Risk, Tool, ToolCtx, ToolOutcome, content_hash},
    workspace::Access,
};

/// Diffs shown back to the model are cut to this many bytes.
const DIFF_MAX_BYTES: usize = 4096;
/// How many lines to scan when looking for the closest line to a failed `old_text`.
const HINT_MAX_LINES: usize = 5000;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` by replacing it in one rename. Keeps the mode of an existing file.
/// On failure the temporary file is removed and `path` is untouched.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("the file has no parent directory"))?;
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.dory-tmp-{}-{n}", std::process::id()));
    let existing = fs::metadata(path).ok().map(|m| m.permissions());

    let result = (|| {
        // create_new never reuses a file that is already there.
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_data()?;
        drop(f);
        if let Some(perms) = existing {
            fs::set_permissions(&tmp, perms)?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The check every write does first: a file that exists must be the one the agent last saw.
fn check_seen(files: &FileLog, path: &Path, shown: &str, bytes: &[u8]) -> Result<(), ToolOutcome> {
    match files.seen_hash(path) {
        None => Err(ToolOutcome::err(format!(
            "'{shown}' exists but was not read in this session. Read it first, so its current content is known."
        ))),
        Some(h) if h != content_hash(bytes) => Err(ToolOutcome::err(format!(
            "'{shown}' changed since you read it. Read it again before changing it."
        ))),
        Some(_) => Ok(()),
    }
}

/// A unified diff and the number of changed lines.
fn diff(old: &str, new: &str, shown: &str) -> (String, usize) {
    let d = TextDiff::from_lines(old, new);
    let changed = d
        .iter_all_changes()
        .filter(|c| c.tag() != ChangeTag::Equal)
        .count();
    let mut text = d
        .unified_diff()
        .context_radius(2)
        .header(&format!("a/{shown}"), &format!("b/{shown}"))
        .to_string();
    if cut_bytes(&mut text, DIFF_MAX_BYTES) {
        text.push_str("\n[diff cut short]");
    }
    (text, changed)
}

pub fn tools() -> Vec<Box<dyn Tool>> {
    vec![Box::new(WriteFile), Box::new(EditFile)]
}

pub struct WriteFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn def(&self) -> ToolDef {
        def(
            "write_file",
            "Create a file, or replace an existing one with the full new content. An existing file must be read first in this session. Parent folders are created.",
            json!({
                "path": {"type": "string", "description": "File path, relative to the project root."},
                "content": {"type": "string", "description": "The complete new content of the file."}
            }),
            &["path", "content"],
        )
    }

    fn risk(&self, _args: &Map<String, Value>, _ctx: &ToolCtx) -> Risk {
        Risk::Write
    }

    fn summary(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> String {
        match parse_args::<WriteArgs>("write_file", args) {
            Ok(a) => {
                let exists = ctx
                    .workspace
                    .resolve(&a.path, Access::Write)
                    .map(|p| p.exists())
                    .unwrap_or(false);
                let mut preview = a.content.clone();
                let truncated = cut_bytes(&mut preview, 4096);
                format!(
                    "{} {} ({} lines, {} bytes)",
                    if exists { "overwrite" } else { "create" },
                    a.path,
                    a.content.lines().count(),
                    a.content.len()
                ) + &format!(
                    "\nProposed content preview:\n{preview}{}",
                    if truncated {
                        "\n[preview truncated]"
                    } else {
                        ""
                    }
                )
            }
            Err(outcome) => outcome.text,
        }
    }

    fn run(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> ToolOutcome {
        let a: WriteArgs = match parse_args("write_file", args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = match ctx.workspace.resolve(&a.path, Access::Write) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        if path.is_dir() {
            return ToolOutcome::err(format!("'{}' is a directory", a.path));
        }
        let old = if path.exists() {
            let old_bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => return ToolOutcome::err(format!("cannot read '{}': {e}", a.path)),
            };
            if let Err(e) = check_seen(&ctx.files.borrow(), &path, &a.path, &old_bytes) {
                return e;
            }
            Some(String::from_utf8_lossy(&old_bytes).into_owned())
        } else {
            None
        };
        if let Err(e) = atomic_write(&path, a.content.as_bytes()) {
            return ToolOutcome::err(format!("cannot write '{}': {e}", a.path));
        }
        ctx.files.borrow_mut().record(&path, a.content.as_bytes());

        let lines = a.content.lines().count();
        match old {
            None => ToolOutcome::ok(format!(
                "created {} ({lines} lines, {} bytes)",
                a.path,
                a.content.len()
            )),
            Some(old) => {
                let (d, changed) = diff(&old, &a.content, &a.path);
                let mut out = format!("overwrote {} ({lines} lines, {changed} changed)", a.path);
                if changed > 0 {
                    out.push('\n');
                    out.push_str(&d);
                }
                let cut = cut_bytes(&mut out, MAX_OUTPUT_BYTES);
                ToolOutcome {
                    text: out,
                    is_error: false,
                    truncated: cut,
                }
            }
        }
    }
}

pub struct EditFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    old_text: String,
    new_text: String,
    replace_all: Option<bool>,
}

impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn def(&self) -> ToolDef {
        def(
            "edit_file",
            "Replace exact text in a file that was read in this session. old_text must match exactly once, unless replace_all is true. Copy old_text from read_file output.",
            json!({
                "path": {"type": "string", "description": "File path, relative to the project root."},
                "old_text": {"type": "string", "description": "The exact text to replace, including its indentation."},
                "new_text": {"type": "string", "description": "What to put in its place."},
                "replace_all": {"type": "boolean", "description": "Replace every match, not only a single one. Default false."}
            }),
            &["path", "old_text", "new_text"],
        )
    }

    fn risk(&self, _args: &Map<String, Value>, _ctx: &ToolCtx) -> Risk {
        Risk::Write
    }

    fn summary(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> String {
        match plan_edit(args, ctx) {
            Ok(plan) => {
                let (d, changed) = diff(&plan.text, &plan.updated, &plan.path_shown);
                format!(
                    "edit {} ({} replacement(s), {changed} line(s) changed)\n{d}",
                    plan.path_shown, plan.count_applied
                )
            }
            Err(outcome) => outcome.text,
        }
    }

    fn run(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> ToolOutcome {
        let plan = match plan_edit(args, ctx) {
            Ok(p) => p,
            Err(outcome) => return outcome,
        };
        if let Err(e) = atomic_write(&plan.path, plan.updated.as_bytes()) {
            return ToolOutcome::err(format!("cannot write '{}': {e}", plan.path_shown));
        }
        ctx.files
            .borrow_mut()
            .record(&plan.path, plan.updated.as_bytes());

        let (d, changed) = diff(&plan.text, &plan.updated, &plan.path_shown);
        let mut out = format!(
            "edited {}: {} replacement(s), {changed} line(s) changed",
            plan.path_shown, plan.count_applied
        );
        if changed > 0 {
            out.push('\n');
            out.push_str(&d);
        }
        let cut = cut_bytes(&mut out, MAX_OUTPUT_BYTES);
        ToolOutcome {
            text: out,
            is_error: false,
            truncated: cut,
        }
    }
}

/// An edit that has been checked and can be applied, or shown for approval, without writing.
struct EditPlan {
    path: std::path::PathBuf,
    path_shown: String,
    /// The file as it is now.
    text: String,
    updated: String,
    count_applied: usize,
}

/// Checks an `edit_file` call against the file. Every refusal is returned as the outcome to send.
fn plan_edit(args: &Map<String, Value>, ctx: &ToolCtx) -> Result<EditPlan, ToolOutcome> {
    let a: EditArgs = parse_args("edit_file", args)?;
    let path = ctx
        .workspace
        .resolve(&a.path, Access::Write)
        .map_err(|e| ToolOutcome::err(e.to_string()))?;
    if !path.is_file() {
        return Err(ToolOutcome::err(format!(
            "'{}' does not exist; use write_file to create it",
            a.path
        )));
    }
    let bytes =
        fs::read(&path).map_err(|e| ToolOutcome::err(format!("cannot read '{}': {e}", a.path)))?;
    check_seen(&ctx.files.borrow(), &path, &a.path, &bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| {
        ToolOutcome::err(format!(
            "'{}' is not valid UTF-8; edit_file only edits text",
            a.path
        ))
    })?;
    if a.old_text.is_empty() {
        return Err(ToolOutcome::err("old_text is empty; say what to replace"));
    }

    // Files with CRLF line endings keep them: the model's text is matched and written in the same ending.
    let crlf = text.contains("\r\n");
    let (old, new) = if crlf {
        (to_crlf(&a.old_text), to_crlf(&a.new_text))
    } else {
        (a.old_text.clone(), a.new_text.clone())
    };

    let count = text.matches(&old).count();
    if count == 0 {
        let hint = closest_line(&text, &a.old_text)
            .map(|(n, line)| format!(" The closest line is {n}: {line:?}"))
            .unwrap_or_default();
        return Err(ToolOutcome::err(format!(
            "old_text was not found in '{}'.{hint}",
            a.path
        )));
    }
    let replace_all = a.replace_all.unwrap_or(false);
    if count > 1 && !replace_all {
        return Err(ToolOutcome::err(format!(
            "old_text matches {count} places in '{}'. Include more surrounding lines so it is unique, or set replace_all to true.",
            a.path
        )));
    }
    let updated = if replace_all {
        text.replace(&old, &new)
    } else {
        text.replacen(&old, &new, 1)
    };
    Ok(EditPlan {
        path,
        path_shown: a.path,
        text,
        updated,
        count_applied: if replace_all { count } else { 1 },
    })
}

fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// The line of `text` most like the first line of `wanted`, with its number. Used to tell the model
/// where a failed `old_text` nearly matched.
fn closest_line(text: &str, wanted: &str) -> Option<(usize, String)> {
    let target = wanted.lines().find(|l| !l.trim().is_empty())?.trim();
    text.lines()
        .take(HINT_MAX_LINES)
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| (i + 1, l, TextDiff::from_chars(target, l.trim()).ratio()))
        .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        .filter(|(_, _, score)| *score >= 0.5)
        .map(|(n, l, _)| (n, l.to_string()))
}
