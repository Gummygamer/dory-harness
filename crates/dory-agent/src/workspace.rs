//! Confines the file tools to one directory tree.
//!
//! A model-supplied path is normalized textually first (so `..` is never resolved by the OS
//! through a symlink), then the longest existing prefix is canonicalized, so symlinks are
//! followed. The result must be inside the workspace root. Nothing that does not exist yet is
//! followed, so new files in new directories work.
//!
//! This guards against mistakes by the model, not against a hostile process running alongside:
//! the check and the later open are separate steps, and a racing process could change the tree
//! between them. Commands are confined separately, by the sandbox.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use globset::{Glob, GlobMatcher};

/// Directories the agent may read but never write.
const PROTECTED_DIRS: &[&str] = &[".git", ".dory"];

/// Files the agent may neither read nor write unless `--allow-path` names them.
const SECRET_GLOBS: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    "**/id_rsa*",
    "**/.ssh/**",
    "**/.aws/**",
    "**/.gnupg/**",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PathError {
    Nul,
    Outside(String),
    Protected(String),
    Secret(String),
    NotADirectory(String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Nul => write!(f, "the path contains a NUL byte"),
            PathError::Outside(p) => write!(f, "'{p}' is outside the workspace"),
            PathError::Protected(p) => write!(f, "'{p}' is inside a protected directory and cannot be written"),
            PathError::Secret(p) => write!(f, "'{p}' looks like a secret and is not accessible (see --allow-path)"),
            PathError::NotADirectory(p) => write!(f, "'{p}' is not a directory"),
        }
    }
}

impl std::error::Error for PathError {}

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    secret_patterns: Vec<GlobMatcher>,
    allow_patterns: Vec<GlobMatcher>,
}

impl Workspace {
    /// Opens a workspace at `root`. The root must be an existing directory.
    pub fn new(root: &Path, allow_paths: &[String]) -> Result<Workspace, PathError> {
        let root = fs::canonicalize(root).map_err(|_| PathError::NotADirectory(root.display().to_string()))?;
        if !root.is_dir() {
            return Err(PathError::NotADirectory(root.display().to_string()));
        }
        Ok(Workspace {
            root,
            secret_patterns: compile(SECRET_GLOBS.iter().copied()),
            allow_patterns: compile(allow_paths.iter().map(String::as_str)),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a model-supplied path to an absolute path inside the workspace.
    pub fn resolve(&self, path: &str, access: Access) -> Result<PathBuf, PathError> {
        if path.contains('\0') {
            return Err(PathError::Nul);
        }
        let joined = if Path::new(path).is_absolute() { PathBuf::from(path) } else { self.root.join(path) };
        let normal = normalize_lexically(&joined);
        let shown = path.to_string();

        // Canonicalize the longest prefix that exists. Symlinks in it are followed here, and must
        // still land inside the root.
        let mut existing = normal.as_path();
        while fs::symlink_metadata(existing).is_err() {
            match existing.parent() {
                Some(parent) => existing = parent,
                None => return Err(PathError::Outside(shown)),
            }
        }
        let canonical_prefix = fs::canonicalize(existing).map_err(|_| PathError::Outside(shown.clone()))?;
        if !canonical_prefix.starts_with(&self.root) {
            return Err(PathError::Outside(shown));
        }
        let rest = normal.strip_prefix(existing).unwrap_or(Path::new(""));
        // `join("")` would add a trailing slash, which makes a file path a directory path.
        let resolved = if rest.as_os_str().is_empty() { canonical_prefix } else { canonical_prefix.join(rest) };
        if !resolved.starts_with(&self.root) {
            return Err(PathError::Outside(shown));
        }

        let relative = resolved.strip_prefix(&self.root).unwrap_or(Path::new(""));
        if !self.allowed_by_user(relative) {
            if self.is_secret(relative) {
                return Err(PathError::Secret(shown));
            }
            if access == Access::Write && is_protected(relative) {
                return Err(PathError::Protected(shown));
            }
        }
        Ok(resolved)
    }

    /// The path relative to the root, for display.
    pub fn relative(&self, abs: &Path) -> String {
        abs.strip_prefix(&self.root).unwrap_or(abs).display().to_string()
    }

    fn is_secret(&self, relative: &Path) -> bool {
        self.secret_patterns.iter().any(|g| g.is_match(relative))
    }

    fn allowed_by_user(&self, relative: &Path) -> bool {
        self.allow_patterns.iter().any(|g| g.is_match(relative))
    }
}

fn is_protected(relative: &Path) -> bool {
    relative.components().any(|c| matches!(c, Component::Normal(n) if PROTECTED_DIRS.iter().any(|p| n == *p)))
}

fn compile<'a>(patterns: impl Iterator<Item = &'a str>) -> Vec<GlobMatcher> {
    patterns.filter_map(|p| Glob::new(p).ok().map(|g| g.compile_matcher())).collect()
}

/// Removes `.` and resolves `..` by text alone. Used only before the filesystem is consulted.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn a() {}\n").unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let ws = Workspace::new(dir.path(), &[]).unwrap();
        (dir, ws)
    }

    #[test]
    fn relative_paths_inside_the_root_resolve() {
        let (_d, ws) = fixture();
        let p = ws.resolve("src/lib.rs", Access::Read).unwrap();
        assert!(p.starts_with(ws.root()));
        assert!(p.ends_with("src/lib.rs"));
    }

    #[test]
    fn new_files_in_new_directories_are_allowed_for_writes() {
        let (_d, ws) = fixture();
        let p = ws.resolve("brand/new/dir/file.txt", Access::Write).unwrap();
        assert!(p.starts_with(ws.root()));
    }

    #[test]
    fn dot_dot_escape_is_refused() {
        let (_d, ws) = fixture();
        assert!(matches!(ws.resolve("../x", Access::Read), Err(PathError::Outside(_))));
        assert!(matches!(ws.resolve("src/../../x", Access::Read), Err(PathError::Outside(_))));
    }

    #[test]
    fn absolute_path_outside_the_root_is_refused() {
        let (_d, ws) = fixture();
        assert!(matches!(ws.resolve("/etc/passwd", Access::Read), Err(PathError::Outside(_))));
    }

    #[test]
    fn absolute_path_inside_the_root_is_allowed() {
        let (_d, ws) = fixture();
        let inside = ws.root().join("src/lib.rs");
        assert!(ws.resolve(inside.to_str().unwrap(), Access::Read).is_ok());
    }

    #[test]
    fn symlink_pointing_out_is_refused() {
        let (d, ws) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "x").unwrap();
        symlink(outside.path().join("secret.txt"), d.path().join("link.txt")).unwrap();
        assert!(matches!(ws.resolve("link.txt", Access::Read), Err(PathError::Outside(_))));
        assert!(matches!(ws.resolve("link.txt", Access::Write), Err(PathError::Outside(_))));
    }

    #[test]
    fn symlinked_parent_directory_pointing_out_is_refused() {
        let (d, ws) = fixture();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), d.path().join("out")).unwrap();
        assert!(matches!(ws.resolve("out/new.txt", Access::Write), Err(PathError::Outside(_))));
        // Lexical normalization removes `out/..` before the filesystem is consulted.
        assert!(ws.resolve("out/../src/lib.rs", Access::Read).is_ok());
    }

    #[test]
    fn symlink_inside_the_root_is_allowed() {
        let (d, ws) = fixture();
        symlink(d.path().join("src"), d.path().join("alias")).unwrap();
        assert!(ws.resolve("alias/lib.rs", Access::Read).is_ok());
    }

    #[test]
    fn nul_byte_is_refused() {
        let (_d, ws) = fixture();
        assert_eq!(ws.resolve("a\0b", Access::Read), Err(PathError::Nul));
    }

    #[test]
    fn writes_under_git_and_dory_are_refused_but_reads_are_not() {
        let (_d, ws) = fixture();
        assert!(matches!(ws.resolve(".git/config", Access::Write), Err(PathError::Protected(_))));
        assert!(matches!(ws.resolve(".dory/sessions/x.jsonl", Access::Write), Err(PathError::Protected(_))));
        assert!(ws.resolve(".git/config", Access::Read).is_ok());
    }

    #[test]
    fn secrets_are_refused_for_reads_and_writes() {
        let (d, ws) = fixture();
        fs::write(d.path().join(".env"), "KEY=1").unwrap();
        fs::write(d.path().join("src/deploy.pem"), "x").unwrap();
        for p in [".env", "src/deploy.pem", "new/.env.local"] {
            assert!(matches!(ws.resolve(p, Access::Read), Err(PathError::Secret(_))), "{p}");
            assert!(matches!(ws.resolve(p, Access::Write), Err(PathError::Secret(_))), "{p}");
        }
    }

    #[test]
    fn allow_path_overrides_the_secret_rule() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "KEY=1").unwrap();
        let ws = Workspace::new(dir.path(), &[".env".to_string()]).unwrap();
        assert!(ws.resolve(".env", Access::Read).is_ok());
    }

    #[test]
    fn a_missing_root_is_an_error() {
        assert!(Workspace::new(Path::new("/definitely/not/here/xyz"), &[]).is_err());
    }
}
