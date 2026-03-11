use std::sync::Arc;

use savfox_app_server_protocol::{
    AskForApproval, JSONRPCErrorError, RequestId, ReviewDelivery as ApiReviewDelivery,
    ReviewStartParams, ReviewStartResponse, ServerNotification, SessionItem, Turn,
    TurnInterruptParams, TurnStartParams, TurnStartResponse, TurnStartedNotification, TurnStatus,
    UserInput as V2UserInput,
};
use savfox_core::protocol::{Op, ReviewDelivery as CoreReviewDelivery, ReviewRequest};
use savfox_core::{NewSession, SavfoxSession};
use savfox_protocol::SessionId;
use savfox_protocol::user_input::UserInput as CoreInputItem;

use super::{
    INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE, SavfoxMessageProcessor,
    read_summary_from_rollout, summary_to_session,
};

impl SavfoxMessageProcessor {
    pub(crate) async fn turn_start(&self, request_id: RequestId, params: TurnStartParams) {
        let (_, session) = match self.load_session(&params.session_id).await {
            Ok(v) => v,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        // Map v2 input items to core input items.
        let mapped_items: Vec<CoreInputItem> = params
            .input
            .into_iter()
            .map(V2UserInput::into_core)
            .collect();

        let has_any_overrides = params.cwd.is_some()
            || params.approval_policy.is_some()
            || params.sandbox_policy.is_some()
            || params.model.is_some()
            || params.effort.is_some()
            || params.summary.is_some()
            || params.collaboration_mode.is_some()
            || params.personality.is_some();

        // If any overrides are provided, update the session turn context first.
        if has_any_overrides {
            let _ = session
                .submit(Op::OverrideTurnContext {
                    cwd: params.cwd,
                    approval_policy: params.approval_policy.map(AskForApproval::to_core),
                    sandbox_policy: params.sandbox_policy.map(|p| p.to_core()),
                    windows_sandbox_level: None,
                    model: params.model,
                    effort: params.effort.map(Some),
                    summary: params.summary,
                    collaboration_mode: params.collaboration_mode,
                    personality: params.personality,
                    permission_policy: None,
                })
                .await;
        }

        // Start the turn by submitting the user input. Return its submission id as turn_id.
        let turn_id = session
            .submit(Op::UserInput {
                items: mapped_items,
                final_output_json_schema: params.output_schema,
            })
            .await;

        match turn_id {
            Ok(turn_id) => {
                let turn = Turn {
                    id: turn_id.clone(),
                    items: vec![],
                    error: None,
                    status: TurnStatus::InProgress,
                };

                let response = TurnStartResponse { turn: turn.clone() };
                self.outgoing.send_response(request_id, response).await;

                // Emit v2 turn/started notification.
                let notif = TurnStartedNotification {
                    session_id: params.session_id,
                    turn,
                };
                self.outgoing
                    .send_server_notification(ServerNotification::TurnStarted(notif))
                    .await;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to start turn: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    fn build_review_turn(turn_id: String, display_text: &str) -> Turn {
        let items = if display_text.is_empty() {
            Vec::new()
        } else {
            vec![SessionItem::UserMessage {
                id: turn_id.clone(),
                content: vec![V2UserInput::Text {
                    text: display_text.to_string(),
                    // Review prompt display text is synthesized; no UI element ranges to preserve.
                    text_elements: Vec::new(),
                }],
            }]
        };

        Turn {
            id: turn_id,
            items,
            error: None,
            status: TurnStatus::InProgress,
        }
    }

    async fn emit_review_started(
        &self,
        request_id: &RequestId,
        turn: Turn,
        parent_session_id: String,
        review_session_id: String,
    ) {
        let response = ReviewStartResponse {
            turn: turn.clone(),
            review_session_id,
        };
        self.outgoing
            .send_response(request_id.clone(), response)
            .await;

        let notif = TurnStartedNotification {
            session_id: parent_session_id,
            turn,
        };
        self.outgoing
            .send_server_notification(ServerNotification::TurnStarted(notif))
            .await;
    }

    async fn start_inline_review(
        &self,
        request_id: &RequestId,
        parent_session: Arc<SavfoxSession>,
        review_request: ReviewRequest,
        display_text: &str,
        parent_session_id: String,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        let turn_id = parent_session.submit(Op::Review { review_request }).await;

        match turn_id {
            Ok(turn_id) => {
                let turn = Self::build_review_turn(turn_id, display_text);
                self.emit_review_started(
                    request_id,
                    turn,
                    parent_session_id.clone(),
                    parent_session_id,
                )
                .await;
                Ok(())
            }
            Err(err) => Err(JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to start review: {err}"),
                data: None,
            }),
        }
    }

    async fn start_detached_review(
        &mut self,
        request_id: &RequestId,
        parent_session_id: SessionId,
        review_request: ReviewRequest,
        display_text: &str,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        let rollout_path = savfox_core::find_session_path_by_id_str(
            &self.config.savfox_home,
            &parent_session_id.to_string(),
        )
        .await
        .map_err(|err| JSONRPCErrorError {
            code: INTERNAL_ERROR_CODE,
            message: format!("failed to locate session id {parent_session_id}: {err}"),
            data: None,
        })?
        .ok_or_else(|| JSONRPCErrorError {
            code: INVALID_REQUEST_ERROR_CODE,
            message: format!("no rollout found for session id {parent_session_id}"),
            data: None,
        })?;

        let mut config = self.config.as_ref().clone();
        if let Some(review_model) = &config.review_model {
            config.model = Some(review_model.clone());
        }

        let NewSession {
            session_id,
            session: review_session,
            session_configured,
            ..
        } = self
            .session_manager
            .fork_session(usize::MAX, config, rollout_path)
            .await
            .map_err(|err| JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("error creating detached review session: {err}"),
                data: None,
            })?;

        if let Err(err) = self.attach_conversation_listener(session_id, false).await {
            tracing::warn!(
                "failed to attach listener for review session {}: {}",
                session_id,
                err.message
            );
        }

        let fallback_provider = self.config.model_provider_id.as_str();
        if let Some(rollout_path) = review_session.rollout_path() {
            match read_summary_from_rollout(rollout_path.as_path(), fallback_provider).await {
                Ok(summary) => {
                    let session = summary_to_session(summary);
                    let notif = savfox_app_server_protocol::SessionStartedNotification { session };
                    self.outgoing
                        .send_server_notification(
                            savfox_app_server_protocol::ServerNotification::SessionStarted(notif),
                        )
                        .await;
                }
                Err(err) => {
                    tracing::warn!(
                        "failed to load summary for review session {}: {}",
                        session_configured.session_id,
                        err
                    );
                }
            }
        } else {
            tracing::warn!(
                "review session {} has no rollout path",
                session_configured.session_id
            );
        }

        let turn_id = review_session
            .submit(Op::Review { review_request })
            .await
            .map_err(|err| JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to start detached review turn: {err}"),
                data: None,
            })?;

        let turn = Self::build_review_turn(turn_id, display_text);
        let review_session_id = session_id.to_string();
        self.emit_review_started(
            request_id,
            turn,
            review_session_id.clone(),
            review_session_id,
        )
        .await;

        Ok(())
    }

    pub(crate) async fn review_start(&mut self, request_id: RequestId, params: ReviewStartParams) {
        let ReviewStartParams {
            session_id,
            target,
            delivery,
        } = params;
        let (parent_session_id, parent_session) = match self.load_session(&session_id).await {
            Ok(v) => v,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let (review_request, display_text) = match Self::review_request_from_target(target) {
            Ok(value) => value,
            Err(err) => {
                self.outgoing.send_error(request_id, err).await;
                return;
            }
        };

        let delivery = delivery.unwrap_or(ApiReviewDelivery::Inline).to_core();
        match delivery {
            CoreReviewDelivery::Inline => {
                if let Err(err) = self
                    .start_inline_review(
                        &request_id,
                        parent_session,
                        review_request,
                        display_text.as_str(),
                        session_id.clone(),
                    )
                    .await
                {
                    self.outgoing.send_error(request_id, err).await;
                }
            }
            CoreReviewDelivery::Detached => {
                if let Err(err) = self
                    .start_detached_review(
                        &request_id,
                        parent_session_id,
                        review_request,
                        display_text.as_str(),
                    )
                    .await
                {
                    self.outgoing.send_error(request_id, err).await;
                }
            }
        }
    }

    pub(crate) async fn turn_interrupt(
        &mut self,
        request_id: RequestId,
        params: TurnInterruptParams,
    ) {
        let TurnInterruptParams { session_id, .. } = params;

        let (session_uuid, session) = match self.load_session(&session_id).await {
            Ok(v) => v,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        // Record the pending interrupt so we can reply when TurnAborted arrives.
        {
            let mut map = self.pending_interrupts.lock().await;
            map.entry(session_uuid).or_default().push(request_id);
        }

        // Submit the interrupt; we'll respond upon TurnAborted.
        let _ = session.submit(Op::Interrupt).await;
    }
}
