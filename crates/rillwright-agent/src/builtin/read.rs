//! Read-only tools: `list_dir`, `read_file` and `search_files`.

use std::{fs, path::Path};

use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use rillwright_engine::chat::ToolDef;

use super::{
    MAX_FILE_BYTES, MAX_OUTPUT_BYTES, binary_type, cut_bytes, def, looks_binary, parse_args, shown,
};
use crate::{
    tool::{FileLog, Risk, Tool, ToolCtx, ToolOutcome},
    workspace::Access,
};

const LIST_MAX_ENTRIES: usize = 500;
const LIST_MAX_DEPTH: usize = 5;
const READ_DEFAULT_LINES: usize = 400;
const READ_MAX_LINES: usize = 2000;
const LINE_CLIP_CHARS: usize = 2000;
const SEARCH_DEFAULT_RESULTS: usize = 100;
const SEARCH_MAX_RESULTS: usize = 500;
const SEARCH_LINE_CLIP_CHARS: usize = 200;

pub fn tools() -> Vec<Box<dyn Tool>> {
    vec![Box::new(ListDir), Box::new(ReadFile), Box::new(SearchFiles)]
}

/// Walks a directory the way `rg` does: `.gitignore` is respected, hidden files are shown, and
/// `.git` itself never is.
fn walker(root: &Path) -> WalkBuilder {
    let mut b = WalkBuilder::new(root);
    // `require_git(false)`: .gitignore applies in any folder, not only inside a git repository.
    b.standard_filters(true)
        .require_git(false)
        .hidden(false)
        .sort_by_file_name(|a, b| a.cmp(b));
    b.filter_entry(|e| e.file_name() != ".git");
    b
}

pub struct ListDir;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: Option<String>,
    depth: Option<usize>,
}

impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn def(&self) -> ToolDef {
        def(
            "list_dir",
            "List files and folders in a directory of the project. Folders end with /. Respects .gitignore.",
            json!({
                "path": {"type": "string", "description": "Directory, relative to the project root. Default: the root."},
                "depth": {"type": "integer", "description": "How many levels to show, 1 to 5. Default 1."}
            }),
            &[],
        )
    }

    fn risk(&self, _args: &Map<String, Value>, _ctx: &ToolCtx) -> Risk {
        Risk::Read
    }

    fn run(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> ToolOutcome {
        let a: ListArgs = match parse_args("list_dir", args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let raw = a.path.unwrap_or_default();
        let dir = match ctx.workspace.resolve(shown(&raw), Access::Read) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        if !dir.is_dir() {
            return ToolOutcome::err(format!(
                "'{}' is not a directory; use read_file for files",
                shown(&raw)
            ));
        }
        let depth = a.depth.unwrap_or(1).clamp(1, LIST_MAX_DEPTH);
        let mut lines = Vec::new();
        let mut total = 0usize;
        for entry in walker(&dir).max_depth(Some(depth)).build().flatten() {
            if entry.depth() == 0 {
                continue;
            }
            total += 1;
            if lines.len() < LIST_MAX_ENTRIES {
                let rel = entry
                    .path()
                    .strip_prefix(&dir)
                    .unwrap_or(entry.path())
                    .display()
                    .to_string();
                let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
                lines.push(if is_dir { format!("{rel}/") } else { rel });
            }
        }
        if lines.is_empty() {
            return ToolOutcome::ok("(empty)");
        }
        let mut out = lines.join("\n");
        let mut truncated = false;
        if total > lines.len() {
            out.push_str(&format!(
                "\n[... {} more entries not shown; list a subdirectory or lower depth]",
                total - lines.len()
            ));
            truncated = true;
        }
        ToolOutcome {
            text: out,
            is_error: false,
            truncated,
        }
    }
}

pub struct ReadFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    start_line: Option<usize>,
    max_lines: Option<usize>,
}

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn def(&self) -> ToolDef {
        def(
            "read_file",
            "Read a text file of the project, with line numbers. Long files are read in parts with start_line.",
            json!({
                "path": {"type": "string", "description": "File path, relative to the project root."},
                "start_line": {"type": "integer", "description": "First line to show, starting at 1. Default 1."},
                "max_lines": {"type": "integer", "description": "How many lines to show, up to 2000. Default 400."}
            }),
            &["path"],
        )
    }

    fn risk(&self, _args: &Map<String, Value>, _ctx: &ToolCtx) -> Risk {
        Risk::Read
    }

    fn run(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> ToolOutcome {
        let a: ReadArgs = match parse_args("read_file", args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = match ctx.workspace.resolve(&a.path, Access::Read) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        read_lines(&path, &a, &mut ctx.files.borrow_mut())
    }
}

fn read_lines(path: &Path, a: &ReadArgs, files: &mut FileLog) -> ToolOutcome {
    if path.is_dir() {
        return ToolOutcome::err(format!("'{}' is a directory; use list_dir", a.path));
    }
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return ToolOutcome::err(format!("cannot read '{}': {e}", a.path)),
    };
    if meta.len() > MAX_FILE_BYTES {
        return ToolOutcome::err(format!(
            "'{}' is {} bytes, more than the {} byte limit for read_file",
            a.path,
            meta.len(),
            MAX_FILE_BYTES
        ));
    }
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => return ToolOutcome::err(format!("cannot read '{}': {e}", a.path)),
    };
    files.record(path, &bytes);
    if looks_binary(&bytes) {
        return ToolOutcome::err(format!(
            "'{}' is a binary file ({}, {} bytes); it cannot be shown as text",
            a.path,
            binary_type(&bytes),
            bytes.len()
        ));
    }

    let mut notes = Vec::new();
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            notes.push("[note: not valid UTF-8; invalid bytes are shown as U+FFFD]".to_string());
            String::from_utf8_lossy(e.as_bytes()).into_owned()
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return ToolOutcome {
            text: "[empty file]".into(),
            is_error: false,
            truncated: false,
        };
    }

    let start = a.start_line.unwrap_or(1).max(1);
    if start > total {
        return ToolOutcome::err(format!(
            "start_line {start} is past the end; '{}' has {total} lines",
            a.path
        ));
    }
    let count = a
        .max_lines
        .unwrap_or(READ_DEFAULT_LINES)
        .clamp(1, READ_MAX_LINES);

    let mut out = String::new();
    let mut shown_to = start - 1;
    for (i, line) in lines.iter().enumerate().skip(start - 1).take(count) {
        let clipped = if line.chars().count() > LINE_CLIP_CHARS {
            format!(
                "{} [line clipped]",
                line.chars().take(LINE_CLIP_CHARS).collect::<String>()
            )
        } else {
            line.to_string()
        };
        let row = format!("{:>6}\t{clipped}\n", i + 1);
        if out.len() + row.len() > MAX_OUTPUT_BYTES {
            break;
        }
        out.push_str(&row);
        shown_to = i + 1;
    }
    let truncated = shown_to < total;
    if shown_to < total {
        notes.push(format!(
            "[showing lines {start}-{shown_to} of {total}. Read the rest with start_line={}.]",
            shown_to + 1
        ));
    } else {
        notes.push(format!("[{total} lines in total]"));
    }
    let mut text = out.trim_end_matches('\n').to_string();
    text.push('\n');
    text.push_str(&notes.join("\n"));
    let cut = cut_bytes(&mut text, MAX_OUTPUT_BYTES + 512);
    ToolOutcome {
        text,
        is_error: false,
        truncated: truncated || cut,
    }
}

pub struct SearchFiles;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    max_results: Option<usize>,
}

impl Tool for SearchFiles {
    fn name(&self) -> &str {
        "search_files"
    }

    fn def(&self) -> ToolDef {
        def(
            "search_files",
            "Search the project's text files for a regular expression. Returns path:line:text. Respects .gitignore.",
            json!({
                "pattern": {"type": "string", "description": "Regular expression to search for."},
                "path": {"type": "string", "description": "Directory or file to search. Default: the project root."},
                "glob": {"type": "string", "description": "Only search files whose name or path matches, e.g. *.rs."},
                "max_results": {"type": "integer", "description": "Most matches to return, up to 500. Default 100."}
            }),
            &["pattern"],
        )
    }

    fn risk(&self, _args: &Map<String, Value>, _ctx: &ToolCtx) -> Risk {
        Risk::Read
    }

    fn run(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> ToolOutcome {
        let a: SearchArgs = match parse_args("search_files", args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let re = match regex::Regex::new(&a.pattern) {
            Ok(re) => re,
            Err(e) => return ToolOutcome::err(format!("invalid regular expression: {e}")),
        };
        let glob = match a
            .glob
            .as_deref()
            .map(|g| globset::Glob::new(g).map(|g| g.compile_matcher()))
        {
            None => None,
            Some(Ok(m)) => Some(m),
            Some(Err(e)) => return ToolOutcome::err(format!("invalid glob: {e}")),
        };
        let raw = a.path.unwrap_or_default();
        let root = match ctx.workspace.resolve(shown(&raw), Access::Read) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        let limit = a
            .max_results
            .unwrap_or(SEARCH_DEFAULT_RESULTS)
            .clamp(1, SEARCH_MAX_RESULTS);

        let mut hits: Vec<String> = Vec::new();
        let mut total = 0usize;
        let files = if root.is_file() {
            vec![root.clone()]
        } else {
            walker(&root)
                .build()
                .flatten()
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .map(|e| e.into_path())
                .collect()
        };
        for file in files {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(&file)
                .display()
                .to_string();
            if let Some(g) = &glob {
                let name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !g.is_match(&rel) && !g.is_match(&name) {
                    continue;
                }
            }
            // Secret files are never searched: a match would show their contents.
            if ctx.workspace.hides_from_reads(&file) {
                continue;
            }
            let Ok(meta) = fs::metadata(&file) else {
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(&file) else { continue };
            if looks_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let shown_path = ctx.workspace.relative(&file);
            for (i, line) in text.lines().enumerate() {
                if !re.is_match(line) {
                    continue;
                }
                total += 1;
                if hits.len() < limit {
                    let clipped: String = line.chars().take(SEARCH_LINE_CLIP_CHARS).collect();
                    hits.push(format!("{shown_path}:{}:{clipped}", i + 1));
                }
            }
        }
        if hits.is_empty() {
            return ToolOutcome::ok("no matches");
        }
        let mut out = hits.join("\n");
        let truncated = total > hits.len();
        if truncated {
            out.push_str(&format!(
                "\n[{} more matches not shown; narrow the pattern or glob]",
                total - hits.len()
            ));
        }
        let cut = cut_bytes(&mut out, MAX_OUTPUT_BYTES);
        ToolOutcome {
            text: out,
            is_error: false,
            truncated: truncated || cut,
        }
    }
}
