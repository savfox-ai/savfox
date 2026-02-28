use savfox_app_server_protocol::SessionSourceKind;
use savfox_core::INTERACTIVE_SESSION_SOURCES;
use savfox_protocol::protocol::{
    SessionSource as CoreSessionSource, SubAgentSource as CoreSubAgentSource,
};

pub(crate) fn compute_source_filters(
    source_kinds: Option<Vec<SessionSourceKind>>,
) -> (Vec<CoreSessionSource>, Option<Vec<SessionSourceKind>>) {
    let Some(source_kinds) = source_kinds else {
        return (INTERACTIVE_SESSION_SOURCES.to_vec(), None);
    };

    if source_kinds.is_empty() {
        return (INTERACTIVE_SESSION_SOURCES.to_vec(), None);
    }

    let requires_post_filter = source_kinds.iter().any(|kind| {
        matches!(
            kind,
            SessionSourceKind::Exec
                | SessionSourceKind::AppServer
                | SessionSourceKind::SubAgent
                | SessionSourceKind::SubAgentReview
                | SessionSourceKind::SubAgentCompact
                | SessionSourceKind::SubAgentSessionSpawn
                | SessionSourceKind::SubAgentOther
                | SessionSourceKind::Unknown
        )
    });

    if requires_post_filter {
        (Vec::new(), Some(source_kinds))
    } else {
        let interactive_sources = source_kinds
            .iter()
            .filter_map(|kind| match kind {
                SessionSourceKind::Cli => Some(CoreSessionSource::Cli),
                SessionSourceKind::VsCode => Some(CoreSessionSource::VSCode),
                SessionSourceKind::Exec
                | SessionSourceKind::AppServer
                | SessionSourceKind::SubAgent
                | SessionSourceKind::SubAgentReview
                | SessionSourceKind::SubAgentCompact
                | SessionSourceKind::SubAgentSessionSpawn
                | SessionSourceKind::SubAgentOther
                | SessionSourceKind::Unknown => None,
            })
            .collect::<Vec<_>>();
        (interactive_sources, Some(source_kinds))
    }
}

pub(crate) fn source_kind_matches(
    source: &CoreSessionSource,
    filter: &[SessionSourceKind],
) -> bool {
    filter.iter().any(|kind| match kind {
        SessionSourceKind::Cli => matches!(source, CoreSessionSource::Cli),
        SessionSourceKind::VsCode => matches!(source, CoreSessionSource::VSCode),
        SessionSourceKind::Exec => matches!(source, CoreSessionSource::Exec),
        SessionSourceKind::AppServer => matches!(source, CoreSessionSource::Mcp),
        SessionSourceKind::SubAgent => matches!(source, CoreSessionSource::SubAgent(_)),
        SessionSourceKind::SubAgentReview => {
            matches!(
                source,
                CoreSessionSource::SubAgent(CoreSubAgentSource::Review)
            )
        }
        SessionSourceKind::SubAgentCompact => {
            matches!(
                source,
                CoreSessionSource::SubAgent(CoreSubAgentSource::Compact)
            )
        }
        SessionSourceKind::SubAgentSessionSpawn => matches!(
            source,
            CoreSessionSource::SubAgent(CoreSubAgentSource::SessionSpawn { .. })
        ),
        SessionSourceKind::SubAgentOther => matches!(
            source,
            CoreSessionSource::SubAgent(CoreSubAgentSource::Other(_))
        ),
        SessionSourceKind::Unknown => matches!(source, CoreSessionSource::Unknown),
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use savfox_protocol::SessionId;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn compute_source_filters_defaults_to_interactive_sources() {
        let (allowed_sources, filter) = compute_source_filters(None);

        assert_eq!(allowed_sources, INTERACTIVE_SESSION_SOURCES.to_vec());
        assert_eq!(filter, None);
    }

    #[test]
    fn compute_source_filters_empty_means_interactive_sources() {
        let (allowed_sources, filter) = compute_source_filters(Some(Vec::new()));

        assert_eq!(allowed_sources, INTERACTIVE_SESSION_SOURCES.to_vec());
        assert_eq!(filter, None);
    }

    #[test]
    fn compute_source_filters_interactive_only_skips_post_filtering() {
        let source_kinds = vec![SessionSourceKind::Cli, SessionSourceKind::VsCode];
        let (allowed_sources, filter) = compute_source_filters(Some(source_kinds.clone()));

        assert_eq!(
            allowed_sources,
            vec![CoreSessionSource::Cli, CoreSessionSource::VSCode]
        );
        assert_eq!(filter, Some(source_kinds));
    }

    #[test]
    fn compute_source_filters_subagent_variant_requires_post_filtering() {
        let source_kinds = vec![SessionSourceKind::SubAgentReview];
        let (allowed_sources, filter) = compute_source_filters(Some(source_kinds.clone()));

        assert_eq!(allowed_sources, Vec::new());
        assert_eq!(filter, Some(source_kinds));
    }

    #[test]
    fn source_kind_matches_distinguishes_subagent_variants() {
        let parent_session_id =
            SessionId::from_string(&Uuid::new_v4().to_string()).expect("valid session id");
        let review = CoreSessionSource::SubAgent(CoreSubAgentSource::Review);
        let spawn = CoreSessionSource::SubAgent(CoreSubAgentSource::SessionSpawn {
            parent_session_id,
            depth: 1,
        });

        assert!(source_kind_matches(
            &review,
            &[SessionSourceKind::SubAgentReview]
        ));
        assert!(!source_kind_matches(
            &review,
            &[SessionSourceKind::SubAgentSessionSpawn]
        ));
        assert!(source_kind_matches(
            &spawn,
            &[SessionSourceKind::SubAgentSessionSpawn]
        ));
        assert!(!source_kind_matches(
            &spawn,
            &[SessionSourceKind::SubAgentReview]
        ));
    }
}
