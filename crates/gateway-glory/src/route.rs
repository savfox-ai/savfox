//! Browser history routes. The gateway serves index.html for deep links.
pub const NAVIGATION: &[(&str, &str)] = &[
    ("/sessions", "nav.sessions"),
    ("/overview", "nav.overview"),
    ("/approvals", "nav.approvals"),
    ("/agents", "nav.agents"),
    ("/models", "nav.models"),
    ("/channels", "nav.channels"),
    ("/cron", "nav.cron_jobs"),
    ("/skills", "nav.skills"),
    ("/speech", "nav.speech"),
    ("/devices", "nav.devices"),
    ("/logs", "nav.logs"),
    ("/usage", "nav.usage"),
    ("/config", "nav.config"),
    ("/config/advanced", "nav.advanced"),
    ("/config/diagnostics", "nav.debug"),
];

pub fn is_session_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    path.is_empty() || path == "/sessions" || path.starts_with("/sessions/")
}

pub fn current_path() -> String {
    #[cfg(target_arch = "wasm32")]
    if let Some(window) = web_sys::window() {
        let location = window.location();
        return format!(
            "{}{}",
            location.pathname().unwrap_or_else(|_| "/".into()),
            location.search().unwrap_or_default()
        );
    }
    "/".into()
}

pub fn is_active(path: &str, destination: &str) -> bool {
    if destination == "/sessions" && is_session_path(path) {
        return true;
    }
    let path = path.split('?').next().unwrap_or(path);
    path == destination
        || (destination != "/"
            && path
                .strip_prefix(destination)
                .is_some_and(|tail| tail.starts_with('/')))
}

#[cfg(test)]
mod tests {
    #[test]
    fn section_matches_deep_links_without_prefix_collisions() {
        assert!(super::is_active("/agents/alice/files", "/agents"));
        assert!(super::is_active("/sessions?session=a", "/sessions"));
        assert!(super::is_active("/?model=openai%2Fexample", "/sessions"));
        assert!(super::is_active("/", "/sessions"));
        assert!(!super::is_active("/overview", "/sessions"));
        assert!(!super::is_active("/agents-extra", "/agents"));
        assert!(!super::is_active("/agents", "/"));
    }
}
