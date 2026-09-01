//! Product scope candidates for new Arkret Agent pairings.
//!
//! Completion belongs only to authoring a new candidate. Accepted provision,
//! key and session scopes must retain their exact commitments and are never
//! upgraded by this module.

use arkret_wire::ServiceOperationId;

/// Return only a recovery selected by a service-supplied registered reason.
pub fn agent_scope_recovery(reason: &str) -> Option<&'static str> {
    use arkret_schema::agent_runtime_scope::AgentRuntimeScopeLayer;
    use arkret_wire::error_codes::ReasonCode;

    let layer = match ReasonCode::from_wire(reason) {
        ReasonCode::AgentProvisionScopeMigrationRequired => AgentRuntimeScopeLayer::Provision,
        ReasonCode::AgentKeyScopeReauthorizationRequired => {
            AgentRuntimeScopeLayer::KeyAuthorization
        }
        ReasonCode::AgentSessionScopeRefreshRequired => AgentRuntimeScopeLayer::Session,
        _ => return None,
    };
    Some(arkret_schema::agent_runtime_scope_layer_descriptor(layer).recovery)
}

/// Build the interactive Agent candidate with encrypted presence and without
/// delayed-publication leases.
pub fn default_agent_runtime_scope() -> Result<Vec<String>, String> {
    arkret_schema::agent_runtime_scope::complete_agent_runtime_scope([
        ServiceOperationId::SELF_EVENTS_STREAM_SUBSCRIBE_V1,
        ServiceOperationId::SELF_EVENTS_READ_SCAN_V1,
        ServiceOperationId::SELF_EVENTS_READ_FRONTIER_V1,
        ServiceOperationId::SELF_EVENTS_COMMAND_SUBMIT_V1,
        ServiceOperationId::SELF_KEYS_KEYPACKAGES_UPLOAD_CREATE_V1,
        ServiceOperationId::SELF_KEYS_KEYPACKAGES_COMMAND_CONSUME_V1,
        ServiceOperationId::SELF_KEYS_KEYPACKAGES_COMMAND_REVOKE_V1,
        ServiceOperationId::SELF_DEVICE_MESSAGES_READ_LIST_V1,
        ServiceOperationId::SELF_DEVICE_MESSAGES_COMMAND_ACK_V1,
        ServiceOperationId::SELF_SIGNAL_COMMAND_SEND_V1,
        "ak.event.read",
        "ak.message.create",
    ])
    .map_err(|error| error.to_string())
}

/// Validate a supplied scope without rewriting or completing its tokens.
pub fn validate_agent_runtime_scope(actions: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for action in actions {
        if action.trim() != action
            || (arkret_schema::capability_action(action).is_none()
                && ServiceOperationId::from_wire(action).is_none())
        {
            return Err(format!("Unknown canonical Arkret scope action: {action}"));
        }
        if !seen.insert(action) {
            return Err(format!("Duplicate Arkret scope action: {action}"));
        }
    }
    // This checks the supplied candidate's completeness, not authority. The
    // Station still independently checks immutable provision and key ceilings.
    if let Some(deficiency) =
        arkret_schema::agent_runtime_scope::assess_agent_runtime_provision_scope(actions)
            .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "Arkret scope migration required; missing operations: {}. Do not widen an existing provision, key or session scope.",
            deficiency.missing_operations.join(", ")
        ));
    }
    Ok(())
}

/// Require a canonical service grant for exactly this local request.
pub fn session_scope_matches_request(requested: &[String], granted: &[String]) -> bool {
    validate_agent_runtime_scope(requested).is_ok()
        && validate_agent_runtime_scope(granted).is_ok()
        && requested.iter().all(|action| granted.contains(action))
        && granted.iter().all(|action| requested.contains(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_scope_candidate_is_registered_complete_and_minimal() {
        let actions = default_agent_runtime_scope().unwrap();
        validate_agent_runtime_scope(&actions).unwrap();
        assert!(
            actions
                .iter()
                .any(|action| action == ServiceOperationId::SELF_SEALS_READ_FRONTIER_V1)
        );
        assert!(
            actions
                .iter()
                .any(|action| action == ServiceOperationId::SELF_SIGNAL_COMMAND_SEND_V1)
        );
        for excluded in [ServiceOperationId::SELF_AUTHORIZATION_LEASES_COMMAND_ISSUE_V1] {
            assert!(!actions.iter().any(|action| action == excluded));
        }
    }

    #[test]
    fn agent_scope_validation_never_upgrades_existing_commitments() {
        for invalid in [
            "ak.self.events.read.scan",
            "ak.self.events.query.scan",
            "ak.self.events.query.scan.v1",
            "ak.self.not_registered.v1",
        ] {
            let actions = vec![invalid.to_owned()];
            assert!(validate_agent_runtime_scope(&actions).is_err());
            assert_eq!(actions, [invalid]);
        }
        let mut incomplete = default_agent_runtime_scope().unwrap();
        incomplete.retain(|action| action != ServiceOperationId::SELF_SEALS_READ_FRONTIER_V1);
        let original = incomplete.clone();
        let error = validate_agent_runtime_scope(&incomplete).unwrap_err();
        assert!(error.contains(ServiceOperationId::SELF_SEALS_READ_FRONTIER_V1));
        assert_eq!(incomplete, original);
    }

    #[test]
    fn agent_scope_recovery_keeps_service_reported_layers_distinct() {
        assert_eq!(
            agent_scope_recovery("agent_provision_scope_migration_required"),
            Some("provision_new_agent")
        );
        assert_eq!(
            agent_scope_recovery("agent_key_scope_reauthorization_required"),
            Some("reauthorize_key_within_provision_ceiling")
        );
        assert_eq!(
            agent_scope_recovery("agent_session_scope_refresh_required"),
            Some("issue_session_within_provision_and_key_ceilings")
        );
        assert_eq!(agent_scope_recovery("unregistered_reason"), None);
    }

    #[test]
    fn agent_scope_session_rejects_overgrant_missing_floor_and_old_tokens() {
        let requested = default_agent_runtime_scope().unwrap();
        assert!(session_scope_matches_request(&requested, &requested));
        let mut extra = requested.clone();
        extra.push(ServiceOperationId::SELF_AUTHORIZATION_LEASES_COMMAND_ISSUE_V1.to_owned());
        assert!(!session_scope_matches_request(&requested, &extra));
        let mut reduced = requested.clone();
        reduced.retain(|action| action != ServiceOperationId::SELF_SEALS_READ_FRONTIER_V1);
        assert!(!session_scope_matches_request(&requested, &reduced));
        let mut old = requested.clone();
        old.push("ak.self.events.read.scan".to_owned());
        assert!(!session_scope_matches_request(&old, &old));
    }
}
