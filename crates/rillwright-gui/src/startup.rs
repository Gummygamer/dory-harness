use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub struct LaunchOptions {
    pub demo: bool,
    pub model: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSource {
    Argument,
    Environment,
    Saved,
    Picker,
    Missing,
}

impl ModelSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::Environment => "environment",
            Self::Saved => "saved preference",
            Self::Picker => "file picker",
            Self::Missing => "not selected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct StartupSelection {
    pub model: Option<PathBuf>,
    pub model_source: ModelSource,
    pub workspace: Option<PathBuf>,
    pub workspace_error: Option<String>,
}

pub fn parse_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<LaunchOptions, String> {
    let mut options = LaunchOptions::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--demo" {
            options.demo = true;
        } else if arg == "--version" || arg == "-V" {
            continue;
        } else if arg == "--model" || arg == "--workspace" {
            let value = args
                .next()
                .ok_or_else(|| format!("{} requires a path", arg.to_string_lossy()))?;
            if value.is_empty() {
                return Err(format!("{} path cannot be empty", arg.to_string_lossy()));
            }
            if arg == "--model" {
                options.model = Some(value.into());
            } else {
                options.workspace = Some(value.into());
            }
        } else if arg.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option: {}", arg.to_string_lossy()));
        } else {
            return Err(format!("unexpected argument: {}", arg.to_string_lossy()));
        }
    }
    Ok(options)
}

pub fn resolve(
    options: &LaunchOptions,
    env_model: Option<PathBuf>,
    saved_model: Option<PathBuf>,
    cwd: Option<PathBuf>,
    saved_workspace: Option<PathBuf>,
) -> StartupSelection {
    let (model, model_source) = if let Some(path) = &options.model {
        (Some(path.clone()), ModelSource::Argument)
    } else if let Some(path) = env_model.filter(|p| !p.as_os_str().is_empty()) {
        (Some(path), ModelSource::Environment)
    } else if let Some(path) = saved_model.filter(|p| !p.as_os_str().is_empty()) {
        (Some(path), ModelSource::Saved)
    } else {
        (None, ModelSource::Missing)
    };

    let mut workspace_error = None;
    let explicit = options.workspace.as_ref();
    let workspace = if let Some(path) = explicit {
        validate_workspace(path, &mut workspace_error)
    } else {
        let inherited = cwd.as_ref().filter(|p| !is_home_or_root(p));
        inherited
            .and_then(|p| validate_workspace(p, &mut workspace_error))
            .or_else(|| {
                saved_workspace
                    .as_ref()
                    .filter(|p| !is_home_or_root(p))
                    .and_then(|p| validate_workspace(p, &mut workspace_error))
            })
    };
    StartupSelection {
        model,
        model_source,
        workspace,
        workspace_error,
    }
}

fn validate_workspace(path: &Path, error: &mut Option<String>) -> Option<PathBuf> {
    match path.canonicalize() {
        Ok(path) if path.is_dir() => {
            *error = None;
            Some(path)
        }
        Ok(_) => {
            *error = Some(format!("Workspace is not a directory: {}", path.display()));
            None
        }
        Err(e) => {
            *error = Some(format!("Cannot open workspace {}: {e}", path.display()));
            None
        }
    }
}

fn is_home_or_root(path: &Path) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    canonical.parent().is_none()
        || env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|p| p.canonicalize().ok())
            .as_ref()
            == Some(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_precedence_preserves_path_values() {
        let options = LaunchOptions {
            model: Some("arg model.gguf".into()),
            ..Default::default()
        };
        let r = resolve(
            &options,
            Some("env.gguf".into()),
            Some("saved.gguf".into()),
            None,
            None,
        );
        assert_eq!(r.model, Some(PathBuf::from("arg model.gguf")));
        assert_eq!(r.model_source, ModelSource::Argument);
        let r = resolve(
            &LaunchOptions::default(),
            Some("env.gguf".into()),
            Some("saved.gguf".into()),
            None,
            None,
        );
        assert_eq!(r.model, Some(PathBuf::from("env.gguf")));
        assert_eq!(r.model_source, ModelSource::Environment);
    }
    #[test]
    fn empty_environment_value_falls_through_to_saved_value() {
        let r = resolve(
            &LaunchOptions::default(),
            Some(PathBuf::new()),
            Some("saved.gguf".into()),
            None,
            None,
        );
        assert_eq!(r.model, Some(PathBuf::from("saved.gguf")));
        assert_eq!(r.model_source, ModelSource::Saved);
    }
    #[test]
    fn argument_parser_rejects_missing_values_and_unknown_flags() {
        assert!(parse_args(["--model".into()]).is_err());
        assert!(parse_args(["--wat".into()]).is_err());
        assert_eq!(
            parse_args(["--demo".into(), "--workspace".into(), ".".into()])
                .unwrap()
                .workspace,
            Some(PathBuf::from("."))
        );
    }

    #[test]
    fn workspace_uses_cwd_before_saved_and_canonicalizes_it() {
        let cwd = tempfile::tempdir().unwrap();
        let saved = tempfile::tempdir().unwrap();
        let r = resolve(
            &LaunchOptions::default(),
            None,
            None,
            Some(cwd.path().join(".")),
            Some(saved.path().into()),
        );
        assert_eq!(r.workspace, Some(cwd.path().canonicalize().unwrap()));
        assert!(r.workspace_error.is_none());
    }

    #[test]
    fn explicit_invalid_workspace_is_reported_without_fallback() {
        let saved = tempfile::tempdir().unwrap();
        let missing = saved.path().join("missing-project");
        let options = LaunchOptions {
            workspace: Some(missing.clone()),
            ..Default::default()
        };
        let r = resolve(
            &options,
            None,
            None,
            Some(saved.path().into()),
            Some(saved.path().into()),
        );
        assert_eq!(r.workspace, None);
        assert!(
            r.workspace_error
                .unwrap()
                .contains(&missing.display().to_string())
        );
    }
}
