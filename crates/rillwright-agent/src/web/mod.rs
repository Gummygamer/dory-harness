//! Web search for the agent: the HTTP layer and the search providers.
//!
//! The harness makes these requests itself, so they work without `--network` (that flag only
//! governs commands in the sandbox). The key is held by the provider and never reaches a message.

pub mod http;
pub mod provider;
pub mod state;

pub use provider::{Brave, SearchError, SearchHit, SearchProvider, Tavily};

/// The base URL a provider sends its requests to. `RILLWRIGHT_WEB_BASE_URL` (or legacy
/// `DORY_WEB_BASE_URL`) may replace the default in
/// debug builds only, so the tests and the evaluation can use a fake server. Release builds ignore
/// it: a stray value must not send the key to another host.
pub fn base_url(default: &str) -> String {
    let override_url = choose_override(
        std::env::var("RILLWRIGHT_WEB_BASE_URL").ok(),
        std::env::var("DORY_WEB_BASE_URL").ok(),
    );
    resolve_base(cfg!(debug_assertions), override_url.as_deref(), default)
}

fn choose_override(canonical: Option<String>, legacy: Option<String>) -> Option<String> {
    canonical
        .filter(|s| !s.trim().is_empty())
        .or_else(|| legacy.filter(|s| !s.trim().is_empty()))
}

fn resolve_base(debug: bool, override_url: Option<&str>, default: &str) -> String {
    match override_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) if debug => url.to_string(),
        _ => default.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{choose_override, resolve_base};

    const DEFAULT: &str = "https://api.tavily.com";

    #[test]
    fn canonical_debug_override_wins_and_empty_falls_back_to_legacy() {
        assert_eq!(
            choose_override(Some("http://new".into()), Some("http://old".into())),
            Some("http://new".into())
        );
        assert_eq!(
            choose_override(Some("  ".into()), Some("http://old".into())),
            Some("http://old".into())
        );
    }

    #[test]
    fn a_debug_build_uses_the_override() {
        assert_eq!(
            resolve_base(true, Some("http://127.0.0.1:9"), DEFAULT),
            "http://127.0.0.1:9"
        );
    }

    #[test]
    fn a_release_build_ignores_the_override() {
        assert_eq!(
            resolve_base(false, Some("http://127.0.0.1:9"), DEFAULT),
            DEFAULT
        );
    }

    #[test]
    fn an_empty_override_is_the_default() {
        assert_eq!(resolve_base(true, Some("  "), DEFAULT), DEFAULT);
        assert_eq!(resolve_base(true, None, DEFAULT), DEFAULT);
    }
}
