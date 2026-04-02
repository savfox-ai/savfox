use savfox_app_server_protocol::{
    AppsListParams, AppsListResponse, JSONRPCErrorError, RequestId, SkillsConfigWriteParams,
    SkillsConfigWriteResponse, SkillsListParams, SkillsListResponse,
};
use savfox_core::config::edit::{ConfigEdit, ConfigEditsBuilder};
use savfox_core::connectors;
use savfox_core::features::Feature;

use super::{INTERNAL_ERROR_CODE, SavfoxMessageProcessor, errors_to_info, skills_to_info};

impl SavfoxMessageProcessor {
    pub(crate) async fn apps_list(&self, request_id: RequestId, params: AppsListParams) {
        let AppsListParams { cursor, limit } = params;
        let config = match self.load_latest_config().await {
            Ok(config) => config,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        if !config.features.enabled(Feature::Apps) {
            self.outgoing
                .send_response(
                    request_id,
                    AppsListResponse {
                        data: Vec::new(),
                        next_cursor: None,
                    },
                )
                .await;
            return;
        }

        let connectors = match connectors::list_accessible_connectors_from_mcp_tools(&config).await
        {
            Ok(connectors) => connectors,
            Err(err) => {
                self.send_internal_error(request_id, format!("failed to list apps: {err}"))
                    .await;
                return;
            }
        };

        let total = connectors.len();
        if total == 0 {
            self.outgoing
                .send_response(
                    request_id,
                    AppsListResponse {
                        data: Vec::new(),
                        next_cursor: None,
                    },
                )
                .await;
            return;
        }

        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = effective_limit.min(total);
        let start = match cursor {
            Some(cursor) => {
                if let Ok(idx) = cursor.parse::<usize>() {
                    idx
                } else {
                    self.send_invalid_request_error(
                        request_id,
                        format!("invalid cursor: {cursor}"),
                    )
                    .await;
                    return;
                }
            }
            None => 0,
        };

        if start > total {
            self.send_invalid_request_error(
                request_id,
                format!("cursor {start} exceeds total apps {total}"),
            )
            .await;
            return;
        }

        let end = start.saturating_add(effective_limit).min(total);
        let data = connectors[start..end].to_vec();

        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };
        self.outgoing
            .send_response(request_id, AppsListResponse { data, next_cursor })
            .await;
    }

    pub(crate) async fn skills_list(&self, request_id: RequestId, params: SkillsListParams) {
        let SkillsListParams { cwds, force_reload } = params;
        let cwds = if cwds.is_empty() {
            vec![self.config.cwd.clone()]
        } else {
            cwds
        };

        let skills_manager = self.session_manager.skills_manager();
        let mut data = Vec::new();
        for cwd in cwds {
            let outcome = skills_manager.skills_for_cwd(&cwd, force_reload).await;
            let errors = errors_to_info(&outcome.errors);
            let skills = skills_to_info(&outcome.skills, &outcome.disabled_paths);
            data.push(savfox_app_server_protocol::SkillsListEntry {
                cwd,
                skills,
                errors,
            });
        }
        self.outgoing
            .send_response(request_id, SkillsListResponse { data })
            .await;
    }

    pub(crate) async fn skills_config_write(
        &self,
        request_id: RequestId,
        params: SkillsConfigWriteParams,
    ) {
        let SkillsConfigWriteParams { path, enabled } = params;
        let edits = vec![ConfigEdit::SetSkillConfig { path, enabled }];
        let result = ConfigEditsBuilder::new(&self.config.savfox_home)
            .with_edits(edits)
            .apply()
            .await;

        match result {
            Ok(()) => {
                self.session_manager.skills_manager().clear_cache();
                self.outgoing
                    .send_response(
                        request_id,
                        SkillsConfigWriteResponse {
                            effective_enabled: enabled,
                        },
                    )
                    .await;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to update skill settings: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }
}
