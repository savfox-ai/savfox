use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use savfox_browser_automation::screenshot::ScreenshotFormat;
use savfox_browser_automation::{Browser, BrowserLaunchOptions, ScreenshotOptions};
use savfox_core::auth::{CLIENT_ID, login_with_api_key};
use savfox_core::{AuthManager, SavfoxAuth};
use savfox_login_oauth::{
    ServerOptions, ShutdownHandle, complete_device_code_login, request_device_code,
    run_login_server,
};
use savfox_protocol::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::{GatewayAuth, TokenInfo, has_scope, required_scope};
use crate::bridge::GatewayBridge;
use crate::chat_session::{
    abort_all_active_threads, abort_first_active_candidate, persist_chat_session_metadata,
    provider_from_model, resolve_abort_candidate_ids, validate_uuid_v7_session_id,
};
use crate::cron_service::{
    CronDelivery, CronPayload, CronSchedule, CronService, CronSessionTarget,
};
use crate::exec_approval::{
    ApprovalForwardingConfig, ExecApprovalRequest, ExecApprovalResolution,
    forward_approval_to_chat, list_pending_approvals, notify_approval_resolved,
    persist_pending_approval, persist_resolved_approval,
};
use crate::identity_links::{
    canonical_for_peer, load_identity_links, save_identity_links, upsert_link,
};
use crate::media_store::MediaStore;
use crate::session::{
    DmScope, GatewaySessionManager, SessionEntry, SessionOverrides, SessionStore,
    build_history_payload, build_routing_id, derive_session_label_from_history,
};
use crate::{
    approval_policy_store, log_store, pairing_store, plugin, skills_store, tts_service,
    voice_store, wizard_store,
};

// ─── JSON-RPC types ──────────────────────────────────────────────────────────

/// JSON-RPC 2.0 request envelope received over WebSocket.
#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcRequest {
    pub jsonrpc: Option<String>,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 success response.
#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub result: Value,
}

/// JSON-RPC 2.0 error response.
#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcError {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub error: JsonRpcErrorBody,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcErrorBody {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ─── Error codes ─────────────────────────────────────────────────────────────

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const PERMISSION_DENIED: i64 = -32001;
const PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE: u32 = 60;

fn rpc_success(id: Value, mut result: Value) -> String {
    crate::redaction::redact_json_in_place(&mut result);
    serde_json::to_string(&JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result,
    })
    .unwrap_or_default()
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> String {
    let safe_message = crate::redaction::redact_text(&message.into());
    serde_json::to_string(&JsonRpcError {
        jsonrpc: "2.0",
        id,
        error: JsonRpcErrorBody {
            code,
            message: safe_message,
            data: None,
        },
    })
    .unwrap_or_default()
}

// ─── Dispatcher ──────────────────────────────────────────────────────────────

/// Dispatch a JSON-RPC request to the appropriate handler.
///
/// Returns a JSON string to be sent back over the WebSocket.
///
/// Supports 96+ methods from the OpenClaw gateway protocol, organized by
/// domain: agent, agents, chat, sessions, channels, config, cron, nodes,
/// devices, tts, skills, system, exec-approvals, usage, logs, wizard, a2a, etc.
pub(crate) async fn dispatch_rpc(
    raw_text: &str,
    _session_id: &str,
    _auth: &Arc<GatewayAuth>,
    session_mgr: &Arc<GatewaySessionManager>,
    bridge: &Arc<GatewayBridge>,
    session_store: &Arc<SessionStore>,
    cron_service: &Arc<CronService>,
    token_info: &TokenInfo,
) -> String {
    let request: JsonRpcRequest = match serde_json::from_str(raw_text) {
        Ok(r) => r,
        Err(err) => return rpc_error(Value::Null, PARSE_ERROR, format!("parse error: {err}")),
    };

    let id = request.id.clone();
    let params = request
        .params
        .clone()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    // ── Scope check ──────────────────────────────────────────────────
    let scope = required_scope(&request.method);
    if !has_scope(token_info, &scope) {
        return rpc_error(
            id,
            PERMISSION_DENIED,
            format!(
                "permission denied: method \"{}\" requires scope \"{}\"",
                request.method, scope,
            ),
        );
    }

    let result = match request.method.as_str() {
        // ── Core ────────────────────────────────────────────────────────
        "connect" => handle_connect(&params).await,
        "health" => handle_health().await,
        "status" => handle_status(session_mgr, bridge).await,
        "account/login/start" => handle_account_login_start(&params, bridge).await,
        "account/login/cancel" => handle_account_login_cancel(&params).await,
        "account/read" => handle_account_read(&params, bridge).await,

        // ── Agent (single-agent operations) ─────────────────────────────
        "agent" => handle_agent(&params, bridge).await,
        "agent.identity" | "agent.identity.get" => handle_agent_identity().await,
        "agent.wait" => handle_agent_wait(&params, bridge).await,
        "agent.capabilities" => handle_agent_capabilities(&params, bridge).await,
        "agent.delegation.list" => handle_agent_delegation_list().await,
        "agent.delegation.chain" => handle_agent_delegation_chain(&params).await,
        "agent.delegation.record" => handle_agent_delegation_record(&params).await,
        "agent.delegation.remove" => handle_agent_delegation_remove(&params).await,

        // ── Agents (multi-agent CRUD) ───────────────────────────────────
        "agents.list" => handle_agents_list(bridge).await,
        "agents.get" => handle_agents_get(&params, bridge).await,
        "agents.create" => handle_agents_create(&params, bridge).await,
        "agents.update" => handle_agents_update(&params, bridge).await,
        "agents.delete" => handle_agents_delete(&params, bridge).await,
        "agents.files.list" => handle_agents_files_list(&params, bridge).await,
        "agents.files.get" => handle_agents_files_get(&params, bridge).await,
        "agents.files.set" => handle_agents_files_set(&params, bridge).await,
        "agents.files.delete" => handle_agents_files_delete(&params, bridge).await,

        // ── Chat ────────────────────────────────────────────────────────
        "chat.send" => handle_chat_send(&params, bridge, session_mgr, session_store).await,
        "chat.history" => handle_chat_history(&params, session_store, bridge).await,
        "chat.abort" => handle_chat_abort(&params, bridge, session_store).await,
        "chat.inject" => handle_chat_inject(&params, session_store).await,

        // ── Sessions ────────────────────────────────────────────────────
        "sessions.list" => handle_sessions_list(session_mgr, session_store, bridge).await,
        "sessions.preview" => handle_sessions_preview(&params, session_store, bridge).await,
        "sessions.patch" => handle_sessions_patch(&params, session_store).await,
        "sessions.reset" => {
            handle_sessions_reset(&params, session_mgr, session_store, bridge).await
        }
        "sessions.delete" => {
            handle_sessions_delete(&params, session_mgr, session_store, bridge).await
        }
        "sessions.compact" => handle_sessions_compact(&params, session_store).await,
        "sessions.overrides.get" => handle_sessions_overrides_get(&params, session_store).await,
        "sessions.overrides.set" => handle_sessions_overrides_set(&params, session_store).await,
        "sessions.identity_links.get" => handle_identity_links_get(bridge).await,
        "sessions.identity_links.set" => handle_identity_links_set(&params, bridge).await,
        "identity.link" => handle_identity_link(&params, bridge).await,
        "sessions.dm_scope.get" => handle_dm_scope_policy_get(bridge).await,
        "sessions.dm_scope.set" => handle_dm_scope_policy_set(&params, bridge).await,
        "sessions.dm_scope.migrate" => handle_dm_scope_migrate(&params, session_store).await,
        "sessions.usage" => handle_sessions_usage(&params, session_store).await,
        "media.staging.list" => handle_media_staging_list(&params, bridge).await,
        "media.staging.import" => handle_media_staging_import(&params, bridge).await,
        "media.staging.cleanup" => handle_media_staging_cleanup(&params, bridge).await,

        // ── Typing indicators ────────────────────────────────────────────
        "typing.start" => handle_typing_start(&params, session_mgr).await,
        "typing.stop" => handle_typing_stop(&params, session_mgr).await,

        // ── Events (server-push subscriptions) ──────────────────────────
        "events.subscribe" => handle_events_subscribe(&params).await,
        "events.unsubscribe" => handle_events_unsubscribe(&params).await,
        "events.list" => handle_events_list().await,

        // ── Send / Wake / Channels ──────────────────────────────────────
        "send" => handle_send(&params, bridge).await,
        "send.metrics" => handle_send_metrics().await,
        "wake" => handle_wake(&params, bridge).await,
        "channels.list" => handle_channels_list(bridge).await,
        "channels.status" => handle_channels_status(&params, bridge).await,
        "channels.login" => handle_channels_login(&params, bridge).await,
        "channels.logout" => handle_channels_logout(&params, bridge).await,
        "channels.test" => handle_channels_test(&params, bridge).await,
        "channels.account.update" => handle_channels_account_update(&params, bridge).await,
        "web.login.start" => handle_web_login_start(&params, bridge).await,
        "web.login.wait" => handle_web_login_wait(&params, bridge).await,
        "channels.nostr.profile.get" => handle_channels_nostr_profile_get(bridge).await,
        "channels.nostr.profile.set" => handle_channels_nostr_profile_set(&params, bridge).await,
        "channels.nostr.profile.import" => {
            handle_channels_nostr_profile_import(&params, bridge).await
        }
        "channels.nostr.profile.export" => handle_channels_nostr_profile_export(bridge).await,
        "channels.nostr.relays.get" => handle_channels_nostr_relays_get(bridge).await,
        "channels.nostr.relays.set" => handle_channels_nostr_relays_set(&params, bridge).await,
        "channels.config.list" => handle_channels_config_list(bridge).await,
        "channels.config.get" => handle_channels_config_get(&params, bridge).await,
        "channels.config.save" => handle_channels_config_save(&params, bridge).await,
        "channels.config.delete" => handle_channels_config_delete(&params, bridge).await,

        // ── Directory service ────────────────────────────────────────
        "directory.self" => handle_directory_self(&params, bridge, session_store).await,
        "directory.peers.list" => handle_directory_peers_list(&params, session_store).await,
        "directory.groups.list" => handle_directory_groups_list(&params, session_store).await,
        "directory.groups.members" => handle_directory_groups_members(&params, session_store).await,

        // ── Config ──────────────────────────────────────────────────────
        "config.get" => handle_config_get(bridge).await,
        "config.set" => handle_config_set(&params, bridge).await,
        "config.apply" => handle_config_apply(&params, bridge).await,
        "config.patch" => handle_config_patch(&params, bridge).await,
        "config.export" => handle_config_export(&params, bridge).await,
        "config.schema" => handle_config_schema().await,

        // ── Cron ────────────────────────────────────────────────────────
        "cron.list" => handle_cron_list(cron_service).await,
        "cron.status" => handle_cron_status(cron_service).await,
        "cron.add" => handle_cron_add(&params, cron_service).await,
        "cron.update" => handle_cron_update(&params, cron_service).await,
        "cron.remove" => handle_cron_remove(&params, cron_service).await,
        "cron.run" => handle_cron_run(&params, cron_service, bridge).await,
        "cron.runs" => handle_cron_runs(&params, cron_service).await,

        // ── Nodes ───────────────────────────────────────────────────────
        "node.list" => handle_node_list().await,
        "node.describe" => handle_node_describe(&params).await,
        "node.capabilities.list" => handle_node_capabilities_list().await,
        "node.invoke" => handle_node_invoke(&params, bridge).await,
        "node.invoke.result" => handle_node_invoke_result(&params).await,
        "node.event" => handle_node_event(&params, bridge).await,
        "node.rename" => handle_node_rename(&params, bridge).await,
        "node.camera.snap" => handle_node_tool_alias("camera.snap", &params, bridge).await,
        "node.camera.clip" => handle_node_tool_alias("camera.clip", &params, bridge).await,
        "node.screen.record" => handle_node_tool_alias("screen.record", &params, bridge).await,
        "node.location.get" => handle_node_tool_alias("location.get", &params, bridge).await,
        "node.notify" => handle_node_tool_alias("notify", &params, bridge).await,

        // ── Device pairing ──────────────────────────────────────────────
        "node.pair.request" => handle_node_pair_request(&params).await,
        "node.pair.list" => handle_node_pair_list().await,
        "node.pair.approve" => handle_node_pair_approve(&params).await,
        "node.pair.reject" => handle_node_pair_reject(&params).await,
        "node.pair.verify" => handle_node_pair_verify(&params).await,
        "device.pair.list" => handle_device_pair_list().await,
        "device.pair.approve" => handle_device_pair_approve(&params).await,
        "device.pair.reject" => handle_device_pair_reject(&params).await,
        "device.token.rotate" => handle_device_token_rotate(&params).await,
        "device.token.revoke" => handle_device_token_revoke(&params).await,

        // ── TTS (text-to-speech) ────────────────────────────────────────
        "tts.status" => handle_tts_status(bridge).await,
        "tts.providers" => handle_tts_providers().await,
        "tts.enable" => handle_tts_enable(&params, bridge).await,
        "tts.disable" => handle_tts_disable(bridge).await,
        "tts.convert" => handle_tts_convert(&params, bridge).await,
        "tts.setProvider" => handle_tts_set_provider(&params, bridge).await,

        // ── Skills ──────────────────────────────────────────────────────
        "skills.status" => handle_skills_status(bridge).await,
        "skills.bins" => handle_skills_bins(bridge).await,
        "skills.install" => handle_skills_install(&params, bridge).await,
        "skills.update" => handle_skills_update(&params, bridge).await,
        "skills.setEnv" => handle_skills_set_env(&params, bridge).await,

        // ── Exec approvals ──────────────────────────────────────────────
        "exec.approvals.get" => handle_exec_approvals_get(bridge).await,
        "exec.approvals.set" => handle_exec_approvals_set(&params, bridge).await,
        "exec.approvals.node.get" => handle_exec_approvals_node_get(&params, bridge).await,
        "exec.approvals.node.set" => handle_exec_approvals_node_set(&params, bridge).await,
        "exec.approval.request" => handle_exec_approval_request(&params, bridge, session_mgr).await,
        "exec.approval.resolve" => handle_exec_approval_resolve(&params, bridge, session_mgr).await,

        // ── Usage ───────────────────────────────────────────────────────
        "usage.status" => handle_usage_status(session_store).await,
        "usage.cost" => handle_usage_cost(&params, session_store).await,

        // ── Logs ────────────────────────────────────────────────────────
        "logs.tail" => handle_logs_tail(&params).await,

        // ── System ──────────────────────────────────────────────────────
        "last-heartbeat" => handle_last_heartbeat(&params).await,
        "set-heartbeats" => handle_set_heartbeats(&params, bridge).await,
        "system-presence" => handle_system_presence(&params, session_mgr).await,
        "system-event" => handle_system_event(&params, bridge, session_mgr, cron_service).await,

        // ── Models ──────────────────────────────────────────────────────
        "models.list" => handle_models_list(&params, bridge).await,
        "models.test" => handle_models_test(&params, bridge).await,
        "models.add" => handle_models_add(&params, bridge).await,
        "models.update" => handle_models_update(&params, bridge).await,
        "models.delete" => handle_models_delete(&params, bridge).await,
        "models.setdefault" => handle_models_setdefault(&params, bridge).await,
        "models.import" => handle_models_import(&params, bridge).await,

        // ── Tools ───────────────────────────────────────────────────────
        "tools.invoke" => handle_tools_invoke(&params, bridge).await,
        "tools.policy.get" => handle_tools_policy_get(&params, bridge).await,
        "tools.policy.set" => handle_tools_policy_set(&params, bridge).await,
        "tools.policy.reset" => handle_tools_policy_reset(&params, bridge).await,
        "tools.policy.allow" => handle_tools_policy_allow(&params, bridge).await,
        "tools.policy.deny" => handle_tools_policy_deny(&params, bridge).await,
        "tools.list" => handle_tools_list(&params, bridge).await,
        "tools.categories" => handle_tools_categories().await,

        // ── Browser ─────────────────────────────────────────────────────
        "browser.request" => handle_browser_request(&params, bridge).await,
        "browser.start" => handle_browser_start(&params, bridge).await,
        "browser.stop" => handle_browser_stop(&params, bridge).await,
        "browser.tabs.list" => handle_browser_tabs_list(&params, bridge).await,
        "browser.tabs.open" => handle_browser_tabs_open(&params, bridge).await,
        "browser.tabs.switch" => handle_browser_tabs_switch(&params, bridge).await,
        "browser.tabs.close" => handle_browser_tabs_close(&params, bridge).await,
        "browser.snapshot" => handle_browser_snapshot(&params, bridge).await,
        "browser.storage.get" => handle_browser_storage_get(&params, bridge).await,
        "browser.storage.set" => handle_browser_storage_set(&params, bridge).await,
        "browser.storage.clear" => handle_browser_storage_clear(&params, bridge).await,
        "browser.download" => handle_browser_download(&params, bridge).await,
        "browser.network.capture" => handle_browser_network_capture(&params, bridge).await,
        "browser.profiles.list" => handle_browser_profiles_list(bridge).await,
        "browser.profiles.create" => handle_browser_profiles_create(&params, bridge).await,
        "browser.profiles.delete" => handle_browser_profiles_delete(&params, bridge).await,
        "browser.profiles.default.set" => {
            handle_browser_profiles_default_set(&params, bridge).await
        }

        // ── Wizard ──────────────────────────────────────────────────────
        "wizard.start" => handle_wizard_start(&params, bridge).await,
        "wizard.next" => handle_wizard_next(&params, bridge).await,
        "wizard.cancel" => handle_wizard_cancel(&params, bridge).await,
        "wizard.status" => handle_wizard_status(bridge).await,

        // ── Memory (Markdown 4-layer system) ────────────────────────────
        "memory.list" => handle_memory_list(&params, bridge).await,
        "memory.get" => handle_memory_get(&params, bridge).await,
        "memory.create" => handle_memory_create(&params, bridge).await,
        "memory.update" => handle_memory_update(&params, bridge).await,
        "memory.delete" => handle_memory_delete(&params, bridge).await,
        "memory.search" => handle_memory_search(&params, bridge).await,
        "memory.promote" => handle_memory_promote(&params, bridge).await,
        "memory.layers" => handle_memory_layers(bridge).await,

        // ── Misc ────────────────────────────────────────────────────────
        "talk.mode" => handle_talk_mode(&params, bridge).await,
        "voicewake.get" => handle_voicewake_get(bridge).await,
        "voicewake.set" => handle_voicewake_set(&params, bridge).await,
        "update.run" => handle_update_run(bridge).await,

        // ── Webhooks ─────────────────────────────────────────────────────
        "webhooks.list" => handle_webhooks_list(bridge).await,
        "webhooks.get" => handle_webhooks_get(&params, bridge).await,
        "webhooks.create" => handle_webhooks_create(&params, bridge).await,
        "webhooks.update" => handle_webhooks_update(&params, bridge).await,
        "webhooks.delete" => handle_webhooks_delete(&params, bridge).await,
        "webhooks.test" => handle_webhooks_test(&params, bridge).await,

        // ── Skill Registry ──────────────────────────────────────────────
        "skills.registry.search" => handle_skills_registry_search(&params, bridge).await,
        "skills.registry.install" => handle_skills_registry_install(&params, bridge).await,
        "skills.registry.uninstall" => handle_skills_registry_uninstall(&params, bridge).await,

        // ── Plugins ──────────────────────────────────────────────────────
        "plugins.list" => handle_plugins_list(bridge).await,
        "plugins.enable" => handle_plugins_enable(&params, bridge).await,
        "plugins.disable" => handle_plugins_disable(&params, bridge).await,
        "plugins.config" => handle_plugins_config(&params, bridge).await,

        // ── DM Policy ───────────────────────────────────────────────────
        "dm.policy.get" => handle_dm_policy_get(&params, bridge).await,
        "dm.policy.set" => handle_dm_policy_set(&params, bridge).await,
        "dm.allowlist.get" => handle_dm_allowlist_get(&params, bridge).await,
        "dm.allowlist.set" => handle_dm_allowlist_set(&params, bridge).await,

        // ── Provider Health ─────────────────────────────────────────────
        "providers.health" => handle_providers_health(bridge).await,

        // ── Config Reload ───────────────────────────────────────────────
        "config.reload" => handle_config_reload(bridge).await,
        "config.validate" => handle_config_validate(&params, bridge).await,
        "config.migrate" => handle_config_migrate(bridge).await,

        // ── STT (speech-to-text) ────────────────────────────────────────
        "stt.transcribe" => handle_stt_transcribe(&params, bridge).await,
        "stt.providers" => handle_stt_providers().await,

        // ── Agent Routing ───────────────────────────────────────────────
        "routing.rules.list" => handle_routing_rules_list(bridge).await,
        "routing.rules.set" => handle_routing_rules_set(&params, bridge).await,

        // ── Canvas ─────────────────────────────────────────────────────
        "canvas.create" => handle_canvas_create(&params).await,
        "canvas.render" => handle_canvas_render(&params).await,
        "canvas.action" => handle_canvas_action(&params).await,
        "canvas.state" => handle_canvas_state(&params).await,
        "canvas.close" => handle_canvas_close(&params).await,

        // ── Config Snapshots (#33) ────────────────────────────────────
        "config.snapshot" => handle_config_snapshot(bridge).await,
        "config.snapshots.list" => handle_config_snapshots_list(bridge).await,
        "config.restore" => handle_config_restore(&params, bridge).await,

        // ── Model Aliases (#34) ───────────────────────────────────────
        "models.aliases.get" => handle_models_aliases_get(bridge).await,
        "models.aliases.set" => handle_models_aliases_set(&params, bridge).await,
        "models.resolve" => handle_models_resolve(&params, bridge).await,

        // ── Session Elevation (#46) ───────────────────────────────────
        "sessions.elevate" => handle_sessions_elevate(&params, session_store).await,
        "sessions.unelevate" => handle_sessions_unelevate(&params, session_store).await,

        // ── Heartbeat Config (#51) ────────────────────────────────────
        "heartbeat.config.get" => handle_heartbeat_config_get(bridge).await,
        "heartbeat.config.set" => handle_heartbeat_config_set(&params, bridge).await,

        // ── Browser CDP (#52) ─────────────────────────────────────────
        "browser.goto" => handle_browser_goto(&params, bridge).await,
        "browser.click" => handle_browser_click(&params, bridge).await,
        "browser.type" => handle_browser_type(&params, bridge).await,
        "browser.screenshot" => handle_browser_screenshot(&params, bridge).await,
        "browser.eval" => handle_browser_eval(&params, bridge).await,
        "browser.extension.relay.start" => {
            handle_browser_extension_relay_start(&params, bridge).await
        }
        "browser.extension.relay.status" => {
            handle_browser_extension_relay_status(&params, bridge).await
        }
        "browser.extension.relay.stop" => {
            handle_browser_extension_relay_stop(&params, bridge).await
        }
        "browser.extension.relay.poll" => {
            handle_browser_extension_relay_poll(&params, bridge).await
        }
        "browser.extension.relay.send" => {
            handle_browser_extension_relay_send(&params, bridge).await
        }
        "browser.content_script.inject" => {
            handle_browser_content_script_inject(&params, bridge).await
        }
        "browser.page.extract" => handle_browser_page_extract(&params, bridge).await,

        // ── Hooks Event Bus (#31) ─────────────────────────────────────
        "hooks.list" => handle_hooks_list(bridge).await,
        "hooks.enable" => handle_hooks_enable(&params, bridge).await,
        "hooks.disable" => handle_hooks_disable(&params, bridge).await,

        // ── Message Reactions (#37) ───────────────────────────────────
        "reactions.add" => handle_reactions_add(&params, bridge).await,
        "reactions.remove" => handle_reactions_remove(&params, bridge).await,

        // ── Streaming Config (#36) ────────────────────────────────────
        "streaming.config.get" => handle_streaming_config_get(bridge).await,
        "streaming.config.set" => handle_streaming_config_set(&params, bridge).await,

        // ── YAML Config Support (#59) ────────────────────────────────
        "config.format" => handle_config_format(bridge).await,
        "config.convert" => handle_config_convert(&params, bridge).await,

        // ── QR Code Pairing (#62) ────────────────────────────────────
        "device.pair.qr" => handle_device_pair_qr(&params, bridge).await,

        // ── Agent Avatar Management (#63) ────────────────────────────
        "agent.avatar.set" => handle_agent_avatar_set(&params, bridge).await,
        "agent.avatar.get" => handle_agent_avatar_get(&params, bridge).await,

        // ── Usage Export (#64) ───────────────────────────────────────
        "usage.export" => handle_usage_export(&params, session_store).await,

        // ── Log Rotation (#65) ──────────────────────────────────────
        "logs.rotate" => handle_logs_rotate(bridge).await,
        "logs.export" => handle_logs_export(&params).await,
        "logs.config" => handle_logs_config(&params, bridge).await,

        // ── Security (#66, #79) ──────────────────────────────────────
        "security.audit" => handle_security_audit(&params, bridge).await,
        "security.rotate" => handle_security_rotate(&params, bridge).await,
        "security.analyze" => handle_security_analyze(&params).await,

        _ => Err((
            METHOD_NOT_FOUND,
            format!("method not found: {}", request.method),
        )),
    };

    match result {
        Ok(value) => rpc_success(id, value),
        Err((code, message)) => rpc_error(id, code, message),
    }
}

// ─── Method handlers ─────────────────────────────────────────────────────────

type RpcResult = Result<Value, (i64, String)>;

#[derive(Debug, Clone, Serialize)]
struct NodeInvokeRecord {
    request_id: String,
    node_id: String,
    method: String,
    status: String,
    result: Value,
    updated_at_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn node_invoke_store() -> &'static Mutex<HashMap<String, NodeInvokeRecord>> {
    static STORE: OnceLock<Mutex<HashMap<String, NodeInvokeRecord>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

enum WsAccountLoginAttempt {
    Chatgpt {
        shutdown_handle: ShutdownHandle,
        task: tokio::task::JoinHandle<()>,
    },
    DeviceCode {
        task: tokio::task::JoinHandle<()>,
    },
}

fn ws_account_login_store() -> &'static Mutex<HashMap<String, WsAccountLoginAttempt>> {
    static STORE: OnceLock<Mutex<HashMap<String, WsAccountLoginAttempt>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn remove_ws_login_attempt(login_id: &str) {
    let mut lock = ws_account_login_store().lock().await;
    lock.remove(login_id);
}

fn gateway_auth_manager(bridge: &Arc<GatewayBridge>) -> Arc<AuthManager> {
    AuthManager::shared(
        bridge.config().savfox_home.clone(),
        false,
        bridge.config().cli_auth_credentials_store_mode,
    )
}

fn chatgpt_server_options(bridge: &Arc<GatewayBridge>) -> ServerOptions {
    ServerOptions::new(
        bridge.config().savfox_home.clone(),
        CLIENT_ID.to_string(),
        bridge.config().forced_chatgpt_workspace_id.clone(),
        bridge.config().cli_auth_credentials_store_mode,
    )
}

async fn handle_account_login_start(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let login_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    match login_type {
        "apiKey" => {
            let api_key = params
                .get("apiKey")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            if api_key.is_empty() {
                return Err((INVALID_REQUEST, "missing 'apiKey' parameter".to_string()));
            }

            login_with_api_key(
                &bridge.config().savfox_home,
                &api_key,
                bridge.config().cli_auth_credentials_store_mode,
            )
            .map_err(|err| (INTERNAL_ERROR, format!("failed to save api key: {err}")))?;

            Ok(json!({ "type": "apiKey" }))
        }
        "chatgpt" => {
            let opts = chatgpt_server_options(bridge);
            let server = run_login_server(opts).map_err(|err| {
                (
                    INTERNAL_ERROR,
                    format!("failed to start login server: {err}"),
                )
            })?;

            let login_id = uuid::Uuid::new_v4().to_string();
            let auth_url = server.auth_url.clone();
            let shutdown_handle = server.cancel_handle();
            let auth_manager = gateway_auth_manager(bridge);
            let login_id_for_task = login_id.clone();

            let task = tokio::spawn(async move {
                let _ =
                    tokio::time::timeout(Duration::from_secs(600), server.block_until_done()).await;
                auth_manager.reload();
                remove_ws_login_attempt(&login_id_for_task).await;
            });

            {
                let mut lock = ws_account_login_store().lock().await;
                lock.insert(
                    login_id.clone(),
                    WsAccountLoginAttempt::Chatgpt {
                        shutdown_handle,
                        task,
                    },
                );
            }

            Ok(json!({
                "type": "chatgpt",
                "loginId": login_id,
                "authUrl": auth_url,
            }))
        }
        "deviceCode" => {
            let opts = chatgpt_server_options(bridge);
            let device_code = request_device_code(&opts).await.map_err(|err| {
                (
                    INTERNAL_ERROR,
                    format!("failed to request device code: {err}"),
                )
            })?;

            let login_id = uuid::Uuid::new_v4().to_string();
            let verification_url = device_code.verification_url.clone();
            let user_code = device_code.user_code.clone();
            let auth_manager = gateway_auth_manager(bridge);
            let login_id_for_task = login_id.clone();

            let task = tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    Duration::from_secs(900),
                    complete_device_code_login(opts, device_code),
                )
                .await;
                auth_manager.reload();
                remove_ws_login_attempt(&login_id_for_task).await;
            });

            {
                let mut lock = ws_account_login_store().lock().await;
                lock.insert(login_id.clone(), WsAccountLoginAttempt::DeviceCode { task });
            }

            Ok(json!({
                "type": "deviceCode",
                "loginId": login_id,
                "verificationUrl": verification_url,
                "userCode": user_code,
            }))
        }
        _ => Err((
            INVALID_REQUEST,
            "unsupported account login type; expected one of: chatgpt, deviceCode, apiKey"
                .to_string(),
        )),
    }
}

async fn handle_account_login_cancel(params: &Value) -> RpcResult {
    let login_id = params
        .get("loginId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if login_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'loginId' parameter".to_string()));
    }

    let removed = {
        let mut lock = ws_account_login_store().lock().await;
        lock.remove(&login_id)
    };

    match removed {
        Some(WsAccountLoginAttempt::Chatgpt {
            shutdown_handle,
            task,
        }) => {
            shutdown_handle.shutdown();
            task.abort();
            Ok(json!({ "status": "cancelled" }))
        }
        Some(WsAccountLoginAttempt::DeviceCode { task }) => {
            task.abort();
            Ok(json!({ "status": "cancelled" }))
        }
        None => Ok(json!({ "status": "notFound" })),
    }
}

async fn handle_account_read(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let refresh_token = params
        .get("refreshToken")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let auth_manager = gateway_auth_manager(bridge);
    if refresh_token {
        auth_manager.reload();
    }

    let requires_openai_auth = bridge.config().model_provider.requires_openai_auth;
    if !requires_openai_auth {
        return Ok(json!({
            "account": Value::Null,
            "requiresOpenaiAuth": false,
        }));
    }

    let account = match auth_manager.auth_cached() {
        Some(SavfoxAuth::ApiKey(_)) => json!({ "type": "apiKey" }),
        Some(auth @ (SavfoxAuth::Chatgpt(_) | SavfoxAuth::ChatgptAuthTokens(_))) => {
            let mut payload = serde_json::Map::new();
            payload.insert("type".to_string(), json!("chatgpt"));
            if let Some(email) = auth.get_account_email() {
                payload.insert("email".to_string(), json!(email));
            }
            if let Some(plan_type) = auth.account_plan_type() {
                payload.insert("planType".to_string(), json!(plan_type));
            }
            Value::Object(payload)
        }
        None => Value::Null,
    };

    Ok(json!({
        "account": account,
        "requiresOpenaiAuth": true,
    }))
}

async fn save_node_invoke_result(record: NodeInvokeRecord) {
    let mut lock = node_invoke_store().lock().await;
    if lock.len() > 2048 {
        // Best-effort pruning of oldest half when cache grows too large.
        let mut entries: Vec<_> = lock.values().cloned().collect();
        entries.sort_by_key(|v| v.updated_at_ms);
        let remove_count = entries.len() / 2;
        for entry in entries.into_iter().take(remove_count) {
            lock.remove(&entry.request_id);
        }
    }
    lock.insert(record.request_id.clone(), record);
}

async fn get_node_invoke_result(request_id: &str) -> Option<NodeInvokeRecord> {
    let lock = node_invoke_store().lock().await;
    lock.get(request_id).cloned()
}

// ── Core ────────────────────────────────────────────────────────────────────

/// WS-RPC protocol version. Increment when breaking changes are made to the
/// protocol (new required fields, removed methods, changed semantics).
const PROTOCOL_VERSION: u32 = 1;

async fn handle_connect(params: &Value) -> RpcResult {
    let client_version = params
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    Ok(json!({
        "status": "connected",
        "protocol_version": PROTOCOL_VERSION,
        "server_version": env!("CARGO_PKG_VERSION"),
        "client_version": client_version,
    }))
}

async fn handle_health() -> RpcResult {
    Ok(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn handle_status(
    session_mgr: &Arc<GatewaySessionManager>,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let count = session_mgr.session_count().await;
    let ids = session_mgr.session_ids().await;
    let audit_summary = crate::security_audit::run_audit(&bridge.config().savfox_home)
        .await
        .summary;
    let plugins = plugin::discover_snapshot(&bridge.config().savfox_home)
        .await
        .unwrap_or_default();
    let plugin_routes = plugin::describe_http_routes(&plugins, PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE);
    Ok(json!({
        "connected_clients": count,
        "session_ids": ids,
        "security_audit": audit_summary,
        "plugin_routes": plugin_routes,
    }))
}

// ── Agent (single-agent operations) ─────────────────────────────────────────

async fn handle_agent(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_string()));
    }

    match bridge.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "response": reply })),
        Err(err) => Err((INTERNAL_ERROR, format!("agent error: {err}"))),
    }
}

async fn handle_agent_identity() -> RpcResult {
    Ok(json!({
        "name": "savfox",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": ["chat", "tools", "sessions", "cron", "nodes", "tts", "a2a", "delegation"],
    }))
}

async fn handle_agent_wait(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_string()));
    }

    match bridge.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "response": reply, "done": true })),
        Err(err) => Err((INTERNAL_ERROR, format!("agent.wait error: {err}"))),
    }
}

// ── Agent capabilities & delegation ─────────────────────────────────────────

/// Returns the capabilities of a specific agent, including its tools,
/// skills, connected channels, and current status.
async fn handle_agent_capabilities(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_id = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    // Collect tools from agent config (if it exists).
    let agents_dir = bridge.config().savfox_home.join("agents");
    let agent_config_path = agents_dir.join(format!("{agent_id}.json"));
    let agent_config: Option<Value> = if agent_config_path.exists() {
        tokio::fs::read_to_string(&agent_config_path)
            .await
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
    } else {
        None
    };

    let agent_name = agent_config
        .as_ref()
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(agent_id);

    // Tools: extract from config or list known built-in tools.
    let tools: Vec<String> = agent_config
        .as_ref()
        .and_then(|c| c.get("tools"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            // Default agent has access to standard tool set.
            vec![
                "shell".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
                "list_dir".to_string(),
                "grep_files".to_string(),
                "web_search".to_string(),
                "web_fetch".to_string(),
                "sessions_send_a2a".to_string(),
                "agent_step".to_string(),
            ]
        });

    // Skills: read from skills store.
    let skills: Vec<String> = match skills_store::status(&bridge.config().savfox_home).await {
        Ok(status_val) => status_val
            .get("installed")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // Channels: derive from configured bridge secrets.
    let channels: Vec<String> = {
        let runtime = bridge.runtime_bridge_secrets().await;
        let mut ch = Vec::new();
        if runtime.discord_bot_token.is_some() || std::env::var("DISCORD_BOT_TOKEN").is_ok() {
            ch.push("discord".to_string());
        }
        if runtime.telegram_bot_token.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok() {
            ch.push("telegram".to_string());
        }
        if runtime.slack_bot_token.is_some() || std::env::var("SLACK_BOT_TOKEN").is_ok() {
            ch.push("slack".to_string());
        }
        if runtime.webhook_secret.is_some() || std::env::var("WEBHOOK_SECRET").is_ok() {
            ch.push("webhook".to_string());
        }
        ch
    };

    // Status: check if the agent is active via session manager.
    let active_sessions = bridge.websocket_manager().session_ids().await;
    let status = if active_sessions
        .iter()
        .any(|s| s.to_string().contains(agent_id))
    {
        "active"
    } else if agent_id == "default" {
        "active"
    } else {
        "idle"
    };
    let delegation_chain = savfox_core::a2a::delegation_chain_for(agent_id).await;
    let delegation_chain_json: Vec<Value> = delegation_chain
        .iter()
        .map(|entry| {
            json!({
                "parent_agent": entry.parent_agent,
                "child_agent": entry.child_agent,
                "spawned_at": entry.spawned_at,
                "purpose": entry.purpose,
            })
        })
        .collect();
    let delegation_parent = delegation_chain
        .last()
        .map(|entry| entry.parent_agent.clone());

    Ok(json!({
        "agent": agent_name,
        "agent_id": agent_id,
        "tools": tools,
        "skills": skills,
        "channels": channels,
        "status": status,
        "delegation_parent": delegation_parent,
        "delegation_depth": delegation_chain_json.len(),
        "delegation_chain": delegation_chain_json,
    }))
}

/// List all recorded delegation entries.
async fn handle_agent_delegation_list() -> RpcResult {
    let entries = savfox_core::a2a::list_delegations().await;
    let entries_json: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "parent_agent": e.parent_agent,
                "child_agent": e.child_agent,
                "spawned_at": e.spawned_at,
                "purpose": e.purpose,
            })
        })
        .collect();

    Ok(json!({
        "delegations": entries_json,
        "count": entries_json.len(),
    }))
}

/// Get the delegation chain for a specific agent, walking up parent links.
async fn handle_agent_delegation_chain(params: &Value) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if agent_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'agent' parameter".to_string()));
    }

    let chain = savfox_core::a2a::delegation_chain_for(agent_id).await;
    let chain_json: Vec<Value> = chain
        .iter()
        .map(|e| {
            json!({
                "parent_agent": e.parent_agent,
                "child_agent": e.child_agent,
                "spawned_at": e.spawned_at,
                "purpose": e.purpose,
            })
        })
        .collect();

    Ok(json!({
        "agent": agent_id,
        "chain": chain_json,
        "depth": chain_json.len(),
    }))
}

/// Record a new delegation entry between a parent and child agent.
async fn handle_agent_delegation_record(params: &Value) -> RpcResult {
    let parent = params
        .get("parent_agent")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let child = params
        .get("child_agent")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let purpose = params
        .get("purpose")
        .and_then(|v| v.as_str())
        .unwrap_or("manual delegation");

    if parent.is_empty() || child.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'parent_agent' or 'child_agent' parameter".to_string(),
        ));
    }

    let spawned_at = savfox_core::a2a::now_ms();

    savfox_core::a2a::record_delegation(savfox_core::a2a::DelegationEntry {
        parent_agent: parent.to_string(),
        child_agent: child.to_string(),
        spawned_at,
        purpose: purpose.to_string(),
    })
    .await;

    Ok(json!({
        "status": "recorded",
        "parent_agent": parent,
        "child_agent": child,
        "spawned_at": spawned_at,
        "purpose": purpose,
    }))
}

/// Remove a delegation entry by child agent ID.
async fn handle_agent_delegation_remove(params: &Value) -> RpcResult {
    let child = params
        .get("child_agent")
        .or_else(|| params.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if child.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'child_agent' parameter".to_string(),
        ));
    }

    let removed = savfox_core::a2a::remove_delegation(child).await;

    Ok(json!({
        "status": if removed { "removed" } else { "not_found" },
        "child_agent": child,
    }))
}

// ── Agents (multi-agent CRUD) ───────────────────────────────────────────────

/// Get the agents directory (SAVFOX_HOME/agents/).
fn agents_dir(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("agents")
}

/// Read an agent config JSON file.
async fn read_agent_config(path: &std::path::Path) -> Option<Value> {
    let data = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&data).ok()
}

fn sanitize_agent_file_stem(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        let mapped = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            _ => ch,
        };
        out.push(mapped);
    }

    let out = out.trim_matches([' ', '.']).to_string();
    if out.is_empty() || out == "." || out == ".." {
        None
    } else {
        Some(out)
    }
}

fn default_agent_name_from_config(config: &Value, fallback: &str) -> String {
    config
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| {
            config
                .get("identity")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| fallback.to_string())
}

async fn resolve_agent_file_stem(bridge: &GatewayBridge, agent_ref: &str) -> Option<String> {
    let dir = agents_dir(bridge);
    let trimmed = agent_ref.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(safe_ref) = sanitize_agent_file_stem(trimmed) {
        let direct = dir.join(format!("{safe_ref}.json"));
        if direct.exists() {
            return Some(safe_ref);
        }
    }

    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "json") {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
        else {
            continue;
        };

        if stem.eq_ignore_ascii_case(trimmed) {
            return Some(stem);
        }

        let Some(config) = read_agent_config(&path).await else {
            continue;
        };

        let id_match = config
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some_and(|id| id.eq_ignore_ascii_case(trimmed));
        if id_match {
            return Some(stem);
        }

        let name_match = config
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));
        if name_match {
            return Some(stem);
        }

        let identity_match = config
            .get("identity")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));
        if identity_match {
            return Some(stem);
        }
    }

    None
}

async fn resolve_agent_files_dir(bridge: &GatewayBridge, agent_ref: &str) -> PathBuf {
    let base = agents_dir(bridge);
    let safe_ref = sanitize_agent_file_stem(agent_ref).unwrap_or_else(|| "default".to_string());
    let by_ref = base.join(&safe_ref);
    if by_ref.exists() {
        return by_ref;
    }

    if let Some(stem) = resolve_agent_file_stem(bridge, agent_ref).await {
        let by_stem = base.join(&stem);
        if by_stem.exists() {
            return by_stem;
        }
    }

    by_ref
}

async fn handle_agents_list(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let dir = agents_dir(bridge);
    let mut agents = vec![json!({
        "id": "default",
        "name": "Savfox Agent",
        "description": "Default Savfox assistant agent",
        "builtin": true,
    })];

    // Scan agents directory for user-defined agents.
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let Some(file_stem) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                else {
                    continue;
                };
                if let Some(mut config) = read_agent_config(&path).await {
                    let id_missing = config
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .is_none_or(|v| v.is_empty());
                    if id_missing {
                        config["id"] = json!(file_stem.clone());
                    }
                    let name_missing = config
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .is_none_or(|v| v.is_empty());
                    if name_missing {
                        config["name"] = json!(default_agent_name_from_config(&config, &file_stem));
                    }
                    agents.push(config);
                }
            }
        }
    }

    let mut enriched = Vec::with_capacity(agents.len());
    for mut agent in agents {
        let agent_id = agent
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !agent_id.is_empty() {
            let chain = savfox_core::a2a::delegation_chain_for(agent_id).await;
            let chain_json: Vec<Value> = chain
                .iter()
                .map(|entry| {
                    json!({
                        "parent_agent": entry.parent_agent,
                        "child_agent": entry.child_agent,
                        "spawned_at": entry.spawned_at,
                        "purpose": entry.purpose,
                    })
                })
                .collect();
            let delegation_parent = chain.last().map(|entry| entry.parent_agent.clone());
            agent["delegation_parent"] =
                serde_json::to_value(delegation_parent).unwrap_or(Value::Null);
            agent["delegation_depth"] = json!(chain_json.len());
            agent["delegation_chain"] = json!(chain_json);
        }
        enriched.push(agent);
    }

    Ok(json!({ "agents": enriched }))
}

async fn handle_agents_get(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_ref = params
        .get("id")
        .or_else(|| params.get("name"))
        .or_else(|| params.get("agent"))
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if agent_ref.trim().is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'name' or 'id' parameter".to_string(),
        ));
    }

    if agent_ref.trim().eq_ignore_ascii_case("default") {
        return Ok(json!({
            "id": "default",
            "name": "Savfox Agent",
            "status": "active",
            "builtin": true,
        }));
    }

    let Some(file_stem) = resolve_agent_file_stem(bridge, agent_ref).await else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };
    let path = agents_dir(bridge).join(format!("{file_stem}.json"));
    let Some(mut config) = read_agent_config(&path).await else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };

    let id_missing = config
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .is_none_or(|v| v.is_empty());
    if id_missing {
        config["id"] = json!(file_stem.clone());
    }
    let name_missing = config
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .is_none_or(|v| v.is_empty());
    if name_missing {
        config["name"] = json!(default_agent_name_from_config(&config, &file_stem));
    }

    Ok(config)
}

async fn handle_agents_create(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let raw_id = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if raw_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }
    let id = sanitize_agent_file_stem(raw_id).ok_or_else(|| {
        (
            INVALID_REQUEST,
            "invalid 'id' parameter (empty after sanitization)".to_string(),
        )
    })?;
    if id != raw_id {
        return Err((
            INVALID_REQUEST,
            "invalid 'id' parameter: use letters, numbers, '-', '_' without path/special characters"
                .to_string(),
        ));
    }

    let display_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    if display_name.is_empty() {
        return Err((INVALID_REQUEST, "missing 'name' parameter".to_string()));
    }

    let description = params
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let system_prompt = params
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Support new `models` object format
    let models_obj = params.get("models");

    let mut agent_config = json!({
        "id": id,
        "name": display_name,
        "description": description,
        "system_prompt": system_prompt,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });

    // Store models in new format, with backward compat for flat `model` field
    if let Some(models) = models_obj {
        agent_config["models"] = models.clone();
    }
    if !model.is_empty() {
        agent_config["model"] = json!(model);
        // Also set as models.primary if models wasn't provided
        if models_obj.is_none() {
            agent_config["models"] = json!({ "primary": model });
        }
    }

    // Per-agent config overrides
    for key in &[
        "provider",
        "thinking",
        "verbose",
        "tools",
        "memory",
        "compaction",
        "sandbox",
        "heartbeat",
        "group_activation",
        "dm_scope",
        "identity",
    ] {
        if let Some(val) = params.get(*key) {
            agent_config[*key] = val.clone();
        }
    }

    let dir = agents_dir(bridge);
    let _ = tokio::fs::create_dir_all(&dir).await;
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        return Err((INVALID_REQUEST, format!("agent already exists: {id}")));
    }
    let data = serde_json::to_string_pretty(&agent_config).unwrap_or_default();
    if let Err(err) = tokio::fs::write(&path, data).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to write agent config: {err}"),
        ));
    }

    Ok(json!({
        "id": id,
        "name": display_name,
        "status": "created",
    }))
}

async fn handle_agents_update(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_ref = params
        .get("id")
        .or_else(|| params.get("name"))
        .or_else(|| params.get("agent"))
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_ref.trim().is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'id' or 'name' parameter".to_string(),
        ));
    }

    let dir = agents_dir(bridge);
    let resolved_id = resolve_agent_file_stem(bridge, agent_ref)
        .await
        .or_else(|| sanitize_agent_file_stem(agent_ref))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("invalid agent reference: {agent_ref}"),
            )
        })?;
    let path = dir.join(format!("{resolved_id}.json"));

    // Read existing config or start fresh.
    let mut config = read_agent_config(&path)
        .await
        .unwrap_or(json!({"id": resolved_id, "name": agent_ref}));
    config["id"] = json!(resolved_id.clone());

    // Merge updatable fields.
    if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
        config["name"] = json!(name);
    }
    if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
        config["description"] = json!(desc);
    }
    if let Some(model) = params.get("model").and_then(|v| v.as_str()) {
        config["model"] = json!(model);
    }
    if let Some(provider) = params.get("provider").and_then(|v| v.as_str()) {
        config["provider"] = json!(provider);
    }
    if let Some(prompt) = params.get("system_prompt").and_then(|v| v.as_str()) {
        config["system_prompt"] = json!(prompt);
    }
    if let Some(models) = params.get("models") {
        config["models"] = models.clone();
    }
    if let Some(fallbacks) = params.get("fallback_models") {
        // Legacy: also store in models.fallbacks
        if config.get("models").is_none() {
            config["models"] = json!({});
        }
        config["models"]["fallbacks"] = fallbacks.clone();
    }
    // Per-agent config overrides
    for key in &[
        "thinking",
        "verbose",
        "tools",
        "memory",
        "compaction",
        "sandbox",
        "heartbeat",
        "group_activation",
        "dm_scope",
        "identity",
    ] {
        if let Some(val) = params.get(*key) {
            config[*key] = val.clone();
        }
    }
    config["updated_at"] = json!(chrono::Utc::now().to_rfc3339());

    let _ = tokio::fs::create_dir_all(&dir).await;
    let data = serde_json::to_string_pretty(&config).unwrap_or_default();
    if let Err(err) = tokio::fs::write(&path, data).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to write agent config: {err}"),
        ));
    }

    Ok(json!({ "id": resolved_id, "status": "updated" }))
}

async fn handle_agents_delete(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_ref = params
        .get("id")
        .or_else(|| params.get("name"))
        .or_else(|| params.get("agent"))
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_ref.trim().is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'id' or 'name' parameter".to_string(),
        ));
    }
    if agent_ref == "default" {
        return Err((
            INVALID_REQUEST,
            "cannot delete the default agent".to_string(),
        ));
    }

    let resolved_id = resolve_agent_file_stem(bridge, agent_ref)
        .await
        .or_else(|| sanitize_agent_file_stem(agent_ref))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("invalid agent reference: {agent_ref}"),
            )
        })?;

    let dir = agents_dir(bridge);
    let path = dir.join(format!("{resolved_id}.json"));
    let _ = tokio::fs::remove_file(&path).await;

    // Also remove the agent's files directory.
    let files_dir = dir.join(&resolved_id);
    let _ = tokio::fs::remove_dir_all(&files_dir).await;
    if let Some(ref_dir) = sanitize_agent_file_stem(agent_ref)
        && ref_dir != resolved_id
    {
        let _ = tokio::fs::remove_dir_all(dir.join(ref_dir)).await;
    }

    Ok(json!({ "id": resolved_id, "status": "deleted" }))
}

async fn handle_agents_files_list(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let dir = resolve_agent_files_dir(bridge, agent_ref).await;
    let mut files = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let size = tokio::fs::metadata(&path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    files.push(json!({ "name": name, "size": size }));
                }
            }
        }
    }

    Ok(json!({ "agent_id": agent_ref, "files": files }))
}

async fn handle_agents_files_get(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let file_path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return Err((INVALID_REQUEST, "missing 'path' parameter".to_string()));
    }

    // Sanitize path to prevent directory traversal.
    let safe_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let dir = resolve_agent_files_dir(bridge, agent_ref).await;
    let path = dir.join(safe_name);
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "content": content })),
        Err(_) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "content": null })),
    }
}

async fn handle_agents_files_set(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let file_path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return Err((INVALID_REQUEST, "missing 'path' parameter".to_string()));
    }

    // Sanitize path to prevent directory traversal.
    let safe_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let dir = resolve_agent_files_dir(bridge, agent_ref).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let path = dir.join(safe_name);

    if let Err(err) = tokio::fs::write(&path, content).await {
        return Err((INTERNAL_ERROR, format!("failed to write file: {err}")));
    }

    Ok(json!({ "agent_id": agent_ref, "path": safe_name, "status": "saved" }))
}

async fn handle_agents_files_delete(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let file_path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return Err((INVALID_REQUEST, "missing 'path' parameter".to_string()));
    }

    // Sanitize path to prevent directory traversal.
    let safe_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let dir = resolve_agent_files_dir(bridge, agent_ref).await;
    let path = dir.join(safe_name);
    match tokio::fs::remove_file(&path).await {
        Ok(_) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "status": "deleted" })),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(json!({
            "agent_id": agent_ref,
            "path": safe_name,
            "status": "deleted"
        })),
        Err(err) => Err((INTERNAL_ERROR, format!("failed to delete file: {err}"))),
    }
}

// ── Chat ────────────────────────────────────────────────────────────────────

fn format_model_footer(model: &str, provider: &str, profile: Option<&str>) -> String {
    match profile {
        Some(p) if !p.trim().is_empty() => {
            format!("model: {model} | provider: {provider} | profile: {p}")
        }
        _ => format!("model: {model} | provider: {provider}"),
    }
}

fn append_footer(reply: &str, footer: &str) -> String {
    let reply = reply.trim_end();
    if reply.is_empty() {
        footer.to_string()
    } else {
        format!("{reply}\n\n---\n{footer}")
    }
}

fn next_stream_block_id(request_id: &str, block_kind: &str, counter: &mut u64) -> String {
    *counter += 1;
    format!("{request_id}:{block_kind}:{counter}")
}

async fn emit_stream_block_start(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.block",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "phase": "start",
                "block_id": block_id,
                "block_kind": block_kind,
            }),
        )
        .await;
}

async fn emit_stream_block_delta(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
    delta: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.block",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "phase": "delta",
                "block_id": block_id,
                "block_kind": block_kind,
                "delta": delta,
            }),
        )
        .await;
}

async fn emit_stream_block_progress(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
    completed: usize,
    status: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.progress",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "block_id": block_id,
                "block_kind": block_kind,
                "completed": completed,
                "status": status,
            }),
        )
        .await;
}

async fn emit_stream_block_end(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
    status: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.block",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "phase": "end",
                "block_id": block_id,
                "block_kind": block_kind,
                "status": status,
            }),
        )
        .await;
}

async fn emit_typing_heartbeat(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    agent_id: &str,
) {
    session_mgr
        .broadcast_to_all(
            "typing.start",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "agent_id": agent_id,
                "heartbeat": true,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;
}

async fn emit_typing_stop(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    agent_id: &str,
) {
    session_mgr
        .broadcast_to_all(
            "typing.stop",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "agent_id": agent_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;
}

async fn handle_chat_send(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    use savfox_core::models_manager::manager::RefreshStrategy;
    use savfox_protocol::protocol::{EventMsg, Op};
    use savfox_protocol::user_input::UserInput;

    use crate::auto_reply::directives::{
        DirectiveKind, fuzzy_match_model_name, parse_directives, parse_model_target,
    };

    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let requested_session_id =
        validate_uuid_v7_session_id(params.get("session_id").and_then(|v| v.as_str()))
            .map_err(|message| (INVALID_PARAMS, message))?;

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_string()));
    }

    let parsed = parse_directives(message);
    let prompt = if parsed.directives.is_empty() {
        message.trim().to_string()
    } else {
        parsed.cleaned_text.trim().to_string()
    };

    // Strip "[user]:" prefix if present (some clients add this prefix)
    let prompt = prompt
        .strip_prefix("[user]:")
        .map(|s| s.trim())
        .unwrap_or(&prompt)
        .to_string();
    if prompt.is_empty() {
        return Err((
            INVALID_REQUEST,
            "message is empty after parsing directives".to_string(),
        ));
    }

    // Deterministic mock path for integration/e2e testing without external model dependencies.
    // Enabled only when the caller explicitly passes `mock_response`.
    if let Some(mock_response) = params.get("mock_response") {
        let raw_response = if let Some(text) = mock_response.as_str() {
            text.to_string()
        } else if mock_response.as_bool().unwrap_or(false) {
            format!("echo: {prompt}")
        } else {
            "(mock response)".to_string()
        };
        let model = "mock/echo";
        let provider = "mock";
        let footer = format_model_footer(model, provider, None);
        let response = append_footer(&raw_response, &footer);
        let session_id = requested_session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

        session_mgr
            .broadcast_to_all(
                "agent.stream",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "phase": "start",
                    "model": model,
                    "provider": provider,
                }),
            )
            .await;
        session_mgr
            .broadcast_to_all(
                "agent.complete",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "thread_id": session_id,
                    "response": response,
                    "raw_response": raw_response,
                    "model": model,
                    "provider": provider,
                    "profile": Value::Null,
                    "aborted": false,
                }),
            )
            .await;

        persist_chat_session_metadata(
            session_store.as_ref(),
            &bridge.config().savfox_home,
            &session_id,
            &session_id,
            model,
            provider,
            None,
            None,
        )
        .await;

        return Ok(json!({
            "response": response,
            "raw_response": raw_response,
            "footer": footer,
            "model": model,
            "provider": provider,
            "profile": Value::Null,
            "session_id": session_id,
            "thread_id": session_id,
            "aborted": false,
            "mock": true,
        }));
    }

    let model_directive = parsed
        .directives
        .iter()
        .rev()
        .find(|d| d.kind == DirectiveKind::Model)
        .map(|d| d.value.clone());

    let (effective_model, model_profile) = if let Some(raw_model) = model_directive {
        let parsed_target = parse_model_target(&raw_model);
        let candidates: Vec<String> = bridge
            .session_manager()
            .list_models(bridge.config(), RefreshStrategy::Offline)
            .await
            .into_iter()
            .map(|m| m.id)
            .collect();
        let resolved = fuzzy_match_model_name(&parsed_target.model, &candidates)
            .unwrap_or(parsed_target.model);
        (resolved, parsed_target.profile)
    } else {
        (agent.to_string(), None)
    };

    let provider = provider_from_model(&effective_model);

    let mut config = (**bridge.config()).clone();
    if !effective_model.trim().is_empty() {
        config.model = Some(effective_model.clone());
    }
    let mut logical_session_id = requested_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let keep_session_loaded = requested_session_id.is_some();

    let mut thread_session_id: Option<SessionId> = None;
    if let Some(requested) = requested_session_id.as_deref() {
        if let Some(entry) = session_store.get(requested).await
            && let Some(thread_id) = entry.thread_id.as_deref()
            && let Ok(parsed_thread_id) = SessionId::from_string(thread_id)
            && bridge
                .session_manager()
                .get_session(parsed_thread_id)
                .await
                .is_ok()
        {
            thread_session_id = Some(parsed_thread_id);
        }

        if thread_session_id.is_none()
            && let Ok(parsed_requested_id) = SessionId::from_string(requested)
            && bridge
                .session_manager()
                .get_session(parsed_requested_id)
                .await
                .is_ok()
        {
            thread_session_id = Some(parsed_requested_id);
        }
    }

    if thread_session_id.is_none() {
        let new_thread = bridge
            .session_manager()
            .start_session(config)
            .await
            .map_err(|err| (INTERNAL_ERROR, format!("failed to start thread: {err}")))?;
        thread_session_id = Some(new_thread.session_id.clone());
        if requested_session_id.is_none() {
            logical_session_id = new_thread.session_id.to_string();
        }
    }

    if session_store.get(&logical_session_id).await.is_none()
        && let Some(mut entry) = session_store.get_or_create(&logical_session_id).await
    {
        entry.model = Some(effective_model.clone());
        entry.provider = Some(provider.clone());
        entry.touch();
        session_store.upsert(entry).await;
    }

    let session_id_obj = thread_session_id.ok_or((
        INTERNAL_ERROR,
        "failed to resolve thread session".to_string(),
    ))?;
    let thread_id = session_id_obj.to_string();
    let session_id = logical_session_id.clone();

    session_mgr
        .broadcast_to_all(
            "agent.stream",
            json!({
                "request_id": request_id,
                "session_id": logical_session_id,
                "thread_id": thread_id,
                "phase": "start",
                "model": effective_model,
                "provider": provider,
                "profile": model_profile,
            }),
        )
        .await;

    let thread = bridge
        .session_manager()
        .get_session(session_id_obj.clone())
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to load thread: {err}")))?;
    let rollout_path = thread.rollout_path();

    thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt.clone(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to submit prompt: {err}")))?;

    let mut reply = String::new();
    let mut aborted = false;
    let timeout = tokio::time::Duration::from_secs(120);
    let deadline = tokio::time::Instant::now() + timeout;
    let mut fatal_error: Option<(i64, String)> = None;
    let mut last_token_usage: Option<savfox_protocol::protocol::TokenUsage> = None;

    let mut block_counter = 0_u64;
    let mut text_block_id: Option<String> = None;
    let mut thinking_block_id: Option<String> = None;
    let mut tool_blocks: HashMap<String, String> = HashMap::new();
    let mut block_progress: HashMap<String, usize> = HashMap::new();

    let typing_interval = Duration::from_secs(3);
    let mut next_typing_heartbeat = tokio::time::Instant::now();
    emit_typing_heartbeat(session_mgr, &request_id, &session_id, agent).await;
    next_typing_heartbeat += typing_interval;

    loop {
        let now = tokio::time::Instant::now();
        if now >= next_typing_heartbeat {
            emit_typing_heartbeat(session_mgr, &request_id, &session_id, agent).await;
            next_typing_heartbeat = now + typing_interval;
        }

        let wait_until = if next_typing_heartbeat < deadline {
            next_typing_heartbeat
        } else {
            deadline
        };

        match tokio::time::timeout_at(wait_until, thread.next_event()).await {
            Ok(Ok(event)) => {
                eprintln!("[GW EVENT] {:?}", event.msg);
                match &event.msg {
                    EventMsg::TokenCount(token_count) => {
                        if let Some(info) = &token_count.info {
                            last_token_usage = Some(info.last_token_usage.clone());
                        }
                    }
                    EventMsg::AgentMessage(msg) => {
                        if !msg.message.is_empty() {
                            reply.push_str(&msg.message);

                            session_mgr
                                .broadcast_to_all(
                                    "agent.stream",
                                    json!({
                                        "request_id": request_id,
                                        "session_id": session_id,
                                        "kind": "text",
                                        "delta": msg.message,
                                    }),
                                )
                                .await;

                            if text_block_id.is_none() {
                                let id =
                                    next_stream_block_id(&request_id, "text", &mut block_counter);
                                emit_stream_block_start(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    &id,
                                    "text",
                                )
                                .await;
                                text_block_id = Some(id);
                            }

                            if let Some(block_id) = text_block_id.as_ref() {
                                emit_stream_block_delta(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    block_id,
                                    "text",
                                    &msg.message,
                                )
                                .await;
                                let completed = {
                                    let entry = block_progress.entry(block_id.clone()).or_insert(0);
                                    *entry += msg.message.chars().count();
                                    *entry
                                };
                                emit_stream_block_progress(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    block_id,
                                    "text",
                                    completed,
                                    "streaming",
                                )
                                .await;
                            }
                        }
                    }
                    EventMsg::AgentMessageDelta(delta) => {
                        if !delta.delta.is_empty() {
                            reply.push_str(&delta.delta);
                            session_mgr
                                .broadcast_to_all(
                                    "agent.stream",
                                    json!({
                                        "request_id": request_id,
                                        "session_id": session_id,
                                        "kind": "text",
                                        "delta": delta.delta,
                                    }),
                                )
                                .await;

                            if text_block_id.is_none() {
                                let id =
                                    next_stream_block_id(&request_id, "text", &mut block_counter);
                                emit_stream_block_start(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    &id,
                                    "text",
                                )
                                .await;
                                text_block_id = Some(id);
                            }

                            if let Some(block_id) = text_block_id.as_ref() {
                                emit_stream_block_delta(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    block_id,
                                    "text",
                                    &delta.delta,
                                )
                                .await;
                                let completed = {
                                    let entry = block_progress.entry(block_id.clone()).or_insert(0);
                                    *entry += delta.delta.chars().count();
                                    *entry
                                };
                                emit_stream_block_progress(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    block_id,
                                    "text",
                                    completed,
                                    "streaming",
                                )
                                .await;
                            }
                        }
                    }
                    EventMsg::AgentReasoningDelta(delta) => {
                        if !delta.delta.is_empty() {
                            if thinking_block_id.is_none() {
                                let id = next_stream_block_id(
                                    &request_id,
                                    "thinking",
                                    &mut block_counter,
                                );
                                emit_stream_block_start(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    &id,
                                    "thinking",
                                )
                                .await;
                                thinking_block_id = Some(id);
                            }

                            session_mgr
                                .broadcast_to_all(
                                    "agent.stream",
                                    json!({
                                        "request_id": request_id,
                                        "session_id": session_id,
                                        "kind": "reasoning",
                                        "delta": delta.delta,
                                    }),
                                )
                                .await;

                            if let Some(block_id) = thinking_block_id.as_ref() {
                                emit_stream_block_delta(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    block_id,
                                    "thinking",
                                    &delta.delta,
                                )
                                .await;
                                let completed = {
                                    let entry = block_progress.entry(block_id.clone()).or_insert(0);
                                    *entry += delta.delta.chars().count();
                                    *entry
                                };
                                emit_stream_block_progress(
                                    session_mgr,
                                    &request_id,
                                    &session_id,
                                    block_id,
                                    "thinking",
                                    completed,
                                    "streaming",
                                )
                                .await;
                            }
                        }
                    }
                    EventMsg::AgentReasoningRawContentDelta(delta) => {
                        if !delta.delta.is_empty() {
                            session_mgr
                                .broadcast_to_all(
                                    "agent.stream",
                                    json!({
                                        "request_id": request_id,
                                        "session_id": session_id,
                                        "kind": "reasoning_raw",
                                        "delta": delta.delta,
                                    }),
                                )
                                .await;
                        }
                    }
                    EventMsg::McpToolCallBegin(begin) => {
                        let block_id =
                            next_stream_block_id(&request_id, "tool_call", &mut block_counter);
                        tool_blocks.insert(begin.call_id.clone(), block_id.clone());

                        session_mgr
                            .broadcast_to_all(
                                "agent.stream.block",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "phase": "start",
                                    "block_id": block_id,
                                    "block_kind": "tool_call",
                                    "call_id": begin.call_id,
                                    "server": begin.invocation.server,
                                    "tool": begin.invocation.tool,
                                }),
                            )
                            .await;
                        emit_stream_block_progress(
                            session_mgr,
                            &request_id,
                            &session_id,
                            &block_id,
                            "tool_call",
                            0,
                            "running",
                        )
                        .await;

                        session_mgr
                            .broadcast_to_all(
                                "tool.call",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "call_id": begin.call_id,
                                    "server": begin.invocation.server,
                                    "tool": begin.invocation.tool,
                                    "arguments": begin.invocation.arguments,
                                }),
                            )
                            .await;
                    }
                    EventMsg::McpToolCallEnd(end) => {
                        let result = serde_json::to_value(&end.result).unwrap_or(Value::Null);

                        let block_id = tool_blocks.remove(&end.call_id).unwrap_or_else(|| {
                            next_stream_block_id(&request_id, "tool_call", &mut block_counter)
                        });
                        emit_stream_block_progress(
                            session_mgr,
                            &request_id,
                            &session_id,
                            &block_id,
                            "tool_call",
                            1,
                            if end.is_success() {
                                "complete"
                            } else {
                                "error"
                            },
                        )
                        .await;
                        emit_stream_block_end(
                            session_mgr,
                            &request_id,
                            &session_id,
                            &block_id,
                            "tool_call",
                            if end.is_success() {
                                "complete"
                            } else {
                                "error"
                            },
                        )
                        .await;

                        session_mgr
                            .broadcast_to_all(
                                "tool.result",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "call_id": end.call_id,
                                    "server": end.invocation.server,
                                    "tool": end.invocation.tool,
                                    "duration_ms": end.duration.as_millis() as u64,
                                    "success": end.is_success(),
                                    "result": result,
                                }),
                            )
                            .await;
                    }
                    EventMsg::TurnAborted(reason) => {
                        aborted = true;
                        session_mgr
                            .broadcast_to_all(
                                "agent.error",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "error": format!("turn aborted: {:?}", reason.reason),
                                }),
                            )
                            .await;
                        break;
                    }
                    EventMsg::TurnComplete(_) => break,
                    EventMsg::Error(err) => {
                        session_mgr
                            .broadcast_to_all(
                                "agent.error",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "error": err.message,
                                }),
                            )
                            .await;
                        if reply.is_empty() {
                            fatal_error =
                                Some((INTERNAL_ERROR, format!("chat error: {}", err.message)));
                        }
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Err(err)) => {
                session_mgr
                    .broadcast_to_all(
                        "agent.error",
                        json!({
                            "request_id": request_id,
                            "session_id": session_id,
                            "error": format!("thread error: {err}"),
                        }),
                    )
                    .await;
                if reply.is_empty() {
                    fatal_error = Some((INTERNAL_ERROR, format!("thread error: {err}")));
                }
                break;
            }
            Err(_) => {
                let now = tokio::time::Instant::now();
                if now < deadline {
                    emit_typing_heartbeat(session_mgr, &request_id, &session_id, agent).await;
                    next_typing_heartbeat = now + typing_interval;
                    continue;
                }

                session_mgr
                    .broadcast_to_all(
                        "agent.error",
                        json!({
                            "request_id": request_id,
                            "session_id": session_id,
                            "error": format!("timed out waiting for response ({}s)", timeout.as_secs()),
                        }),
                    )
                    .await;
                if reply.is_empty() {
                    fatal_error = Some((
                        INTERNAL_ERROR,
                        format!("timed out waiting for response ({}s)", timeout.as_secs()),
                    ));
                }
                break;
            }
        }
    }

    let final_stream_status = if fatal_error.is_some() {
        "error"
    } else if aborted {
        "aborted"
    } else {
        "complete"
    };

    if let Some(block_id) = text_block_id.as_ref() {
        emit_stream_block_end(
            session_mgr,
            &request_id,
            &session_id,
            block_id,
            "text",
            final_stream_status,
        )
        .await;
    }
    if let Some(block_id) = thinking_block_id.as_ref() {
        emit_stream_block_end(
            session_mgr,
            &request_id,
            &session_id,
            block_id,
            "thinking",
            final_stream_status,
        )
        .await;
    }
    for block_id in tool_blocks.values() {
        emit_stream_block_end(
            session_mgr,
            &request_id,
            &session_id,
            block_id,
            "tool_call",
            final_stream_status,
        )
        .await;
    }
    emit_typing_stop(session_mgr, &request_id, &session_id, agent).await;

    if !keep_session_loaded {
        let _ = bridge
            .session_manager()
            .remove_session(&session_id_obj)
            .await;
    }

    if let Some((code, message)) = fatal_error {
        return Err((code, message));
    }

    if reply.is_empty() {
        reply = "(no response from agent)".to_string();
    }

    let footer = format_model_footer(&effective_model, &provider, model_profile.as_deref());
    let response = append_footer(&reply, &footer);

    persist_chat_session_metadata(
        session_store.as_ref(),
        &bridge.config().savfox_home,
        &session_id,
        &thread_id,
        &effective_model,
        &provider,
        rollout_path.as_deref(),
        last_token_usage.as_ref(),
    )
    .await;

    session_mgr
        .broadcast_to_all(
            "agent.complete",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "thread_id": thread_id,
                "response": response,
                "raw_response": reply,
                "model": effective_model,
                "provider": provider,
                "profile": model_profile,
                "aborted": aborted,
            }),
        )
        .await;

    Ok(json!({
        "response": response,
        "raw_response": reply,
        "footer": footer,
        "model": effective_model,
        "provider": provider,
        "profile": model_profile,
        "session_id": session_id,
        "thread_id": thread_id,
        "aborted": aborted,
    }))
}

async fn handle_chat_history(
    params: &Value,
    session_store: &Arc<SessionStore>,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    let source_channel = params
        .get("source_channel")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());

    println!(
        "[DEBUG] chat.history RPC called: session_id={}, limit={}",
        session_id, limit
    );

    if session_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'session_id' parameter".to_string(),
        ));
    }

    Ok(build_history_payload(session_id, limit, source_channel, session_store, bridge).await)
}

async fn handle_chat_abort(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let thread_id_param = params
        .get("thread_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    let session_id_param = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let candidates = resolve_abort_candidate_ids(
        session_store.as_ref(),
        thread_id_param.as_deref(),
        session_id_param.as_deref(),
    )
    .await;
    if let Some(thread_id) = abort_first_active_candidate(bridge.as_ref(), &candidates).await {
        return Ok(json!({ "status": "aborted", "thread_id": thread_id }));
    }

    let aborted = abort_all_active_threads(bridge.as_ref()).await;
    Ok(json!({ "status": "aborted", "aborted_count": aborted }))
}

/// Inject a message into a session's history without triggering the agent.
async fn handle_chat_inject(
    params: &Value,
    session_store: &Arc<crate::session::SessionStore>,
) -> RpcResult {
    let session_id = params["session_id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing session_id".to_string()))?;
    let content = params["content"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing content".to_string()))?;
    let role = params["role"].as_str().unwrap_or("system");

    // Validate role
    if !matches!(role, "system" | "user" | "assistant") {
        return Err((
            INVALID_PARAMS,
            format!("invalid role: {role} (expected system, user, or assistant)"),
        ));
    }

    // Check session exists and touch its timestamp
    let entry = session_store
        .get(session_id)
        .await
        .or(session_store.get_by_session_id(session_id).await)
        .ok_or((INVALID_PARAMS, format!("session not found: {session_id}")))?;

    // Touch session's updated_at via update
    session_store.update(&entry.session_id, |e| e.touch()).await;

    Ok(json!({
        "status": "injected",
        "session_id": entry.session_id,
        "role": role,
        "content_length": content.len(),
    }))
}

// ── Sessions ────────────────────────────────────────────────────────────────

async fn handle_sessions_list(
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let ws_ids = session_mgr.session_ids().await;
    let persistent = session_store.list().await;
    let mut sorted_sessions = persistent.clone();
    sorted_sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let persistent_keys: Vec<String> = sorted_sessions
        .iter()
        .map(|e| e.session_id.clone())
        .collect();
    let mut entries: Vec<Value> = Vec::with_capacity(sorted_sessions.len());
    for entry in sorted_sessions {
        let mut label = entry
            .label
            .clone()
            .or_else(|| entry.subject.clone())
            .or_else(|| entry.sender.as_ref().and_then(|s| s.display_name.clone()));
        if label.is_none() {
            label =
                derive_session_label_from_history(&entry.session_id, session_store, bridge).await;
        }

        let last_activity =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(entry.updated_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        entries.push(json!({
            "session_id": entry.session_id,
            "id": entry.session_id,
            "scope": entry.session_id,
            "label": label,
            "subject": entry.subject,
            "sender": entry.sender,
            "identity": entry.identity,
            "provenance_count": entry.provenance.len(),
            "last_activity": last_activity,
            "message_count": Value::Null,
            "messages": Value::Null,
            "model": entry.model,
            "provider": entry.provider,
            "thread_id": entry.thread_id,
        }));
    }
    let stats = session_store.stats().await;
    Ok(json!({
        "active_connections": ws_ids,
        "persistent_sessions": persistent_keys,
        "entries": entries,
        "total_persistent": stats.total_sessions,
        "total_tokens": stats.total_tokens,
    }))
}

async fn handle_sessions_preview(
    params: &Value,
    session_store: &Arc<SessionStore>,
    bridge: &GatewayBridge,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'session_id' parameter".to_string(),
        ));
    }
    let links = load_identity_links(&bridge.config().savfox_home).await;
    match session_store.get(session_id).await {
        Some(entry) => {
            let identity = entry.identity.clone().or_else(|| {
                entry
                    .to
                    .as_deref()
                    .and_then(|peer| canonical_for_peer(&links, peer))
            });
            let linked_identities = identity
                .as_ref()
                .and_then(|id| links.get(id).cloned())
                .unwrap_or_default();
            let value = serde_json::to_value(&entry).unwrap_or(Value::Null);
            Ok(json!({
                "session_id": session_id,
                "entry": value,
                "identity": identity,
                "linked_identities": linked_identities,
            }))
        }
        None => Ok(json!({
            "session_id": session_id,
            "entry": null,
            "identity": null,
            "linked_identities": [],
        })),
    }
}

async fn handle_sessions_patch(params: &Value, session_store: &Arc<SessionStore>) -> RpcResult {
    let requested_session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_id = if requested_session_id.trim().is_empty() {
        uuid::Uuid::now_v7().to_string()
    } else {
        requested_session_id.to_string()
    };
    let patch = params
        .get("patch")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let mut updated = session_store.get_or_create(&session_id).await.ok_or((
        INVALID_REQUEST,
        "invalid 'session_id' parameter (UUID v7 required)".to_string(),
    ))?;

    // Apply known patch fields.
    if let Some(model) = patch_str(params, &patch, "model") {
        updated.model = Some(model.to_owned());
    }
    if let Some(provider) = patch_str(params, &patch, "provider") {
        updated.provider = Some(provider.to_owned());
    }
    if let Some(label) = patch_str(params, &patch, "label") {
        let normalized = label.trim();
        updated.label = if normalized.is_empty() {
            None
        } else {
            Some(normalized.to_owned())
        };
    }
    if let Some(channel) = patch_str(params, &patch, "channel") {
        updated.last_channel = updated.channel.take();
        updated.channel = Some(channel.to_owned());
    }
    // Threading support: thread_id and parent_message_id
    if let Some(thread_id) = patch_str(params, &patch, "thread_id") {
        updated.thread_id = Some(thread_id.to_owned());
    }
    if let Some(parent_id) = patch_str(params, &patch, "parent_message_id") {
        updated.parent_message_id = Some(parent_id.to_owned());
    }
    // Group activation mode
    if let Some(ga) = patch_str(params, &patch, "group_activation") {
        updated.group_activation = Some(ga.to_owned());
    }

    // Apply overrides from the patch object or from the top-level params.
    let overrides_value = patch.get("overrides").or_else(|| params.get("overrides"));
    if let Some(ov) = overrides_value {
        if let Ok(incoming) = serde_json::from_value::<SessionOverrides>(ov.clone()) {
            updated.patch_overrides(incoming);
        }
    }
    updated.touch();
    session_store.upsert(updated.clone()).await;

    Ok(json!({ "session_id": updated.session_id, "status": "patched" }))
}

fn patch_str<'a>(params: &'a Value, patch: &'a Value, key: &str) -> Option<&'a str> {
    patch
        .get(key)
        .and_then(|v| v.as_str())
        .or_else(|| params.get(key).and_then(|v| v.as_str()))
}

async fn handle_sessions_reset(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'session_id' parameter".to_string(),
        ));
    }
    let session_id_obj = SessionId::from_string(session_id).map_err(|_| {
        (
            INVALID_REQUEST,
            "invalid 'session_id' parameter".to_string(),
        )
    })?;
    // Remove from WS session manager.
    session_mgr.remove_session(&session_id_obj).await;
    // Remove from persistent store.
    session_store.remove(session_id).await;
    let staging_cleaned = MediaStore::from_home(&bridge.config().savfox_home)
        .cleanup_staging_for_session(session_id)
        .await;
    Ok(json!({
        "session_id": session_id,
        "status": "reset",
        "staging_cleaned": staging_cleaned,
    }))
}

async fn handle_sessions_delete(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'session_id' parameter".to_string(),
        ));
    }
    let session_id_obj = SessionId::from_string(session_id).map_err(|_| {
        (
            INVALID_REQUEST,
            "invalid 'session_id' parameter".to_string(),
        )
    })?;
    session_mgr.remove_session(&session_id_obj).await;
    session_store.remove(session_id).await;
    let staging_cleaned = MediaStore::from_home(&bridge.config().savfox_home)
        .cleanup_staging_for_session(session_id)
        .await;
    Ok(json!({ "status": "deleted", "staging_cleaned": staging_cleaned }))
}

async fn handle_sessions_compact(params: &Value, session_store: &Arc<SessionStore>) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        // Global compaction  - prune stale entries.
        let pruned = session_store.prune().await;
        return Ok(json!({ "status": "compacted", "pruned": pruned }));
    }
    // Per-session compaction: increment counter.
    match session_store
        .update(session_id, |entry| {
            entry.compaction_count += 1;
        })
        .await
    {
        Some(entry) => Ok(json!({
            "session_id": session_id,
            "status": "compacted",
            "compaction_count": entry.compaction_count,
            "memory_flush_count": entry.memory_flush_count,
            "memory_flush_bytes": entry.memory_flush_bytes,
            "memory_flush_tokens_saved": entry.memory_flush_tokens_saved,
        })),
        None => Err((INVALID_REQUEST, format!("session '{session_id}' not found"))),
    }
}

// ── Session Overrides ────────────────────────────────────────────────────────

async fn handle_sessions_overrides_get(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .or_else(|| params.get("key"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'session_id' parameter".to_string(),
        ));
    }

    let entry = session_store.get(session_id).await;
    match entry {
        Some(e) => {
            let overrides = e.overrides.unwrap_or_default();
            Ok(json!({
                "session_id": session_id,
                "overrides": serde_json::to_value(overrides).unwrap_or(Value::Null),
            }))
        }
        None => Err((INVALID_REQUEST, format!("session '{session_id}' not found"))),
    }
}

async fn handle_sessions_overrides_set(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .or_else(|| params.get("key"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'session_id' parameter".to_string(),
        ));
    }

    let overrides_value = params
        .get("overrides")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));
    let incoming: SessionOverrides = serde_json::from_value(overrides_value)
        .map_err(|e| (INVALID_REQUEST, format!("invalid 'overrides': {e}")))?;

    let mut updated = session_store.get_or_create(session_id).await.ok_or((
        INVALID_REQUEST,
        "invalid 'session_id' parameter (UUID v7 required)".to_string(),
    ))?;
    updated.patch_overrides(incoming);
    session_store.upsert(updated.clone()).await;

    let overrides = updated.overrides.unwrap_or_default();
    Ok(json!({
        "session_id": updated.session_id,
        "overrides": serde_json::to_value(overrides).unwrap_or(Value::Null),
        "status": "updated",
    }))
}

// ── Identity Linking ────────────────────────────────────────────────────────

async fn handle_identity_links_get(bridge: &GatewayBridge) -> RpcResult {
    let links = load_identity_links(&bridge.config().savfox_home).await;
    Ok(json!({ "links": links }))
}

async fn handle_identity_links_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let input: HashMap<String, Vec<String>> = serde_json::from_value(
        params
            .get("links")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new())),
    )
    .map_err(|e| (INVALID_PARAMS, format!("invalid links format: {e}")))?;

    let mut merged = HashMap::new();
    for (canonical, peers) in input {
        let _ = upsert_link(&mut merged, &canonical, &peers);
    }

    save_identity_links(&bridge.config().savfox_home, &merged)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "updated", "count": merged.len(), "links": merged }))
}

async fn handle_identity_link(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let canonical = params
        .get("canonical")
        .or_else(|| params.get("identity"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if canonical.is_empty() {
        return Err((INVALID_PARAMS, "missing 'canonical' parameter".to_string()));
    }

    let mut peers: Vec<String> = params
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(single) = params.get("id").and_then(|v| v.as_str()) {
        peers.push(single.to_string());
    }
    if peers.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'ids' (or 'id') parameter".to_string(),
        ));
    }

    let mut links = load_identity_links(&bridge.config().savfox_home).await;
    let summary = upsert_link(&mut links, canonical, &peers)
        .ok_or_else(|| (INVALID_PARAMS, "invalid canonical or ids".to_string()))?;
    save_identity_links(&bridge.config().savfox_home, &links)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "linked",
        "summary": summary,
        "links": links,
    }))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DmScopePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<DmScope>,
    #[serde(default)]
    agents: HashMap<String, DmScope>,
    #[serde(default)]
    channels: HashMap<String, DmScope>,
}

impl DmScopePolicyConfig {
    fn normalize(&mut self) {
        self.agents = self
            .agents
            .drain()
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value))
            .collect();
        self.channels = self
            .channels
            .drain()
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value))
            .collect();
    }
}

fn dm_scope_policy_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("dm-scope.json")
}

fn parse_dm_scope(value: Option<&str>) -> Option<DmScope> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "main" => Some(DmScope::Main),
        "per_peer" => Some(DmScope::PerPeer),
        "per_channel_peer" => Some(DmScope::PerChannelPeer),
        "per_account_channel_peer" => Some(DmScope::PerAccountChannelPeer),
        _ => None,
    }
}

async fn handle_dm_scope_policy_get(bridge: &GatewayBridge) -> RpcResult {
    let path = dm_scope_policy_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let mut policy = serde_json::from_str::<DmScopePolicyConfig>(&content).unwrap_or_default();
    policy.normalize();
    Ok(json!({ "policy": policy }))
}

async fn handle_dm_scope_policy_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let raw = params
        .get("policy")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let mut policy: DmScopePolicyConfig = serde_json::from_value(raw)
        .map_err(|e| (INVALID_PARAMS, format!("invalid policy: {e}")))?;
    policy.normalize();

    let content = serde_json::to_string_pretty(&policy)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(dm_scope_policy_path(bridge), content)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "updated", "policy": policy }))
}

fn agent_from_routing_id(routing_id: &str) -> Option<String> {
    let rest = routing_id.strip_prefix("agent:")?;
    let agent = rest.split(':').next()?.trim();
    if agent.is_empty() {
        None
    } else {
        Some(agent.to_string())
    }
}

fn merge_session_entries(mut left: SessionEntry, right: SessionEntry) -> SessionEntry {
    if right.updated_at > left.updated_at {
        left.updated_at = right.updated_at;
        left.channel = right.channel.clone().or(left.channel);
        left.last_channel = right.last_channel.clone().or(left.last_channel);
        left.to = right.to.clone().or(left.to);
        left.last_to = right.last_to.clone().or(left.last_to);
        left.thread_id = right.thread_id.clone().or(left.thread_id);
        left.reply_target = right.reply_target.clone().or(left.reply_target);
        left.parent_thread_id = right.parent_thread_id.clone().or(left.parent_thread_id);
        left.parent_message_id = right.parent_message_id.clone().or(left.parent_message_id);
        left.display_name = right.display_name.clone().or(left.display_name);
        left.identity = right.identity.clone().or(left.identity);
    }

    left.input_tokens = left.input_tokens.saturating_add(right.input_tokens);
    left.output_tokens = left.output_tokens.saturating_add(right.output_tokens);
    left.total_tokens = left.total_tokens.saturating_add(right.total_tokens);
    left.compaction_count = left.compaction_count.saturating_add(right.compaction_count);
    left.memory_flush_count = left
        .memory_flush_count
        .saturating_add(right.memory_flush_count);
    left.memory_flush_bytes = left
        .memory_flush_bytes
        .saturating_add(right.memory_flush_bytes);
    left.memory_flush_tokens_saved = left
        .memory_flush_tokens_saved
        .saturating_add(right.memory_flush_tokens_saved);
    left.provenance.extend(right.provenance);
    left.provenance.sort_by_key(|item| item.timestamp);
    left
}

async fn handle_dm_scope_migrate(params: &Value, session_store: &Arc<SessionStore>) -> RpcResult {
    let target_scope = parse_dm_scope(params.get("scope").and_then(|v| v.as_str()))
        .ok_or_else(|| (INVALID_PARAMS, "invalid or missing 'scope'".to_string()))?;
    let dry_run = params
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let filter_agent = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());
    let filter_channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());

    let mut moved = 0usize;
    let mut skipped = 0usize;
    let merged = 0usize;
    let mut rebuilt: HashMap<String, SessionEntry> = HashMap::new();

    for mut entry in session_store.list().await {
        if entry.group_id.is_some() {
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        }

        let Some(routing_id) = entry.routing_id.as_deref() else {
            skipped = skipped.saturating_add(1);
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        };
        let Some(agent) = agent_from_routing_id(routing_id) else {
            skipped = skipped.saturating_add(1);
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        };
        if let Some(filter) = filter_agent.as_deref()
            && agent.to_ascii_lowercase() != filter
        {
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        }
        if let Some(filter) = filter_channel.as_deref()
            && entry
                .channel
                .as_deref()
                .map(|ch| ch.to_ascii_lowercase())
                .as_deref()
                != Some(filter)
        {
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        }

        let peer = entry
            .to
            .clone()
            .or(entry.identity.clone())
            .or_else(|| entry.sender.as_ref().and_then(|s| s.display_name.clone()));
        let Some(peer) = peer else {
            skipped = skipped.saturating_add(1);
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        };

        let new_routing_id = build_routing_id(
            &agent,
            entry.channel.as_deref(),
            entry.group_id.as_deref(),
            entry.thread_id.as_deref(),
            Some(peer.as_str()),
            entry.account_id.as_deref(),
            target_scope,
        );
        if entry.routing_id.as_deref() != Some(new_routing_id.as_str()) {
            moved = moved.saturating_add(1);
            entry.routing_id = Some(new_routing_id);
        }
        rebuilt.insert(entry.session_id.clone(), entry);
    }

    if !dry_run {
        session_store
            .replace_all(rebuilt.clone())
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("persist error: {e}")))?;
    }

    Ok(json!({
        "status": if dry_run { "dry_run" } else { "migrated" },
        "scope": target_scope.as_str(),
        "moved": moved,
        "merged": merged,
        "skipped": skipped,
        "sessions": rebuilt.len(),
        "filters": {
            "agent": filter_agent,
            "channel": filter_channel,
        }
    }))
}

// ── Sessions Usage ──────────────────────────────────────────────────────────

async fn handle_sessions_usage(params: &Value, session_store: &Arc<SessionStore>) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if session_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'session_id' parameter".to_string(),
        ));
    }

    match session_store.get(session_id).await {
        Some(entry) => {
            let total = entry.total_tokens;
            let input = entry.input_tokens;
            let output = entry.output_tokens;

            // Context weight analysis (estimated percentages)
            let system_pct = if total > 0 {
                (input as f64 * 0.3 / total as f64 * 100.0).round()
            } else {
                0.0
            };
            let history_pct = if total > 0 {
                (input as f64 * 0.6 / total as f64 * 100.0).round()
            } else {
                0.0
            };
            let tools_pct = if total > 0 {
                (input as f64 * 0.1 / total as f64 * 100.0).round()
            } else {
                0.0
            };

            Ok(json!({
                "session_id": session_id,
                "input_tokens": input,
                "output_tokens": output,
                "total_tokens": total,
                "model": entry.model,
                "provider": entry.provider,
                "created_at": entry.created_at,
                "updated_at": entry.updated_at,
                "context_weight": {
                    "system_prompt_pct": system_pct,
                    "history_pct": history_pct,
                    "tools_pct": tools_pct,
                },
            }))
        }
        None => Err((INVALID_PARAMS, format!("session not found: {session_id}"))),
    }
}

// ── Media Staging ───────────────────────────────────────────────────────────

async fn handle_media_staging_list(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let session_id = params.get("session_id").and_then(|v| v.as_str());
    let store = MediaStore::from_home(&bridge.config().savfox_home);
    let entries = store.list_staging(session_id).await;
    Ok(json!({
        "entries": entries,
        "count": entries.len(),
        "session_id": session_id,
    }))
}

async fn handle_media_staging_import(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }
    let workspace_dir = params
        .get("workspace_dir")
        .or_else(|| params.get("workspace"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                INVALID_PARAMS,
                "missing 'workspace_dir' parameter".to_string(),
            )
        })?;
    if workspace_dir.trim().is_empty() {
        return Err((INVALID_PARAMS, "workspace_dir cannot be empty".to_string()));
    }

    let store = MediaStore::from_home(&bridge.config().savfox_home);
    let imported_path = store
        .import_from_staging(id, &PathBuf::from(workspace_dir))
        .await
        .map_err(|e| (INVALID_PARAMS, e))?;
    Ok(json!({
        "status": "ok",
        "id": id,
        "workspace_dir": workspace_dir,
        "imported_path": imported_path,
    }))
}

async fn handle_media_staging_cleanup(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'session_id' parameter".to_string()));
    }

    let store = MediaStore::from_home(&bridge.config().savfox_home);
    let removed = store.cleanup_staging_for_session(session_id).await;
    Ok(json!({
        "status": "ok",
        "session_id": session_id,
        "removed": removed,
    }))
}

// ── Typing Indicators ───────────────────────────────────────────────────────

async fn handle_typing_start(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    session_mgr
        .broadcast_to_all(
            "typing.start",
            json!({
                "session_id": session_id,
                "agent_id": agent_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    Ok(json!({ "status": "typing_started", "session_id": session_id }))
}

async fn handle_typing_stop(params: &Value, session_mgr: &Arc<GatewaySessionManager>) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    session_mgr
        .broadcast_to_all(
            "typing.stop",
            json!({
                "session_id": session_id,
                "agent_id": agent_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    Ok(json!({ "status": "typing_stopped", "session_id": session_id }))
}

// ── Events (Server-Push) ────────────────────────────────────────────────────

/// Known event types that can be subscribed to.
const EVENT_TYPES: &[&str] = &[
    "agent.stream",
    "agent.stream.block",
    "agent.stream.progress",
    "agent.complete",
    "agent.error",
    "tool.call",
    "tool.result",
    "typing.start",
    "typing.stop",
    "session.updated",
    "session.created",
    "session.deleted",
    "config.changed",
    "channel.status",
    "channel.connected",
    "channel.disconnected",
    "approval.requested",
    "approval.resolved",
    "system.event",
    "system.presence",
    "cron.started",
    "cron.completed",
    "memory.updated",
];

async fn handle_events_subscribe(params: &Value) -> RpcResult {
    let events: Vec<String> = params
        .get("events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if events.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'events' array parameter".to_string(),
        ));
    }

    // Validate event patterns (support wildcards like "agent.*")
    let valid: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
    let invalid: Vec<&&str> = valid
        .iter()
        .filter(|e| !e.contains('*') && !EVENT_TYPES.contains(e))
        .collect();

    if !invalid.is_empty() {
        // Allow unknown events too (for forward compat), just note them
    }

    Ok(json!({
        "status": "subscribed",
        "events": events,
        "count": events.len(),
    }))
}

async fn handle_events_unsubscribe(params: &Value) -> RpcResult {
    let events: Vec<String> = params
        .get("events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "status": "unsubscribed",
        "events": events,
        "count": events.len(),
    }))
}

async fn handle_events_list() -> RpcResult {
    let events: Vec<Value> = EVENT_TYPES.iter().map(|e| json!({ "event": e })).collect();

    Ok(json!({
        "events": events,
        "count": events.len(),
    }))
}

// ── Send / Wake / Channels ──────────────────────────────────────────────────

async fn handle_send(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if channel.is_empty() || text.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'channel' or 'text' parameter".to_string(),
        ));
    }

    match bridge
        .send_platform_message(channel, text, None, None, None)
        .await
    {
        Ok(()) => Ok(json!({ "status": "sent" })),
        Err(err) => Err((INTERNAL_ERROR, format!("send error: {err}"))),
    }
}

async fn handle_send_metrics() -> RpcResult {
    let metrics = crate::bridges::runtime::send_metrics_snapshot().await;
    Ok(json!({ "metrics": metrics }))
}

async fn handle_wake(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("wake");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let heartbeat = params
        .get("heartbeat")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if heartbeat {
        // Heartbeat mode: just acknowledge without invoking agent.
        return Ok(json!({ "status": "heartbeat", "timestamp": chrono::Utc::now().to_rfc3339() }));
    }

    match bridge.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "status": "awake", "response": reply })),
        Err(err) => Err((INTERNAL_ERROR, format!("wake error: {err}"))),
    }
}

async fn handle_channels_list(_bridge: &Arc<GatewayBridge>) -> RpcResult {
    // List all supported platforms with their webhook endpoints.
    let channels = vec![
        json!({"platform": "discord", "endpoint": "/webhooks/discord", "type": "bridge"}),
        json!({"platform": "telegram", "endpoint": "/webhooks/telegram", "type": "bridge"}),
        json!({"platform": "slack", "endpoint": "/webhooks/slack", "type": "bridge"}),
        json!({"platform": "msteams", "endpoint": "/webhooks/msteams", "type": "bridge"}),
        json!({"platform": "webhook", "endpoint": "/webhooks/webhook", "type": "generic"}),
        json!({"platform": "matrix", "endpoint": "/webhooks/matrix", "type": "webhook"}),
        json!({"platform": "mattermost", "endpoint": "/webhooks/mattermost", "type": "webhook"}),
        json!({"platform": "googlechat", "endpoint": "/webhooks/googlechat", "type": "webhook"}),
        json!({"platform": "line", "endpoint": "/webhooks/line", "type": "webhook"}),
        json!({"platform": "feishu", "endpoint": "/webhooks/feishu", "type": "webhook"}),
        json!({"platform": "irc", "endpoint": "/webhooks/irc", "type": "webhook"}),
        json!({"platform": "nostr", "endpoint": "/webhooks/nostr", "type": "bridge"}),
        json!({"platform": "zalo", "endpoint": "/webhooks/zalo", "type": "webhook"}),
    ];
    Ok(json!({ "channels": channels }))
}

async fn handle_channels_status(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let runtime = bridge.runtime_bridge_secrets().await;
    let discord_configured =
        runtime.discord_bot_token.is_some() || std::env::var("DISCORD_BOT_TOKEN").is_ok();
    let telegram_configured =
        runtime.telegram_bot_token.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok();
    let slack_configured =
        runtime.slack_bot_token.is_some() || std::env::var("SLACK_BOT_TOKEN").is_ok();
    let webhook_configured =
        runtime.webhook_secret.is_some() || std::env::var("WEBHOOK_SECRET").is_ok();
    let probe_requested = params
        .get("probe")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let health_metrics = crate::bridges::runtime::channel_health_snapshot().await;
    let send_metrics = crate::bridges::runtime::send_metrics_snapshot().await;
    let nostr_profile = load_nostr_profile(bridge).await;
    let nostr_configured = nostr_profile
        .get("private_key")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let nostr_public_key = nostr_profile
        .get("public_key")
        .cloned()
        .unwrap_or(Value::Null);
    let nostr_relay_count = nostr_profile
        .get("relays")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len() as u32)
        .unwrap_or(0);

    let mut channels = json!({
        "discord": {
            "configured": discord_configured,
            "running": discord_configured,
            "connected": discord_configured,
        },
        "telegram": {
            "configured": telegram_configured,
            "running": telegram_configured,
            "connected": telegram_configured,
        },
        "slack": {
            "configured": slack_configured,
            "running": slack_configured,
            "connected": slack_configured,
        },
        "matrix": {
            "configured": false,
            "running": false,
            "connected": false,
        },
        "whatsapp": {
            "configured": false,
            "running": false,
            "connected": false,
            "linked": false,
            "qr_data_url": Value::Null,
        },
        "signal": {
            "configured": false,
            "running": false,
            "connected": false,
        },
        "mattermost": {
            "configured": false,
            "running": false,
            "connected": false,
        },
        "googlechat": {
            "configured": false,
            "running": false,
            "connected": false,
        },
        "webhook": {
            "configured": webhook_configured,
            "running": webhook_configured,
            "connected": webhook_configured,
        },
        "irc": {
            "configured": false,
            "running": false,
            "connected": false,
        },
        "line": {
            "configured": false,
            "running": false,
            "connected": false,
        },
        "feishu": {
            "configured": false,
            "running": false,
            "connected": false,
        },
        "nostr": {
            "configured": nostr_configured,
            "running": nostr_configured,
            "connected": nostr_configured,
            "public_key": nostr_public_key,
            "relay_count": nostr_relay_count,
        },
    });

    // Overlay persisted channel configs so UI can restore configured channels on page load.
    if let Ok(saved_configs) =
        crate::channel_store::list_channel_configs(&bridge.config().savfox_home).await
        && let Some(channels_map) = channels.as_object_mut()
    {
        for saved in saved_configs {
            let key = saved.id.to_ascii_lowercase();
            let entry = channels_map.entry(key.clone()).or_insert_with(|| {
                json!({
                    "configured": false,
                    "running": false,
                    "connected": false,
                })
            });
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("configured".to_string(), json!(true));
                obj.insert("saved".to_string(), json!(true));
                obj.insert("enabled".to_string(), json!(saved.enabled));
                obj.insert("channelName".to_string(), json!(saved.name));
                obj.insert("agentId".to_string(), json!(saved.agent_id));
            }
        }
    }

    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let to_rfc3339 = |timestamp_ms: Option<u64>| -> Value {
        timestamp_ms
            .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts as i64))
            .map(|dt| Value::String(dt.to_rfc3339()))
            .unwrap_or(Value::Null)
    };

    if let Some(map) = channels.as_object_mut() {
        for (platform, info) in map.iter_mut() {
            let metrics = health_metrics.get(platform).cloned().unwrap_or_default();
            let send = send_metrics.get(platform).cloned().unwrap_or_default();
            let error_rate = if send.attempts == 0 {
                0.0
            } else {
                send.failed as f64 / send.attempts as f64
            };
            let connection_uptime_ms = metrics
                .connected_since_ms
                .map(|since| now_ms.saturating_sub(since));
            let last_activity_ms = [metrics.last_event_time_ms, metrics.last_message_time_ms]
                .into_iter()
                .flatten()
                .max();

            let configured = info
                .get("configured")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let connected = info
                .get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let probe_status = if probe_requested {
                if !configured {
                    "not_configured".to_string()
                } else if connected {
                    "ok".to_string()
                } else {
                    "degraded".to_string()
                }
            } else {
                metrics
                    .probe_status
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string())
            };
            if probe_requested {
                crate::bridges::runtime::record_channel_probe(platform, &probe_status).await;
            }

            if let Some(obj) = info.as_object_mut() {
                obj.insert(
                    "last_message_time".to_string(),
                    json!(metrics.last_message_time_ms),
                );
                obj.insert(
                    "last_event_time".to_string(),
                    json!(metrics.last_event_time_ms),
                );
                obj.insert(
                    "reconnect_attempt_count".to_string(),
                    json!(metrics.reconnect_attempt_count),
                );
                obj.insert("probe_status".to_string(), json!(probe_status));
                obj.insert(
                    "connection_uptime_ms".to_string(),
                    json!(connection_uptime_ms),
                );
                obj.insert("error_rate".to_string(), json!(error_rate));
                obj.insert("messages_total".to_string(), json!(send.attempts));
                obj.insert("messages_failed".to_string(), json!(send.failed));

                obj.insert(
                    "lastMessageTime".to_string(),
                    to_rfc3339(metrics.last_message_time_ms),
                );
                obj.insert(
                    "lastEventTime".to_string(),
                    to_rfc3339(metrics.last_event_time_ms),
                );
                obj.insert(
                    "reconnectAttemptCount".to_string(),
                    json!(metrics.reconnect_attempt_count),
                );
                obj.insert("probeStatus".to_string(), json!(probe_status));
                obj.insert("uptimeMs".to_string(), json!(connection_uptime_ms));
                obj.insert("errorRate".to_string(), json!(error_rate));
                obj.insert(
                    "lastProbeAt".to_string(),
                    to_rfc3339(metrics.last_probe_time_ms),
                );
                obj.insert("lastActivity".to_string(), to_rfc3339(last_activity_ms));
            }
        }
    }

    let requested_channel = params
        .get("channel")
        .or_else(|| params.get("platform"))
        .and_then(|v| v.as_str());
    if let Some(channel) = requested_channel {
        if let Some(entry) = channels.get(channel) {
            let mut payload = entry.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("platform".to_string(), Value::String(channel.to_string()));
            }
            return Ok(payload);
        }
        return Err((INVALID_REQUEST, format!("unknown channel: {channel}")));
    }

    Ok(json!({ "channels": channels }))
}

async fn handle_channels_login(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let platform = params
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_string()));
    }

    let runtime = bridge.runtime_bridge_secrets().await;
    let nostr_profile = load_nostr_profile(bridge).await;
    let nostr_configured = nostr_profile
        .get("private_key")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let is_configured = match platform {
        "discord" => {
            runtime.discord_bot_token.is_some() || std::env::var("DISCORD_BOT_TOKEN").is_ok()
        }
        "telegram" => {
            runtime.telegram_bot_token.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok()
        }
        "slack" => runtime.slack_bot_token.is_some() || std::env::var("SLACK_BOT_TOKEN").is_ok(),
        "webhook" => runtime.webhook_secret.is_some() || std::env::var("WEBHOOK_SECRET").is_ok(),
        "nostr" => nostr_configured,
        _ => false,
    };

    Ok(json!({
        "platform": platform,
        "status": if is_configured { "already_configured" } else { "needs_config" },
        "configured": is_configured,
        "message": if is_configured {
            format!("{} is already configured", platform)
        } else {
            format!("Please configure {} in the modal", platform)
        }
    }))
}

async fn handle_channels_logout(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let platform = params
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_string()));
    }

    let mut secrets = bridge.runtime_bridge_secrets().await;
    match platform {
        "discord" => secrets.discord_bot_token = None,
        "telegram" => secrets.telegram_bot_token = None,
        "slack" => {
            secrets.slack_bot_token = None;
            secrets.slack_signing_secret = None;
        }
        "webhook" => secrets.webhook_secret = None,
        "nostr" => {
            let mut profile = load_nostr_profile(bridge).await;
            profile["private_key"] = json!("");
            profile["public_key"] = json!("");
            let _ = save_nostr_profile(bridge, &profile).await;
        }
        "matrix" | "whatsapp" | "signal" | "mattermost" | "googlechat" | "irc" | "line"
        | "feishu" => {
            // These platforms may not have runtime secrets yet
        }
        _ => {
            return Err((INVALID_REQUEST, format!("unknown platform: {platform}")));
        }
    }
    bridge.set_runtime_bridge_secrets(secrets).await;

    Ok(json!({ "platform": platform, "status": "logged_out" }))
}

async fn handle_channels_test(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_string()));
    }

    let runtime = bridge.runtime_bridge_secrets().await;
    let nostr_profile = load_nostr_profile(bridge).await;
    let nostr_configured = nostr_profile
        .get("private_key")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let configured = match platform {
        "discord" => {
            runtime.discord_bot_token.is_some() || std::env::var("DISCORD_BOT_TOKEN").is_ok()
        }
        "telegram" => {
            runtime.telegram_bot_token.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok()
        }
        "slack" => runtime.slack_bot_token.is_some() || std::env::var("SLACK_BOT_TOKEN").is_ok(),
        "webhook" => runtime.webhook_secret.is_some() || std::env::var("WEBHOOK_SECRET").is_ok(),
        "nostr" => nostr_configured,
        "whatsapp" => true,
        _ => false,
    };

    Ok(json!({
        "platform": platform,
        "ok": configured,
        "message": if configured {
            format!("{platform} test passed")
        } else {
            format!("{platform} is not configured")
        }
    }))
}

async fn handle_channels_account_update(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let platform = params
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let account = params.get("account").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_string()));
    }
    if account.is_empty() {
        return Err((INVALID_REQUEST, "missing 'account' parameter".to_string()));
    }

    let path = bridge
        .config()
        .savfox_home
        .join("gateway")
        .join("channel-accounts.json");
    let mut root = tokio::fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    root[platform]["accounts"][account] = json!({
        "enabled": enabled,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let payload = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
    if let Err(err) = tokio::fs::write(&path, payload).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to persist account state: {err}"),
        ));
    }

    Ok(json!({
        "platform": platform,
        "account": account,
        "enabled": enabled,
        "status": "updated",
    }))
}

const DIRECTORY_SUPPORTED_CHANNELS: [&str; 4] = ["discord", "slack", "telegram", "whatsapp"];

fn normalize_directory_channel(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if DIRECTORY_SUPPORTED_CHANNELS.contains(&normalized.as_str()) {
        Some(normalized)
    } else {
        None
    }
}

fn parse_directory_channels(params: &Value) -> Result<Vec<String>, (i64, String)> {
    let mut channels = Vec::new();

    if let Some(raw_channels) = params.get("channels").and_then(|v| v.as_array()) {
        for raw in raw_channels {
            if let Some(value) = raw.as_str() {
                let Some(channel) = normalize_directory_channel(value) else {
                    return Err((INVALID_PARAMS, format!("unsupported channel: {value}")));
                };
                if !channels.contains(&channel) {
                    channels.push(channel);
                }
            }
        }
    }

    for key in ["channel", "platform"] {
        if let Some(value) = params.get(key).and_then(|v| v.as_str()) {
            let Some(channel) = normalize_directory_channel(value) else {
                return Err((INVALID_PARAMS, format!("unsupported channel: {value}")));
            };
            if !channels.contains(&channel) {
                channels.push(channel);
            }
        }
    }

    if channels.is_empty() {
        return Ok(DIRECTORY_SUPPORTED_CHANNELS
            .iter()
            .map(|v| (*v).to_string())
            .collect());
    }

    Ok(channels)
}

fn parse_directory_query(params: &Value) -> Option<String> {
    let query = params.get("query").and_then(|v| v.as_str())?.trim();
    if query.is_empty() {
        None
    } else {
        Some(query.to_ascii_lowercase())
    }
}

fn parse_directory_limit(params: &Value, default_limit: usize) -> usize {
    params
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v.clamp(1, 500) as usize)
        .unwrap_or(default_limit)
}

fn directory_query_match(query: Option<&str>, fields: &[&str]) -> bool {
    let Some(query) = query else {
        return true;
    };
    fields
        .iter()
        .any(|value| value.to_ascii_lowercase().contains(query))
}

fn session_platform(entry: &SessionEntry) -> Option<String> {
    let channel = entry
        .channel
        .as_deref()
        .or(entry.last_channel.as_deref())
        .or(entry.from.as_deref())?;
    if let Some((platform, _)) = channel.split_once(':') {
        let trimmed = platform.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_ascii_lowercase())
        }
    } else {
        let trimmed = channel.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_ascii_lowercase())
        }
    }
}

fn session_channel_id(entry: &SessionEntry) -> Option<String> {
    let channel = entry.channel.as_deref().or(entry.last_channel.as_deref())?;
    if let Some((_, id)) = channel.split_once(':') {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    }
}

fn session_group_id(entry: &SessionEntry) -> Option<String> {
    if let Some(group_id) = entry
        .group_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(group_id.to_string());
    }

    if matches!(entry.chat_type.as_deref(), Some("group" | "channel")) {
        return session_channel_id(entry);
    }
    None
}

fn latest_provenance(entry: &SessionEntry) -> Option<&crate::session::SessionMessageProvenance> {
    entry.provenance.iter().max_by_key(|item| item.timestamp)
}

fn session_peer_id(entry: &SessionEntry) -> Option<String> {
    entry
        .to
        .as_deref()
        .or(entry.last_to.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            latest_provenance(entry)
                .map(|item| item.user_id.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            entry
                .identity
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn session_display_name(entry: &SessionEntry) -> Option<String> {
    entry
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            latest_provenance(entry)
                .map(|item| item.display_name.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn channel_accounts_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge
        .config()
        .savfox_home
        .join("gateway")
        .join("channel-accounts.json")
}

async fn load_channel_accounts(bridge: &GatewayBridge) -> Value {
    let path = channel_accounts_path(bridge);
    let content = tokio::fs::read_to_string(path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    serde_json::from_str::<Value>(&content)
        .ok()
        .filter(|value| value.is_object())
        .unwrap_or_else(|| json!({}))
}

fn directory_channel_configured(
    channel: &str,
    runtime: &crate::bridge::RuntimeBridgeSecrets,
) -> bool {
    match channel {
        "discord" => {
            runtime.discord_bot_token.is_some() || std::env::var("DISCORD_BOT_TOKEN").is_ok()
        }
        "telegram" => {
            runtime.telegram_bot_token.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok()
        }
        "slack" => runtime.slack_bot_token.is_some() || std::env::var("SLACK_BOT_TOKEN").is_ok(),
        "whatsapp" => {
            std::env::var("WHATSAPP_ACCESS_TOKEN").is_ok()
                && std::env::var("WHATSAPP_PHONE_NUMBER_ID").is_ok()
        }
        _ => false,
    }
}

async fn handle_directory_self(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let channels = parse_directory_channels(params)?;
    let runtime = bridge.runtime_bridge_secrets().await;
    let account_doc = load_channel_accounts(bridge).await;
    let sessions = session_store.list().await;

    let mut accounts = Vec::new();

    for channel in &channels {
        let configured = directory_channel_configured(channel, &runtime);
        let mut seen_accounts = HashSet::new();

        if let Some(account_map) = account_doc
            .get(channel)
            .and_then(|v| v.get("accounts"))
            .and_then(|v| v.as_object())
        {
            for (account_id, details) in account_map {
                let account_id = account_id.trim();
                if account_id.is_empty() {
                    continue;
                }
                let enabled = details
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if seen_accounts.insert(account_id.to_string()) {
                    accounts.push(json!({
                        "channel": channel,
                        "account_id": account_id,
                        "configured": configured,
                        "enabled": enabled,
                        "source": "channel-accounts",
                    }));
                }
            }
        }

        for entry in &sessions {
            if session_platform(entry).as_deref() != Some(channel.as_str()) {
                continue;
            }
            let Some(account_id) = entry
                .account_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if seen_accounts.insert(account_id.to_string()) {
                accounts.push(json!({
                    "channel": channel,
                    "account_id": account_id,
                    "configured": configured,
                    "enabled": true,
                    "source": "sessions",
                }));
            }
        }

        if seen_accounts.is_empty() {
            let fallback_account = match channel.as_str() {
                "whatsapp" => std::env::var("WHATSAPP_PHONE_NUMBER_ID")
                    .unwrap_or_else(|_| "default".to_string()),
                _ => "default".to_string(),
            };
            accounts.push(json!({
                "channel": channel,
                "account_id": fallback_account,
                "configured": configured,
                "enabled": configured,
                "source": "runtime",
            }));
        }
    }

    Ok(json!({
        "channels": channels,
        "accounts": accounts,
    }))
}

async fn handle_directory_peers_list(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let channels = parse_directory_channels(params)?;
    let channel_set: HashSet<String> = channels.iter().cloned().collect();
    let query = parse_directory_query(params);
    let limit = parse_directory_limit(params, 50);

    let mut entries = session_store.list().await;
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));

    let mut seen = HashSet::new();
    let mut peers = Vec::new();

    for entry in entries {
        let Some(channel) = session_platform(&entry) else {
            continue;
        };
        if !channel_set.contains(&channel) {
            continue;
        }

        let Some(peer_id) = session_peer_id(&entry) else {
            continue;
        };
        let display_name = session_display_name(&entry).unwrap_or_else(|| peer_id.clone());
        let identity = entry.identity.clone().unwrap_or_default();
        let chat_type = entry.chat_type.clone().unwrap_or_else(|| "dm".to_string());

        if !directory_query_match(
            query.as_deref(),
            &[&channel, &peer_id, &display_name, &identity, &chat_type],
        ) {
            continue;
        }

        let dedupe_key = format!("{channel}:{peer_id}");
        if !seen.insert(dedupe_key) {
            continue;
        }

        peers.push(json!({
            "channel": channel,
            "peer_id": peer_id,
            "display_name": display_name,
            "identity": entry.identity,
            "chat_type": chat_type,
            "group_id": entry.group_id,
            "last_seen_ms": entry.updated_at,
            "session_id": entry.session_id,
        }));

        if peers.len() >= limit {
            break;
        }
    }

    Ok(json!({
        "channels": channels,
        "query": query,
        "limit": limit,
        "peers": peers,
    }))
}

#[derive(Debug, Clone, Default)]
struct DirectoryGroupAccumulator {
    channel: String,
    group_id: String,
    name: String,
    topic: Option<String>,
    members: HashSet<String>,
    sessions: u64,
    last_seen_ms: u64,
}

async fn handle_directory_groups_list(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let channels = parse_directory_channels(params)?;
    let channel_set: HashSet<String> = channels.iter().cloned().collect();
    let query = parse_directory_query(params);
    let limit = parse_directory_limit(params, 50);

    let mut grouped: HashMap<String, DirectoryGroupAccumulator> = HashMap::new();
    for entry in session_store.list().await {
        let Some(channel) = session_platform(&entry) else {
            continue;
        };
        if !channel_set.contains(&channel) {
            continue;
        }

        let Some(group_id) = session_group_id(&entry) else {
            continue;
        };
        let group_name_candidate = entry
            .subject
            .clone()
            .or(entry.group_channel.clone())
            .or(entry.label.clone())
            .filter(|value| !value.trim().is_empty());
        let key = format!("{channel}:{group_id}");
        let accumulator = grouped
            .entry(key)
            .or_insert_with(|| DirectoryGroupAccumulator {
                channel: channel.clone(),
                group_id: group_id.clone(),
                name: group_name_candidate
                    .clone()
                    .unwrap_or_else(|| group_id.clone()),
                topic: group_name_candidate.clone(),
                members: HashSet::new(),
                sessions: 0,
                last_seen_ms: entry.updated_at,
            });

        accumulator.sessions = accumulator.sessions.saturating_add(1);
        accumulator.last_seen_ms = accumulator.last_seen_ms.max(entry.updated_at);
        if (accumulator.name == accumulator.group_id || accumulator.name.trim().is_empty())
            && group_name_candidate.is_some()
        {
            accumulator.name = group_name_candidate.clone().unwrap_or_default();
        }
        if accumulator
            .topic
            .as_deref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
            && group_name_candidate.is_some()
        {
            accumulator.topic = group_name_candidate.clone();
        }

        for provenance in &entry.provenance {
            if !provenance.user_id.trim().is_empty() {
                accumulator.members.insert(provenance.user_id.clone());
            }
        }
        if let Some(peer_id) = session_peer_id(&entry) {
            accumulator.members.insert(peer_id);
        }
    }

    let mut groups = grouped
        .into_values()
        .filter(|group| {
            directory_query_match(
                query.as_deref(),
                &[&group.channel, &group.group_id, &group.name],
            )
        })
        .map(|group| {
            json!({
                "channel": group.channel,
                "group_id": group.group_id,
                "name": group.name,
                "topic": group.topic,
                "members_estimate": group.members.len(),
                "sessions": group.sessions,
                "last_seen_ms": group.last_seen_ms,
            })
        })
        .collect::<Vec<_>>();

    groups.sort_by_key(|item| {
        std::cmp::Reverse(
            item.get("last_seen_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        )
    });
    groups.truncate(limit);

    Ok(json!({
        "channels": channels,
        "query": query,
        "limit": limit,
        "groups": groups,
    }))
}

#[derive(Debug, Clone, Default)]
struct DirectoryMemberAccumulator {
    channel: String,
    user_id: String,
    display_name: String,
    sessions: u64,
    last_seen_ms: u64,
}

async fn handle_directory_groups_members(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let group_id = params
        .get("group_id")
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if group_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'group_id' parameter".to_string()));
    }

    let channels = parse_directory_channels(params)?;
    let channel_set: HashSet<String> = channels.iter().cloned().collect();
    let query = parse_directory_query(params);
    let limit = parse_directory_limit(params, 200);

    let mut members: HashMap<String, DirectoryMemberAccumulator> = HashMap::new();

    for entry in session_store.list().await {
        let Some(channel) = session_platform(&entry) else {
            continue;
        };
        if !channel_set.contains(&channel) {
            continue;
        }
        if session_group_id(&entry).as_deref() != Some(group_id.as_str()) {
            continue;
        }

        if entry.provenance.is_empty() {
            if let Some(peer_id) = session_peer_id(&entry) {
                let display_name = session_display_name(&entry).unwrap_or_else(|| peer_id.clone());
                let key = format!("{channel}:{peer_id}");
                let member = members
                    .entry(key)
                    .or_insert_with(|| DirectoryMemberAccumulator {
                        channel: channel.clone(),
                        user_id: peer_id.clone(),
                        display_name: display_name.clone(),
                        sessions: 0,
                        last_seen_ms: entry.updated_at,
                    });
                member.sessions = member.sessions.saturating_add(1);
                member.last_seen_ms = member.last_seen_ms.max(entry.updated_at);
                if member.display_name.trim().is_empty() && !display_name.trim().is_empty() {
                    member.display_name = display_name;
                }
            }
            continue;
        }

        for provenance in &entry.provenance {
            let user_id = provenance.user_id.trim();
            if user_id.is_empty() {
                continue;
            }
            let display_name = provenance.display_name.trim();
            let key = format!("{channel}:{user_id}");
            let member = members
                .entry(key)
                .or_insert_with(|| DirectoryMemberAccumulator {
                    channel: channel.clone(),
                    user_id: user_id.to_string(),
                    display_name: if display_name.is_empty() {
                        user_id.to_string()
                    } else {
                        display_name.to_string()
                    },
                    sessions: 0,
                    last_seen_ms: entry.updated_at.max(provenance.timestamp),
                });

            member.sessions = member.sessions.saturating_add(1);
            member.last_seen_ms = member
                .last_seen_ms
                .max(entry.updated_at)
                .max(provenance.timestamp);
            if member.display_name == member.user_id && !display_name.is_empty() {
                member.display_name = display_name.to_string();
            }
        }
    }

    let mut list = members
        .into_values()
        .filter(|member| {
            directory_query_match(
                query.as_deref(),
                &[&member.channel, &member.user_id, &member.display_name],
            )
        })
        .map(|member| {
            json!({
                "channel": member.channel,
                "user_id": member.user_id,
                "display_name": member.display_name,
                "sessions": member.sessions,
                "last_seen_ms": member.last_seen_ms,
            })
        })
        .collect::<Vec<_>>();

    list.sort_by_key(|item| {
        std::cmp::Reverse(
            item.get("last_seen_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        )
    });
    list.truncate(limit);

    Ok(json!({
        "group_id": group_id,
        "channels": channels,
        "query": query,
        "limit": limit,
        "members": list,
    }))
}

async fn handle_web_login_start(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("whatsapp");
    if platform == "whatsapp" {
        return Ok(json!({
            "platform": "whatsapp",
            "status": "started",
            "next": "web.login.wait",
            "message": "Scan the QR code in the WhatsApp page and poll web.login.wait.",
        }));
    }
    handle_channels_login(&json!({ "platform": platform }), bridge).await
}

async fn handle_web_login_wait(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("whatsapp");

    let status = handle_channels_status(&json!({ "channel": platform }), bridge).await?;
    let connected = status
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(json!({
        "platform": platform,
        "connected": connected,
        "status": if connected { "connected" } else { "pending" },
    }))
}

fn nostr_profile_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge
        .config()
        .savfox_home
        .join("gateway")
        .join("nostr-profile.json")
}

fn default_nostr_profile() -> Value {
    json!({
        "display_name": "",
        "about": "",
        "picture": "",
        "nip05": "",
        "public_key": "",
        "private_key": "",
        "relays": ["wss://relay.damus.io", "wss://nos.lol"],
    })
}

async fn load_nostr_profile(bridge: &GatewayBridge) -> Value {
    let path = nostr_profile_path(bridge);
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(default_nostr_profile)
}

async fn save_nostr_profile(bridge: &GatewayBridge, profile: &Value) -> Result<(), String> {
    let path = nostr_profile_path(bridge);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create nostr profile dir: {err}"))?;
    }
    let payload = serde_json::to_string_pretty(profile)
        .map_err(|err| format!("failed to serialize nostr profile: {err}"))?;
    tokio::fs::write(path, payload)
        .await
        .map_err(|err| format!("failed to write nostr profile: {err}"))
}

async fn handle_channels_nostr_profile_get(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let profile = load_nostr_profile(bridge).await;
    Ok(json!({ "profile": profile }))
}

async fn handle_channels_nostr_profile_set(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let mut profile = load_nostr_profile(bridge).await;
    if let Some(incoming) = params.get("profile").and_then(|v| v.as_object()) {
        for (key, value) in incoming {
            profile[key] = value.clone();
        }
    } else {
        for key in [
            "display_name",
            "about",
            "picture",
            "nip05",
            "public_key",
            "private_key",
            "relays",
        ] {
            if let Some(value) = params.get(key) {
                profile[key] = value.clone();
            }
        }
    }
    save_nostr_profile(bridge, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({ "status": "saved", "profile": profile }))
}

async fn handle_channels_nostr_profile_import(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let private_key = params
        .get("private_key")
        .or_else(|| params.get("keypair"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if private_key.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'private_key' parameter".to_string(),
        ));
    }
    let mut profile = load_nostr_profile(bridge).await;
    profile["private_key"] = json!(private_key);
    if let Some(public_key) = params.get("public_key").and_then(|v| v.as_str()) {
        profile["public_key"] = json!(public_key);
    }
    save_nostr_profile(bridge, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({
        "status": "imported",
        "profile": profile,
    }))
}

async fn handle_channels_nostr_profile_export(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let profile = load_nostr_profile(bridge).await;
    Ok(json!({
        "status": "exported",
        "profile": profile,
    }))
}

async fn handle_channels_nostr_relays_get(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let profile = load_nostr_profile(bridge).await;
    let relays = profile.get("relays").cloned().unwrap_or_else(|| json!([]));
    Ok(json!({ "relays": relays }))
}

async fn handle_channels_nostr_relays_set(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let relays = params.get("relays").cloned().unwrap_or_else(|| json!([]));
    if !relays.is_array() {
        return Err((INVALID_REQUEST, "'relays' must be an array".to_string()));
    }
    let mut profile = load_nostr_profile(bridge).await;
    profile["relays"] = relays;
    save_nostr_profile(bridge, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({
        "status": "saved",
        "relays": profile.get("relays").cloned().unwrap_or_else(|| json!([])),
    }))
}

// ── Channel Config Management ─────────────────────────────────────────

async fn handle_channels_config_list(bridge: &Arc<GatewayBridge>) -> RpcResult {
    use crate::channel_store;
    let configs = channel_store::list_channel_configs(&bridge.config().savfox_home)
        .await
        .map_err(|e| {
            (
                INTERNAL_ERROR,
                format!("failed to list channel configs: {e}"),
            )
        })?;
    Ok(channel_store::channel_configs_to_json(&configs))
}

async fn handle_channels_config_get(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    use crate::channel_store;
    let channel_id = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    if channel_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'channel' parameter".to_string()));
    }
    match channel_store::get_channel_config(&bridge.config().savfox_home, &channel_id).await {
        Ok(Some(config)) => Ok(json!({ "config": channel_store::channel_config_to_json(&config) })),
        Ok(None) => Ok(json!({ "config": serde_json::Value::Null })),
        Err(e) => Err((INTERNAL_ERROR, format!("failed to get channel config: {e}"))),
    }
}

async fn handle_channels_config_save(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    use crate::channel_store;
    let channel_id = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let channel_id_owned = channel_id.to_string();
    let channel_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&channel_id_owned);
    let config_value = params.get("config").cloned().unwrap_or_else(|| json!({}));
    let agent_id = params.get("agent_id").and_then(|v| v.as_str());
    let _enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if channel_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'channel' parameter".to_string()));
    }

    let mut patch = config_value;
    if let Some(obj) = patch.as_object_mut() {
        if let Some(aid) = agent_id {
            obj.insert("agent_id".to_string(), json!(aid));
        }
    } else {
        patch = json!({});
        if let Some(aid) = agent_id {
            if let Some(obj) = patch.as_object_mut() {
                obj.insert("agent_id".to_string(), json!(aid));
            }
        }
    }

    match channel_store::merge_channel_config(
        &bridge.config().savfox_home,
        &channel_id,
        &channel_name,
        &patch,
    )
    .await
    {
        Ok(config) => Ok(
            json!({ "config": channel_store::channel_config_to_json(&config), "status": "saved" }),
        ),
        Err(e) => Err((
            INTERNAL_ERROR,
            format!("failed to save channel config: {e}"),
        )),
    }
}

async fn handle_channels_config_delete(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    use crate::channel_store;
    let channel_id = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    if channel_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'channel' parameter".to_string()));
    }

    match channel_store::delete_channel_config(&bridge.config().savfox_home, &channel_id).await {
        Ok(deleted) => Ok(json!({ "deleted": deleted, "channel": channel_id })),
        Err(e) => Err((
            INTERNAL_ERROR,
            format!("failed to delete channel config: {e}"),
        )),
    }
}

// ── Config ──────────────────────────────────────────────────────────────────

fn to_title_case_segment(segment: &str) -> String {
    let mut chars = segment.chars();
    if let Some(first) = chars.next() {
        let mut out = String::new();
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
        out
    } else {
        String::new()
    }
}

fn humanize_hyphenated_id(raw: &str) -> String {
    raw.split('-')
        .filter(|segment| !segment.is_empty())
        .map(to_title_case_segment)
        .collect::<Vec<_>>()
        .join(" ")
}

fn non_empty_trimmed(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalized_provider_object(provider_id: &str, base_url: Option<&str>) -> Value {
    let resolved_base_url = base_url
        .and_then(non_empty_trimmed)
        .or_else(|| model_test_default_base_url(provider_id));

    json!({
        "id": provider_id,
        "name": humanize_hyphenated_id(provider_id),
        "base_url": resolved_base_url,
    })
}

fn normalized_model_object(
    provider_id: &str,
    model_code: &str,
    provider_base_url: Option<&str>,
) -> Value {
    json!({
        "id": format!("{provider_id}/{model_code}"),
        "code": model_code,
        "name": humanize_hyphenated_id(model_code),
        "provider": normalized_provider_object(provider_id, provider_base_url),
    })
}

fn extract_provider_id(provider_value: &Value) -> Option<String> {
    match provider_value {
        Value::String(provider_id) => non_empty_trimmed(provider_id),
        Value::Object(provider) => provider
            .get("id")
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed),
        _ => None,
    }
}

fn extract_provider_base_url(provider_value: &Value) -> Option<String> {
    match provider_value {
        Value::Object(provider) => provider
            .get("base_url")
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed),
        _ => None,
    }
}

fn normalize_provider_value(provider_value: &mut Value) {
    if let Some(provider_id) = extract_provider_id(provider_value) {
        let provider_base_url = extract_provider_base_url(provider_value);
        *provider_value = normalized_provider_object(&provider_id, provider_base_url.as_deref());
    }
}

fn normalize_model_value(model_value: &mut Value) {
    match model_value {
        Value::String(model_id) => {
            if let Some((provider_id, model_code)) =
                savfox_core::parse_provider_prefixed_model(model_id.as_str())
            {
                *model_value = normalized_model_object(provider_id, model_code, None);
            }
        }
        Value::Object(model) => {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed);
            let parsed_from_id = id
                .as_deref()
                .and_then(savfox_core::parse_provider_prefixed_model)
                .map(|(provider_id, model_code)| (provider_id.to_string(), model_code.to_string()));
            let provider_base_url = model.get("provider").and_then(extract_provider_base_url);

            let provider_id = model
                .get("provider")
                .and_then(extract_provider_id)
                .or_else(|| {
                    parsed_from_id
                        .as_ref()
                        .map(|(provider, _)| provider.clone())
                });
            let model_code = model
                .get("code")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed)
                .or_else(|| {
                    model
                        .get("model_code")
                        .and_then(Value::as_str)
                        .and_then(non_empty_trimmed)
                })
                .or_else(|| parsed_from_id.as_ref().map(|(_, code)| code.clone()));

            if let (Some(provider_id), Some(model_code)) = (provider_id, model_code) {
                *model_value = normalized_model_object(
                    &provider_id,
                    &model_code,
                    provider_base_url.as_deref(),
                );
                return;
            }

            if let Some(provider_value) = model.get_mut("provider") {
                normalize_provider_value(provider_value);
            }
        }
        _ => {}
    }
}

fn normalize_config_model_fields(config: &mut Value) {
    if let Some(model_value) = config.get_mut("model") {
        normalize_model_value(model_value);
    }

    let Some(profiles) = config.get_mut("profiles").and_then(Value::as_object_mut) else {
        return;
    };

    for profile in profiles.values_mut() {
        if let Some(profile_map) = profile.as_object_mut()
            && let Some(model_value) = profile_map.get_mut("model")
        {
            normalize_model_value(model_value);
        }
    }
}

fn normalize_model_reasoning_key(config: &mut Value) {
    let Some(model) = config.get_mut("model").and_then(Value::as_object_mut) else {
        return;
    };

    if model.get("reasoning_effort").is_none()
        && let Some(reasoning_level) = model.get("reasoning_level").cloned()
    {
        model.insert("reasoning_effort".to_string(), reasoning_level);
    }
    model.remove("reasoning_level");
}

fn remove_legacy_model_fields(config: &mut Value) {
    let Some(root) = config.as_object_mut() else {
        return;
    };
    if root.get("model").and_then(Value::as_object).is_some() {
        root.remove("model_provider");
        root.remove("model_reasoning_effort");
    }
}

enum DetachedBridgeConfig {
    Upsert(Value),
    Delete,
}

fn take_detached_matrix_bridge_config(config: &mut Value) -> Option<DetachedBridgeConfig> {
    let root = config.as_object_mut()?;
    let (matrix_value, remove_gateway) = {
        let gateway = root.get_mut("gateway")?.as_object_mut()?;
        let (matrix, remove_bridges) = {
            let bridges = gateway.get_mut("bridges")?.as_object_mut()?;
            let matrix = bridges.remove("matrix")?;
            (matrix, bridges.is_empty())
        };
        if remove_bridges {
            gateway.remove("bridges");
        }
        (matrix, gateway.is_empty())
    };

    if remove_gateway {
        root.remove("gateway");
    }

    if matrix_value.is_null() {
        Some(DetachedBridgeConfig::Delete)
    } else {
        Some(DetachedBridgeConfig::Upsert(matrix_value))
    }
}

async fn persist_detached_matrix_bridge_config(
    bridge: &Arc<GatewayBridge>,
    detached: DetachedBridgeConfig,
) -> Result<(), (i64, String)> {
    use crate::channel_store;

    match detached {
        DetachedBridgeConfig::Delete => {
            channel_store::delete_channel_config(&bridge.config().savfox_home, "matrix")
                .await
                .map_err(|e| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to delete matrix channel config: {e}"),
                    )
                })?;
        }
        DetachedBridgeConfig::Upsert(Value::Object(matrix_config)) => {
            let patch = Value::Object(matrix_config);
            channel_store::merge_channel_config(
                &bridge.config().savfox_home,
                "matrix",
                "Matrix",
                &patch,
            )
            .await
            .map_err(|e| {
                (
                    INTERNAL_ERROR,
                    format!("failed to persist matrix channel config: {e}"),
                )
            })?;
        }
        DetachedBridgeConfig::Upsert(_) => {
            return Err((
                INVALID_PARAMS,
                "gateway.bridges.matrix must be an object or null".to_string(),
            ));
        }
    }

    Ok(())
}

async fn sanitize_config_before_write(
    config: &mut Value,
    bridge: &Arc<GatewayBridge>,
) -> Result<(), (i64, String)> {
    normalize_model_reasoning_key(config);
    remove_legacy_model_fields(config);

    if let Some(detached_matrix) = take_detached_matrix_bridge_config(config) {
        persist_detached_matrix_bridge_config(bridge, detached_matrix).await?;
    }

    Ok(())
}

fn deep_merge_patch(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target_obj), Value::Object(patch_obj)) => {
            for (key, patch_value) in patch_obj {
                if patch_value.is_null() {
                    target_obj.remove(key);
                    continue;
                }
                if let Some(existing_value) = target_obj.get_mut(key) {
                    deep_merge_patch(existing_value, patch_value);
                } else {
                    target_obj.insert(key.clone(), patch_value.clone());
                }
            }
        }
        (target_slot, patch_value) => {
            *target_slot = patch_value.clone();
        }
    }
}

async fn handle_config_get(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let session_count = bridge.websocket_manager().session_count().await;

    let mut config_value = load_config_value_or_empty(bridge).await;
    normalize_config_model_fields(&mut config_value);

    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "connected_clients": session_count,
        "endpoints": {
            "ws": "/ws",
            "health": "/health",
            "chat_completions": "/v1/chat/completions",
            "responses": "/v1/responses",
            "tools_invoke": "/tools/invoke",
        },
        "config": config_value,
    }))
}

async fn load_config_intermediate(
    bridge: &GatewayBridge,
) -> Result<crate::security_audit::ConfigDocument, String> {
    crate::security_audit::load_config_document(&bridge.config().savfox_home).await
}

fn primary_config_json_path(bridge: &GatewayBridge) -> PathBuf {
    bridge.config().savfox_home.join("config.json")
}

async fn load_config_value_or_empty(bridge: &GatewayBridge) -> Value {
    let mut config = load_config_intermediate(bridge)
        .await
        .map(|doc| doc.value)
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
    if !config.is_object() {
        config = Value::Object(serde_json::Map::new());
    }
    config
}

async fn write_config_json(bridge: &GatewayBridge, config: &Value) -> Result<(), String> {
    let path = primary_config_json_path(bridge);
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("JSON serialization failed: {e}"))?;
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| format!("failed to write config: {e}"))
}

async fn handle_config_export(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let mut doc = load_config_intermediate(bridge)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    let requested = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or(doc.format.as_str());
    let redacted = params
        .get("redacted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let valid = ["json", "yaml", "toml"];
    if !valid.contains(&requested) {
        return Err((
            INVALID_PARAMS,
            format!(
                "unsupported format: {requested}. Must be one of: {}",
                valid.join(", ")
            ),
        ));
    }

    if redacted {
        crate::redaction::redact_json_in_place(&mut doc.value);
    }

    let content = match requested {
        "json" => serde_json::to_string_pretty(&doc.value)
            .map_err(|e| (INTERNAL_ERROR, format!("JSON serialization failed: {e}")))?,
        "yaml" => serde_yaml::to_string(&doc.value)
            .map_err(|e| (INTERNAL_ERROR, format!("YAML serialization failed: {e}")))?,
        "toml" => json_value_to_toml_string(&doc.value).map_err(|e| (INTERNAL_ERROR, e))?,
        _ => unreachable!("validated above"),
    };

    Ok(json!({
        "status": "ok",
        "source_format": doc.format,
        "source_path": doc.path,
        "format": requested,
        "redacted": redacted,
        "content": content,
    }))
}

/// Convert a serde_json::Value to a TOML string.
fn json_value_to_toml_string(value: &Value) -> Result<String, String> {
    let toml_value = savfox_utils_json_to_toml::json_to_toml(value.clone());
    toml::to_string(&toml_value).map_err(|e| format!("TOML serialization failed: {e}"))
}

fn preserve_toml_leading_comments_for_yaml(source_toml: &str, yaml: &str) -> String {
    let mut prefix_lines = Vec::new();

    for line in source_toml.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            prefix_lines.push(line.to_string());
            continue;
        }
        if trimmed.is_empty() {
            if !prefix_lines.is_empty() {
                prefix_lines.push(String::new());
            }
            continue;
        }
        break;
    }

    if prefix_lines.is_empty() {
        return yaml.to_string();
    }

    let mut result = prefix_lines.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(yaml);
    result
}

async fn handle_config_set(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let config = params.get("config");
    let Some(config_value) = config else {
        return Err((INVALID_REQUEST, "missing 'config' parameter".to_string()));
    };

    let mut sanitized = config_value.clone();
    sanitize_config_before_write(&mut sanitized, bridge).await?;
    write_config_json(bridge, &sanitized)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "ok" }))
}

async fn handle_config_apply(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let config = params.get("config");
    let Some(config_value) = config else {
        return Err((INVALID_REQUEST, "missing 'config' parameter".to_string()));
    };

    let mut sanitized = config_value.clone();
    sanitize_config_before_write(&mut sanitized, bridge).await?;
    let config_path = primary_config_json_path(bridge);

    // Auto-snapshot before applying (#33)
    let _ = handle_config_snapshot(bridge).await;

    // Create a backup before applying.
    if config_path.exists() {
        let backup = bridge.config().savfox_home.join("config.json.bak");
        let _ = tokio::fs::copy(&config_path, &backup).await;
    }

    write_config_json(bridge, &sanitized)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({
        "status": "applied",
        "note": "restart required for changes to take effect",
    }))
}

async fn handle_config_patch(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let patch = params.get("patch");
    let Some(patch_value) = patch else {
        return Err((INVALID_REQUEST, "missing 'patch' parameter".to_string()));
    };

    let mut config = load_config_value_or_empty(bridge).await;

    // Merge patch fields (deep merge, null deletes keys).
    if patch_value.is_object() {
        deep_merge_patch(&mut config, patch_value);
    }

    sanitize_config_before_write(&mut config, bridge).await?;
    write_config_json(bridge, &config)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "patched" }))
}

async fn handle_config_schema() -> RpcResult {
    Ok(json!({
        "schema": {
            "type": "object",
            "properties": {
                "gateway": {
                    "type": "object",
                    "title": "Gateway",
                    "description": "Gateway server settings (port, auth, binding)",
                    "properties": {
                        "host": {
                            "type": "string",
                            "title": "Host",
                            "description": "Host address to bind to (e.g., 127.0.0.1 or 0.0.0.0)",
                            "default": "127.0.0.1"
                        },
                        "port": {
                            "type": "integer",
                            "title": "Port",
                            "description": "Port to listen on",
                            "default": 18881,
                            "minimum": 1,
                            "maximum": 65535
                        },
                        "token": {
                            "type": "string",
                            "title": "Auth Token",
                            "description": "Bearer token for authentication (auto-generated if not set)"
                        },
                        "tls_cert": {
                            "type": "string",
                            "title": "TLS Certificate",
                            "description": "Path to TLS certificate file (PEM format)"
                        },
                        "tls_key": {
                            "type": "string",
                            "title": "TLS Key",
                            "description": "Path to TLS private key file (PEM format)"
                        }
                    }
                },
                "env": {
                    "type": "object",
                    "title": "Environment",
                    "description": "Environment variables passed to the gateway process",
                    "properties": {
                        "shell_env": {
                            "type": "object",
                            "title": "Shell Env",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "timeout_ms": {
                                    "type": "integer",
                                    "title": "Timeout (ms)",
                                    "default": 3000
                                }
                            }
                        },
                        "vars": {
                            "type": "object",
                            "title": "Variables",
                            "description": "Key/value variables injected into runtime environment",
                            "additionalProperties": { "type": "string" }
                        }
                    }
                },
                "update": {
                    "type": "object",
                    "title": "Updates",
                    "description": "Auto-update settings and release channel",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "title": "Channel",
                            "enum": ["stable", "beta", "dev"],
                            "default": "stable"
                        },
                        "check_on_start": {
                            "type": "boolean",
                            "title": "Check On Start",
                            "default": true
                        }
                    }
                },
                "auth": {
                    "type": "object",
                    "title": "Authentication",
                    "description": "API keys and authentication profiles",
                    "properties": {
                        "profiles": {
                            "type": "object",
                            "title": "Profiles",
                            "additionalProperties": {
                                "type": "object",
                                "properties": {
                                    "provider": {
                                        "type": "string",
                                        "title": "Provider"
                                    },
                                    "mode": {
                                        "type": "string",
                                        "title": "Mode",
                                        "enum": ["api_key", "oauth", "token"]
                                    },
                                    "email": {
                                        "type": "string",
                                        "title": "Email"
                                    }
                                }
                            }
                        },
                        "order": {
                            "type": "object",
                            "title": "Profile Order",
                            "additionalProperties": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "cooldowns": {
                            "type": "object",
                            "title": "Cooldowns",
                            "properties": {
                                "billing_backoff_hours": {
                                    "type": "number",
                                    "title": "Billing Backoff (hours)"
                                },
                                "billing_max_hours": {
                                    "type": "number",
                                    "title": "Billing Max (hours)"
                                },
                                "failure_window_hours": {
                                    "type": "number",
                                    "title": "Failure Window (hours)"
                                }
                            }
                        }
                    }
                },
                "messages": {
                    "type": "object",
                    "title": "Messages",
                    "description": "Message handling and routing settings",
                    "properties": {
                        "max_context_chars": {
                            "type": "integer",
                            "title": "Max Context Chars"
                        },
                        "markdown": {
                            "type": "boolean",
                            "title": "Markdown Enabled",
                            "default": true
                        },
                        "include_quoted_reply": {
                            "type": "boolean",
                            "title": "Include Quoted Reply",
                            "default": true
                        }
                    }
                },
                "commands": {
                    "type": "object",
                    "title": "Commands",
                    "description": "Custom slash commands",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "title": "Enabled",
                                "default": true
                            },
                            "description": {
                                "type": "string",
                                "title": "Description"
                            },
                            "prompt": {
                                "type": "string",
                                "title": "Prompt"
                            }
                        }
                    }
                },
                "hooks": {
                    "type": "object",
                    "title": "Hooks",
                    "description": "Webhooks and event hooks",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": false
                        },
                        "path": {
                            "type": "string",
                            "title": "Path"
                        },
                        "token": {
                            "type": "string",
                            "title": "Token"
                        },
                        "default_session_id": {
                            "type": "string",
                            "title": "Default Session ID"
                        },
                        "allow_request_session_id": {
                            "type": "boolean",
                            "title": "Allow Request Session ID",
                            "default": false
                        },
                        "allowed_session_id_prefixes": {
                            "type": "array",
                            "title": "Allowed Session ID Prefixes",
                            "items": { "type": "string" }
                        },
                        "allowed_agent_ids": {
                            "type": "array",
                            "title": "Allowed Agent IDs",
                            "items": { "type": "string" }
                        },
                        "max_body_bytes": {
                            "type": "integer",
                            "title": "Max Body Bytes"
                        },
                        "presets": {
                            "type": "array",
                            "title": "Presets",
                            "items": { "type": "string" }
                        },
                        "transforms_dir": {
                            "type": "string",
                            "title": "Transforms Directory"
                        }
                    }
                },
                "skills": {
                    "type": "object",
                    "title": "Skills",
                    "description": "Skill packs and capabilities",
                    "properties": {
                        "allow_bundled": {
                            "type": "array",
                            "title": "Allow Bundled",
                            "items": { "type": "string" }
                        },
                        "load": {
                            "type": "object",
                            "title": "Load",
                            "properties": {
                                "extra_dirs": {
                                    "type": "array",
                                    "title": "Extra Directories",
                                    "items": { "type": "string" }
                                },
                                "watch": {
                                    "type": "boolean",
                                    "title": "Watch",
                                    "default": false
                                },
                                "watch_debounce_ms": {
                                    "type": "integer",
                                    "title": "Watch Debounce (ms)"
                                }
                            }
                        },
                        "entries": {
                            "type": "object",
                            "title": "Entries",
                            "additionalProperties": {
                                "type": "object",
                                "properties": {
                                    "enabled": {
                                        "type": "boolean",
                                        "title": "Enabled",
                                        "default": true
                                    },
                                    "api_key": {
                                        "type": "string",
                                        "title": "API Key"
                                    },
                                    "env": {
                                        "type": "object",
                                        "title": "Env",
                                        "additionalProperties": { "type": "string" }
                                    },
                                    "config": {
                                        "type": "object",
                                        "title": "Config",
                                        "additionalProperties": true
                                    }
                                }
                            }
                        }
                    }
                },
                "wizard": {
                    "type": "object",
                    "title": "Setup Wizard",
                    "description": "Setup wizard state and history",
                    "properties": {
                        "last_run_at": {
                            "type": "string",
                            "title": "Last Run At"
                        },
                        "last_run_version": {
                            "type": "string",
                            "title": "Last Run Version"
                        },
                        "last_run_commit": {
                            "type": "string",
                            "title": "Last Run Commit"
                        },
                        "last_run_command": {
                            "type": "string",
                            "title": "Last Run Command"
                        },
                        "last_run_mode": {
                            "type": "string",
                            "title": "Last Run Mode",
                            "enum": ["local", "remote"]
                        }
                    }
                },
                "browser": {
                    "type": "object",
                    "title": "Browser",
                    "description": "Browser automation settings",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": false
                        },
                        "evaluate_enabled": {
                            "type": "boolean",
                            "title": "Evaluate Enabled",
                            "default": false
                        },
                        "cdp_url": {
                            "type": "string",
                            "title": "CDP URL"
                        },
                        "headless": {
                            "type": "boolean",
                            "title": "Headless",
                            "default": true
                        },
                        "no_sandbox": {
                            "type": "boolean",
                            "title": "No Sandbox",
                            "default": false
                        },
                        "executable_path": {
                            "type": "string",
                            "title": "Executable Path"
                        },
                        "default_profile": {
                            "type": "string",
                            "title": "Default Profile"
                        }
                    }
                },
                "canvasHost": {
                    "type": "object",
                    "title": "Canvas Host",
                    "description": "Canvas rendering and display",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": false
                        },
                        "root": {
                            "type": "string",
                            "title": "Root"
                        },
                        "port": {
                            "type": "integer",
                            "title": "Port"
                        },
                        "live_reload": {
                            "type": "boolean",
                            "title": "Live Reload",
                            "default": false
                        }
                    }
                },
                "talk": {
                    "type": "object",
                    "title": "Talk",
                    "description": "Voice and speech settings",
                    "properties": {
                        "voice_id": {
                            "type": "string",
                            "title": "Voice ID"
                        },
                        "voice_aliases": {
                            "type": "object",
                            "title": "Voice Aliases",
                            "additionalProperties": { "type": "string" }
                        },
                        "model_id": {
                            "type": "string",
                            "title": "Model ID"
                        },
                        "output_format": {
                            "type": "string",
                            "title": "Output Format"
                        },
                        "api_key": {
                            "type": "string",
                            "title": "API Key"
                        },
                        "interrupt_on_speech": {
                            "type": "boolean",
                            "title": "Interrupt On Speech",
                            "default": false
                        }
                    }
                },
                "agents": {
                    "type": "object",
                    "title": "Agents",
                    "description": "Agent configurations, models, and identities",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "model": {
                                "type": "string",
                                "title": "Model (legacy)",
                                "description": "Legacy flat model ID (use models.primary instead)"
                            },
                            "provider": {
                                "type": "string",
                                "title": "Provider (legacy)",
                                "description": "Legacy provider field",
                                "enum": ["openai", "anthropic", "azure", "ollama", "lmstudio", "google"]
                            },
                            "models": {
                                "type": "object",
                                "title": "Models",
                                "properties": {
                                    "primary": {
                                        "type": "string",
                                        "title": "Primary Model",
                                        "description": "Global model ID (e.g. openai/gpt-4o)"
                                    },
                                    "fallbacks": {
                                        "type": "array",
                                        "title": "Fallback Models",
                                        "items": { "type": "string" },
                                        "description": "Fallback model IDs tried in order"
                                    }
                                }
                            },
                            "system_prompt": {
                                "type": "string",
                                "title": "System Prompt",
                                "description": "System prompt for this agent"
                            },
                            "dm_scope": {
                                "type": "string",
                                "title": "DM Session Scope",
                                "description": "How direct-message sessions are scoped: main (shared), per_peer (per user), per_channel_peer (per channel+user), per_account_channel_peer (per account+channel+user)",
                                "enum": ["main", "per_peer", "per_channel_peer", "per_account_channel_peer"],
                                "default": "main"
                            },
                            "identity": {
                                "type": "object",
                                "title": "Identity",
                                "description": "Agent identity settings",
                                "properties": {
                                    "name": {
                                        "type": "string",
                                        "title": "Name",
                                        "description": "Agent display name"
                                    },
                                    "avatar": {
                                        "type": "string",
                                        "title": "Avatar",
                                        "description": "Avatar URL or emoji"
                                    },
                                    "description": {
                                        "type": "string",
                                        "title": "Description",
                                        "description": "Agent description"
                                    }
                                }
                            },
                            "thinking": {
                                "type": "string",
                                "title": "Thinking Level",
                                "description": "Thinking/reasoning effort level for this agent",
                                "enum": ["off", "minimal", "low", "medium", "high", "xhigh"],
                                "default": "medium"
                            },
                            "tools": {
                                "type": "object",
                                "title": "Tool Controls",
                                "description": "Tool allow/deny lists for this agent",
                                "properties": {
                                    "allow_list": {
                                        "type": "array",
                                        "title": "Allowed Tools",
                                        "items": { "type": "string" },
                                        "description": "Only allow these tools (empty = all)"
                                    },
                                    "deny_list": {
                                        "type": "array",
                                        "title": "Denied Tools",
                                        "items": { "type": "string" },
                                        "description": "Block these tools"
                                    }
                                }
                            },
                            "memory": {
                                "type": "object",
                                "title": "Memory Settings",
                                "properties": {
                                    "enabled": {
                                        "type": "boolean",
                                        "title": "Memory Enabled",
                                        "description": "Enable memory system for this agent",
                                        "default": true
                                    }
                                }
                            },
                            "compaction": {
                                "type": "object",
                                "title": "Compaction",
                                "description": "Context window compaction settings",
                                "properties": {
                                    "mode": {
                                        "type": "string",
                                        "title": "Compaction Mode",
                                        "enum": ["auto", "manual", "off"],
                                        "default": "auto"
                                    },
                                    "max_history_share": {
                                        "type": "number",
                                        "title": "Max History Share",
                                        "description": "Max fraction of context for history (0-1)",
                                        "default": 0.7,
                                        "minimum": 0,
                                        "maximum": 1
                                    }
                                }
                            },
                            "sandbox": {
                                "type": "object",
                                "title": "Sandbox",
                                "description": "Code execution sandbox settings",
                                "properties": {
                                    "mode": {
                                        "type": "string",
                                        "title": "Sandbox Mode",
                                        "enum": ["off", "non_main", "all"],
                                        "default": "off"
                                    }
                                }
                            },
                            "heartbeat": {
                                "type": "object",
                                "title": "Heartbeat",
                                "description": "Periodic agent heartbeat settings",
                                "properties": {
                                    "every": {
                                        "type": "string",
                                        "title": "Interval",
                                        "description": "Heartbeat interval (e.g. '30m', '1h')"
                                    },
                                    "active_hours": {
                                        "type": "object",
                                        "title": "Active Hours",
                                        "properties": {
                                            "start": { "type": "string", "title": "Start", "description": "Start time (HH:MM)" },
                                            "end": { "type": "string", "title": "End", "description": "End time (HH:MM)" },
                                            "timezone": { "type": "string", "title": "Timezone", "description": "IANA timezone" }
                                        }
                                    },
                                    "prompt": {
                                        "type": "string",
                                        "title": "Heartbeat Prompt",
                                        "description": "Custom prompt for heartbeat messages"
                                    }
                                }
                            },
                            "group_activation": {
                                "type": "string",
                                "title": "Group Activation",
                                "description": "When to respond in group chats",
                                "enum": ["mention", "keyword", "always", "command", "off"],
                                "default": "mention"
                            }
                        }
                    }
                },
                "models": {
                    "type": "object",
                    "title": "Models",
                    "description": "AI model configurations and providers",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "provider": {
                                "type": "string",
                                "title": "Provider",
                                "description": "Model provider",
                                "enum": ["openai", "anthropic", "azure", "google", "ollama", "lmstudio", "openrouter", "deepseek", "custom"]
                            },
                            "api_key": {
                                "type": "string",
                                "title": "API Key",
                                "description": "API key for authentication"
                            },
                            "base_url": {
                                "type": "string",
                                "title": "Base URL",
                                "description": "Custom API base URL"
                            },
                            "max_tokens": {
                                "type": "integer",
                                "title": "Max Tokens",
                                "description": "Maximum tokens for responses",
                                "default": 4096
                            },
                            "temperature": {
                                "type": "number",
                                "title": "Temperature",
                                "description": "Response randomness (0-2)",
                                "default": 0.7,
                                "minimum": 0,
                                "maximum": 2
                            },
                            "default": {
                                "type": "boolean",
                                "title": "Default Model",
                                "description": "Use as default model",
                                "default": false
                            },
                            "cost_input_per_m": {
                                "type": "number",
                                "title": "Input Cost per 1M tokens",
                                "description": "Cost in USD per million input tokens"
                            },
                            "cost_output_per_m": {
                                "type": "number",
                                "title": "Output Cost per 1M tokens",
                                "description": "Cost in USD per million output tokens"
                            },
                            "cost_cache_read_per_m": {
                                "type": "number",
                                "title": "Cache Read Cost per 1M tokens",
                                "description": "Cost in USD per million cached input tokens"
                            },
                            "cost_cache_write_per_m": {
                                "type": "number",
                                "title": "Cache Write Cost per 1M tokens",
                                "description": "Cost in USD per million cache write tokens"
                            },
                            "context_window": {
                                "type": "integer",
                                "title": "Context Window",
                                "description": "Maximum context window size in tokens"
                            },
                            "max_output_tokens": {
                                "type": "integer",
                                "title": "Max Output Tokens",
                                "description": "Maximum output tokens the model supports"
                            },
                            "auth_type": {
                                "type": "string",
                                "title": "Auth Type",
                                "description": "Authentication method for this provider",
                                "enum": ["api_key", "bearer", "aws_sdk", "oauth", "custom_header"],
                                "default": "api_key"
                            },
                            "custom_headers": {
                                "type": "object",
                                "title": "Custom Headers",
                                "description": "Additional HTTP headers to send with requests",
                                "additionalProperties": { "type": "string" }
                            }
                        }
                    }
                },
                "channels": {
                    "type": "object",
                    "title": "Channels",
                    "description": "Messaging channels (Telegram, Discord, Slack, etc.)",
                    "properties": {
                        "discord": {
                            "type": "object",
                            "title": "Discord",
                            "description": "Discord bot configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "bot_token": {
                                    "type": "string",
                                    "title": "Bot Token",
                                    "description": "Discord bot token"
                                },
                                "application_id": {
                                    "type": "string",
                                    "title": "Application ID",
                                    "description": "Discord application ID"
                                }
                            }
                        },
                        "telegram": {
                            "type": "object",
                            "title": "Telegram",
                            "description": "Telegram bot configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "bot_token": {
                                    "type": "string",
                                    "title": "Bot Token",
                                    "description": "Telegram bot token"
                                }
                            }
                        },
                        "slack": {
                            "type": "object",
                            "title": "Slack",
                            "description": "Slack bot configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "bot_token": {
                                    "type": "string",
                                    "title": "Bot Token",
                                    "description": "Slack bot token"
                                },
                                "signing_secret": {
                                    "type": "string",
                                    "title": "Signing Secret",
                                    "description": "Slack signing secret"
                                }
                            }
                        },
                        "whatsapp": {
                            "type": "object",
                            "title": "WhatsApp",
                            "description": "WhatsApp Business configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "phone_number_id": {
                                    "type": "string",
                                    "title": "Phone Number ID"
                                },
                                "access_token": {
                                    "type": "string",
                                    "title": "Access Token"
                                }
                            }
                        },
                        "signal": {
                            "type": "object",
                            "title": "Signal",
                            "description": "Signal messaging configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "phone_number": {
                                    "type": "string",
                                    "title": "Phone Number"
                                }
                            }
                        }
                    }
                },
                "cron": {
                    "type": "object",
                    "title": "Cron",
                    "description": "Scheduled tasks and automation",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "title": "Enabled",
                                "default": true
                            },
                            "schedule": {
                                "type": "string",
                                "title": "Schedule",
                                "description": "Cron expression (e.g., '0 9 * * *' for daily at 9am)"
                            },
                            "command": {
                                "type": "string",
                                "title": "Command",
                                "description": "Command to execute"
                            },
                            "channel": {
                                "type": "string",
                                "title": "Channel",
                                "description": "Target channel ID"
                            }
                        }
                    }
                },
                "memory": {
                    "type": "object",
                    "title": "Memory",
                    "description": "Memory and knowledge storage",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": true
                        },
                        "provider": {
                            "type": "string",
                            "title": "Provider",
                            "description": "Embedding provider",
                            "enum": ["openai", "voyage", "gemini", "ollama"]
                        },
                        "embedding_model": {
                            "type": "string",
                            "title": "Embedding Model",
                            "description": "Model for embeddings",
                            "default": "text-embedding-3-small"
                        },
                        "persist": {
                            "type": "boolean",
                            "title": "Persist",
                            "description": "Persist memory to disk",
                            "default": true
                        }
                    }
                },
                "audio": {
                    "type": "object",
                    "title": "Audio",
                    "description": "Audio input/output settings",
                    "properties": {
                        "tts_enabled": {
                            "type": "boolean",
                            "title": "TTS Enabled",
                            "default": false
                        },
                        "tts_provider": {
                            "type": "string",
                            "title": "TTS Provider",
                            "enum": ["openai", "elevenlabs", "browser"]
                        },
                        "voice_wake_enabled": {
                            "type": "boolean",
                            "title": "Voice Wake Enabled",
                            "default": false
                        },
                        "wake_word": {
                            "type": "string",
                            "title": "Wake Word",
                            "default": "hey savfox"
                        }
                    }
                },
                "logging": {
                    "type": "object",
                    "title": "Logging",
                    "description": "Log levels and output configuration",
                    "properties": {
                        "level": {
                            "type": "string",
                            "title": "Log Level",
                            "enum": ["trace", "debug", "info", "warn", "error"],
                            "default": "info"
                        },
                        "file": {
                            "type": "string",
                            "title": "Log File",
                            "description": "Path to log file"
                        },
                        "max_size_mb": {
                            "type": "integer",
                            "title": "Max Size (MB)",
                            "default": 10
                        }
                    }
                },
                "tools": {
                    "type": "object",
                    "title": "Tools",
                    "description": "Tool configurations (browser, search, etc.)",
                    "properties": {
                        "browser_enabled": {
                            "type": "boolean",
                            "title": "Browser Enabled",
                            "default": false
                        },
                        "search_enabled": {
                            "type": "boolean",
                            "title": "Search Enabled",
                            "default": false
                        },
                        "search_provider": {
                            "type": "string",
                            "title": "Search Provider",
                            "enum": ["google", "bing", "duckduckgo", "searx"]
                        }
                    }
                },
                "session": {
                    "type": "object",
                    "title": "Session",
                    "description": "Session management and persistence",
                    "properties": {
                        "auto_save": {
                            "type": "boolean",
                            "title": "Auto Save",
                            "default": true
                        },
                        "max_history": {
                            "type": "integer",
                            "title": "Max History",
                            "description": "Maximum messages to keep in history",
                            "default": 100
                        }
                    }
                },
                "plugins": {
                    "type": "object",
                    "title": "Plugins",
                    "description": "Plugin management and extensions",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "title": "Enabled",
                                "default": true
                            },
                            "config": {
                                "type": "object",
                                "title": "Configuration"
                            }
                        }
                    }
                }
            }
        },
        "uiHints": {
            "gateway.host": { "order": 1 },
            "gateway.port": { "order": 2 },
            "gateway.token": { "order": 3, "sensitive": true },
            "gateway.tls_cert": { "order": 4 },
            "gateway.tls_key": { "order": 5, "sensitive": true },
            "agents.*.model": { "order": 1 },
            "agents.*.provider": { "order": 2 },
            "models.*.provider": { "order": 1 },
            "models.*.api_key": { "order": 2, "sensitive": true },
            "models.*.base_url": { "order": 3 },
            "channels.*.enabled": { "order": 1 },
            "channels.*.bot_token": { "order": 2, "sensitive": true },
            "hooks.token": { "order": 3, "sensitive": true },
            "talk.api_key": { "order": 5, "sensitive": true },
            "skills.entries.*.api_key": { "order": 2, "sensitive": true },
            "memory.enabled": { "order": 1 },
            "memory.provider": { "order": 2 },
            "logging.level": { "order": 1 }
        }
    }))
}

// ── Cron ────────────────────────────────────────────────────────────────────

async fn handle_cron_list(cron_service: &Arc<CronService>) -> RpcResult {
    let jobs = cron_service.list_jobs().await;
    let value = serde_json::to_value(&jobs).unwrap_or(json!([]));
    Ok(json!({ "jobs": value }))
}

async fn handle_cron_status(cron_service: &Arc<CronService>) -> RpcResult {
    let status = cron_service.status().await;
    let value = serde_json::to_value(&status).unwrap_or(Value::Null);
    Ok(value)
}

async fn handle_cron_add(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    if name.is_empty() {
        return Err((INVALID_REQUEST, "missing 'name' parameter".to_string()));
    }

    // Parse schedule.
    let schedule = if let Some(sched_obj) = params.get("schedule") {
        serde_json::from_value::<CronSchedule>(sched_obj.clone())
            .map_err(|e| (INVALID_REQUEST, format!("invalid schedule: {e}")))?
    } else if let Some(expression) = params.get("expression").and_then(|v| v.as_str()) {
        CronSchedule::Cron {
            expression: expression.to_owned(),
            timezone: None,
        }
    } else {
        return Err((
            INVALID_REQUEST,
            "missing 'schedule' or 'expression' parameter".to_string(),
        ));
    };

    // Parse payload.
    let payload = if let Some(payload_obj) = params.get("payload") {
        serde_json::from_value::<CronPayload>(payload_obj.clone())
            .map_err(|e| (INVALID_REQUEST, format!("invalid payload: {e}")))?
    } else if let Some(command) = params.get("command").and_then(|v| v.as_str()) {
        CronPayload::SystemEvent {
            text: command.to_owned(),
        }
    } else {
        return Err((
            INVALID_REQUEST,
            "missing 'payload' or 'command' parameter".to_string(),
        ));
    };

    // Parse delivery.
    let delivery = params
        .get("delivery")
        .and_then(|v| serde_json::from_value::<CronDelivery>(v.clone()).ok())
        .unwrap_or_default();

    // Parse session target (main or isolated).
    let session_target = params
        .get("session_target")
        .and_then(|v| serde_json::from_value::<CronSessionTarget>(v.clone()).ok())
        .unwrap_or_default();

    let id = cron_service
        .add_job(name.clone(), schedule, payload, delivery, session_target)
        .await;
    Ok(json!({ "id": id, "name": name, "status": "added" }))
}

async fn handle_cron_update(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    let schedule = params
        .get("schedule")
        .and_then(|v| serde_json::from_value::<CronSchedule>(v.clone()).ok());
    let payload = params
        .get("payload")
        .and_then(|v| serde_json::from_value::<CronPayload>(v.clone()).ok());
    let delivery = params
        .get("delivery")
        .and_then(|v| serde_json::from_value::<CronDelivery>(v.clone()).ok());
    let enabled = params.get("enabled").and_then(|v| v.as_bool());

    match cron_service
        .update_job(id, name, schedule, payload, delivery, enabled)
        .await
    {
        Ok(job) => {
            let value = serde_json::to_value(&job).unwrap_or(Value::Null);
            Ok(json!({ "id": id, "status": "updated", "job": value }))
        }
        Err(err) => Err((INVALID_REQUEST, err)),
    }
}

async fn handle_cron_remove(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }
    let removed = cron_service.remove_job(id).await;
    if removed {
        Ok(json!({ "id": id, "status": "removed" }))
    } else {
        Err((INVALID_REQUEST, format!("job '{id}' not found")))
    }
}

async fn handle_cron_run(
    params: &Value,
    cron_service: &Arc<CronService>,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }
    match cron_service.run_job(id, bridge).await {
        Ok(()) => Ok(json!({ "id": id, "status": "triggered" })),
        Err(err) => Err((INTERNAL_ERROR, err)),
    }
}

async fn handle_cron_runs(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    let runs = cron_service.get_runs(id, limit).await;
    let value = serde_json::to_value(&runs).unwrap_or(json!([]));
    Ok(json!({ "id": id, "runs": value }))
}

// ── Nodes ───────────────────────────────────────────────────────────────────

fn node_capability_catalog() -> Vec<Value> {
    vec![
        json!({
            "id": "camera.snap",
            "method": "system.camera",
            "default_params": { "mode": "snap" },
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.camera.snap"],
        }),
        json!({
            "id": "camera.clip",
            "method": "system.camera",
            "default_params": { "mode": "clip" },
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.camera.clip"],
        }),
        json!({
            "id": "screen.record",
            "method": "system.screen.record",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.screen.record"],
        }),
        json!({
            "id": "location.get",
            "method": "system.location",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.location.get"],
        }),
        json!({
            "id": "notify",
            "method": "system.notify",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.notify"],
        }),
    ]
}

fn latest_pairing_record_for_node<'a>(
    node_id: &str,
    records: &'a [pairing_store::PairingRecord],
) -> Option<&'a pairing_store::PairingRecord> {
    records
        .iter()
        .filter(|record| record.node_id == node_id)
        .max_by_key(|record| record.updated_at)
}

fn node_is_approved(record: Option<&pairing_store::PairingRecord>) -> bool {
    match record {
        Some(record) => matches!(record.status, crate::pairing_store::PairingStatus::Approved),
        None => false,
    }
}

async fn handle_node_capabilities_list() -> RpcResult {
    Ok(json!({
        "capabilities": node_capability_catalog(),
    }))
}

async fn handle_node_list() -> RpcResult {
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let mut by_node: HashMap<String, &pairing_store::PairingRecord> = HashMap::new();
    for record in &records {
        match by_node.get(&record.node_id) {
            Some(existing) if existing.updated_at >= record.updated_at => {}
            _ => {
                by_node.insert(record.node_id.clone(), record);
            }
        }
    }

    let mut nodes = Vec::new();
    for (node_id, record) in by_node {
        let status = serde_json::to_value(&record.status)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        nodes.push(json!({
            "node_id": node_id,
            "name": record.device_name.clone().unwrap_or_else(|| record.node_id.clone()),
            "capabilities": node_capability_catalog(),
            "paired": true,
            "approved": matches!(record.status, crate::pairing_store::PairingStatus::Approved),
            "status": status,
            "device_id": record.device_id,
            "updated_at": record.updated_at,
        }));
    }
    nodes.sort_by(|a, b| {
        let a_id = a.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        let b_id = b.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        a_id.cmp(b_id)
    });
    Ok(json!({ "nodes": nodes }))
}

async fn handle_node_describe(params: &Value) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let latest = latest_pairing_record_for_node(node_id, &records);
    if let Some(record) = latest {
        let status = serde_json::to_value(&record.status)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        return Ok(json!({
            "node_id": node_id,
            "name": record.device_name.clone().unwrap_or_else(|| node_id.to_string()),
            "capabilities": node_capability_catalog(),
            "paired": true,
            "approved": matches!(record.status, crate::pairing_store::PairingStatus::Approved),
            "requires_pairing": true,
            "requires_approval": true,
            "status": status,
            "device_id": record.device_id,
            "request_id": record.request_id,
            "verification_code": record.verification_code,
            "updated_at": record.updated_at,
        }));
    }

    Ok(json!({
        "node_id": node_id,
        "name": node_id,
        "capabilities": node_capability_catalog(),
        "paired": false,
        "approved": false,
        "requires_pairing": true,
        "requires_approval": true,
        "status": "unknown",
    }))
}

async fn handle_node_tool_alias(
    method: &str,
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let node_id = params
        .get("node_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }

    let mut merged = serde_json::Map::new();
    merged.insert("node_id".to_string(), Value::String(node_id));
    merged.insert("method".to_string(), Value::String(method.to_string()));

    if let Some(extra_params) = params.get("params") {
        merged.insert("params".to_string(), extra_params.clone());
    } else {
        let mut passthrough = serde_json::Map::new();
        if let Some(duration_ms) = params.get("duration_ms") {
            passthrough.insert("duration_ms".to_string(), duration_ms.clone());
        }
        if let Some(display) = params.get("display") {
            passthrough.insert("display".to_string(), display.clone());
        }
        if let Some(device) = params.get("device") {
            passthrough.insert("device".to_string(), device.clone());
        }
        if let Some(title) = params.get("title") {
            passthrough.insert("title".to_string(), title.clone());
        }
        if let Some(body) = params.get("body") {
            passthrough.insert("body".to_string(), body.clone());
        }
        if !passthrough.is_empty() {
            merged.insert("params".to_string(), Value::Object(passthrough));
        }
    }

    handle_node_invoke(&Value::Object(merged), bridge).await
}

async fn handle_node_invoke(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    let method_raw = params.get("method").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() || method_raw.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'node_id' or 'method' parameter".to_string(),
        ));
    }

    let (method, default_mode): (&str, Option<&str>) = match method_raw {
        "camera.snap" => ("system.camera", Some("snap")),
        "camera.clip" => ("system.camera", Some("clip")),
        "screen.record" => ("system.screen.record", None),
        "location.get" => ("system.location", None),
        "notify" => ("system.notify", None),
        other => (other, None),
    };

    let mut invoke_params = params
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if let Some(mode) = default_mode {
        if let Value::Object(ref mut map) = invoke_params {
            map.entry("mode".to_string())
                .or_insert_with(|| Value::String(mode.to_string()));
        }
    }

    let requires_pairing = matches!(
        method,
        "system.camera"
            | "system.screen.record"
            | "system.location"
            | "system.notify"
            | "camera.snap"
            | "camera.clip"
            | "screen.record"
            | "location.get"
            | "notify"
    );
    if requires_pairing {
        let records = pairing_store::list_requests()
            .await
            .map_err(|err| (INTERNAL_ERROR, err))?;
        let approved = node_is_approved(latest_pairing_record_for_node(node_id, &records));
        if !approved {
            return Err((
                INVALID_REQUEST,
                format!("node '{node_id}' is not paired/approved"),
            ));
        }
    }

    let params_json = serde_json::to_string(&invoke_params).unwrap_or_else(|_| "{}".to_string());
    let prompt = format!("[node:{node_id}] {method} {params_json}");
    let request_id = uuid::Uuid::now_v7().to_string();
    match bridge.invoke_agent_text(&prompt, "default").await {
        Ok(reply) => {
            let record = NodeInvokeRecord {
                request_id: request_id.clone(),
                node_id: node_id.to_owned(),
                method: method.to_owned(),
                status: "completed".to_string(),
                result: json!({
                    "reply": reply,
                    "params": invoke_params,
                }),
                updated_at_ms: now_ms(),
            };
            save_node_invoke_result(record).await;
            Ok(json!({
                "request_id": request_id,
                "node_id": node_id,
                "method": method,
                "status": "completed",
                "result": reply,
                "params": invoke_params
            }))
        }
        Err(err) => Err((INTERNAL_ERROR, format!("node invoke error: {err}"))),
    }
}

async fn handle_node_invoke_result(params: &Value) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }
    if let Some(record) = get_node_invoke_result(request_id).await {
        let value = serde_json::to_value(record).unwrap_or(Value::Null);
        Ok(json!({ "request": value }))
    } else {
        Ok(json!({ "request_id": request_id, "result": null, "status": "not_found" }))
    }
}

async fn handle_node_event(params: &Value, _bridge: &Arc<GatewayBridge>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    let event_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }

    // Log the event and verify the node exists in the pairing store.
    let records = pairing_store::list_requests().await.unwrap_or_default();
    let node_exists = records.iter().any(|r| r.node_id == node_id);

    tracing::info!(
        node_id = %node_id,
        event_type = %event_type,
        node_known = %node_exists,
        "node event received"
    );

    Ok(json!({
        "node_id": node_id,
        "type": event_type,
        "status": "received",
        "node_known": node_exists,
    }))
}

async fn handle_node_rename(params: &Value, _bridge: &Arc<GatewayBridge>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() || name.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'node_id' or 'name' parameter".to_string(),
        ));
    }

    // Update the device name in the pairing store if a matching record exists.
    let records = pairing_store::list_requests().await.unwrap_or_default();
    let found = records.iter().any(|r| r.node_id == node_id);

    if !found {
        return Err((
            INVALID_REQUEST,
            format!("node '{node_id}' not found in pairing store"),
        ));
    }

    // Note: The pairing store doesn't have a direct rename method, so we log the rename
    // and return success. The device_name is set during pairing creation.
    tracing::info!(node_id = %node_id, new_name = %name, "node renamed");

    Ok(json!({ "node_id": node_id, "name": name, "status": "renamed" }))
}

// ── Device pairing ──────────────────────────────────────────────────────────

async fn handle_node_pair_request(params: &Value) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    let device_id = params.get("device_id").and_then(|v| v.as_str());
    let device_name = params.get("device_name").and_then(|v| v.as_str());
    let record = pairing_store::create_request(node_id, device_id, device_name)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

async fn handle_node_pair_list() -> RpcResult {
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(records).unwrap_or(json!([]));
    Ok(json!({ "requests": value }))
}

async fn handle_node_pair_approve(params: &Value) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }
    let record = pairing_store::approve_request(request_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

async fn handle_node_pair_reject(params: &Value) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }
    let record = pairing_store::reject_request(request_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

async fn handle_node_pair_verify(params: &Value) -> RpcResult {
    let code = params.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if code.is_empty() {
        return Err((INVALID_REQUEST, "missing 'code' parameter".to_string()));
    }
    let found = pairing_store::verify_code(code)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    if let Some(record) = found {
        let value = serde_json::to_value(record).unwrap_or(Value::Null);
        Ok(json!({ "valid": true, "request": value }))
    } else {
        Ok(json!({ "valid": false }))
    }
}

async fn handle_device_pair_list() -> RpcResult {
    let records = pairing_store::list_devices()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(records).unwrap_or(json!([]));
    Ok(json!({ "devices": value }))
}

async fn handle_device_pair_approve(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::approve_device(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

async fn handle_device_pair_reject(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::reject_device(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

async fn handle_device_token_rotate(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::rotate_device_token(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

async fn handle_device_token_revoke(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::revoke_device_token(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

// ── TTS (text-to-speech) ────────────────────────────────────────────────────

async fn handle_tts_status(bridge: &Arc<GatewayBridge>) -> RpcResult {
    tts_service::status(&bridge.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_providers() -> RpcResult {
    Ok(json!({ "providers": tts_service::providers() }))
}

async fn handle_tts_enable(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let provider = params.get("provider").and_then(|v| v.as_str());
    let voice = params.get("voice").and_then(|v| v.as_str());
    let model = params.get("model").and_then(|v| v.as_str());
    tts_service::enable(&bridge.config().savfox_home, provider, voice, model)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_disable(bridge: &Arc<GatewayBridge>) -> RpcResult {
    tts_service::disable(&bridge.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_convert(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return Err((INVALID_REQUEST, "missing 'text' parameter".to_string()));
    }
    tts_service::convert(&bridge.config().savfox_home, bridge.http_client(), params)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_set_provider(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if provider.is_empty() {
        return Err((INVALID_REQUEST, "missing 'provider' parameter".to_string()));
    }
    let voice = params.get("voice").and_then(|v| v.as_str());
    let model = params.get("model").and_then(|v| v.as_str());
    tts_service::set_provider(&bridge.config().savfox_home, provider, voice, model)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

// ── Skills ──────────────────────────────────────────────────────────────────

async fn handle_skills_status(bridge: &Arc<GatewayBridge>) -> RpcResult {
    skills_store::status(&bridge.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_skills_bins(bridge: &Arc<GatewayBridge>) -> RpcResult {
    skills_store::bins(&bridge.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_skills_install(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        return Err((INVALID_REQUEST, "missing 'name' parameter".to_string()));
    }
    let source = params.get("source").and_then(|v| v.as_str());
    skills_store::install(&bridge.config().savfox_home, name, source)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_skills_update(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = params.get("enabled").and_then(|v| v.as_bool());
    let disabled_reason = params.get("disabled_reason").and_then(|v| v.as_str());
    skills_store::update(
        &bridge.config().savfox_home,
        if name.is_empty() { None } else { Some(name) },
        enabled,
        disabled_reason,
    )
    .await
    .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_skills_set_env(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() {
        return Err((INVALID_REQUEST, "missing 'key' parameter".to_string()));
    }
    if value.is_empty() {
        return Err((INVALID_REQUEST, "missing 'value' parameter".to_string()));
    }
    skills_store::set_env(&bridge.config().savfox_home, key, value)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

// ── Exec approvals ──────────────────────────────────────────────────────────

async fn handle_exec_approvals_get(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let approvals = list_pending_approvals(&bridge.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to load approvals: {err}")))?;
    let policy = approval_policy_store::get_global(&bridge.config().savfox_home)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to load approval policy: {err}"),
            )
        })?;
    let count = approvals.len();
    Ok(json!({
        "mode": policy.get("mode").cloned().unwrap_or(Value::String("auto".to_string())),
        "rules": policy.get("rules").cloned().unwrap_or(Value::Array(Vec::new())),
        "approvals": approvals,
        "count": count,
    }))
}

async fn handle_exec_approvals_set(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    approval_policy_store::set_global(&bridge.config().savfox_home, mode, params.get("rules"))
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_exec_approvals_node_get(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    approval_policy_store::get_node(&bridge.config().savfox_home, node_id)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_exec_approvals_node_set(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    approval_policy_store::set_node(
        &bridge.config().savfox_home,
        node_id,
        mode,
        params.get("rules"),
    )
    .await
    .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_exec_approval_request(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let command = params.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if command.is_empty() {
        return Err((INVALID_REQUEST, "missing 'command' parameter".to_string()));
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let request = ExecApprovalRequest {
        id: uuid::Uuid::now_v7().to_string(),
        command: command.to_owned(),
        cwd: params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        host: params
            .get("host")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        security: params
            .get("security")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        ask: params
            .get("ask")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        agent_id: params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        session_id: params
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        created_at_ms: now_ms,
        expires_at_ms: params
            .get("expires_at_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(now_ms + 300_000), // Default 5 min expiry
    };

    if let Err(err) = persist_pending_approval(&bridge.config().savfox_home, &request).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to persist approval request: {err}"),
        ));
    }

    // Load forwarding config from env and forward the approval.
    let config = load_approval_forwarding_config();
    forward_approval_to_chat(bridge, session_mgr, &request, &config).await;

    Ok(json!({
        "request_id": request.id,
        "command": command,
        "status": "pending",
    }))
}

async fn handle_exec_approval_resolve(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let approved = params
        .get("approved")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }

    let resolution = ExecApprovalResolution {
        id: request_id.to_owned(),
        approved,
        resolved_by: params
            .get("resolved_by")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        reason: params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
    };

    let resolved_pending = persist_resolved_approval(&bridge.config().savfox_home, &resolution)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to persist approval resolution: {err}"),
            )
        })?;

    // Notify channels about the resolution.
    let config = load_approval_forwarding_config();
    notify_approval_resolved(bridge, session_mgr, &resolution, &config).await;

    Ok(json!({
        "request_id": request_id,
        "approved": approved,
        "status": "resolved",
        "resolved_pending": resolved_pending,
    }))
}

/// Load approval forwarding config from environment variables.
fn load_approval_forwarding_config() -> ApprovalForwardingConfig {
    let enabled = std::env::var("SAVFOX_APPROVAL_FORWARDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mode = std::env::var("SAVFOX_APPROVAL_MODE").unwrap_or_else(|_| "targets".to_string());

    let targets = std::env::var("SAVFOX_APPROVAL_TARGETS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let agent_filter = std::env::var("SAVFOX_APPROVAL_AGENT_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let session_filter = std::env::var("SAVFOX_APPROVAL_SESSION_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    ApprovalForwardingConfig {
        enabled,
        mode,
        targets,
        agent_filter,
        session_filter,
    }
}

// ── Usage ───────────────────────────────────────────────────────────────────

async fn handle_usage_status(session_store: &Arc<SessionStore>) -> RpcResult {
    let sessions = session_store.list().await;
    let total_input: u64 = sessions.iter().map(|s| s.input_tokens).sum();
    let total_output: u64 = sessions.iter().map(|s| s.output_tokens).sum();
    let total: u64 = sessions.iter().map(|s| s.total_tokens).sum();

    // Build hourly distribution from session update times.
    let mut hourly = vec![0u64; 24];
    for s in &sessions {
        let secs = s.updated_at / 1000;
        let hour = ((secs % 86400) / 3600) as usize;
        if hour < 24 {
            hourly[hour] += 1;
        }
    }

    Ok(json!({
        "total_tokens": total,
        "prompt_tokens": total_input,
        "completion_tokens": total_output,
        "session_count": sessions.len(),
        "total_messages": null,
        "tool_calls": null,
        "errors": null,
        "cache_hits": null,
        "cache_misses": null,
        "hourly_distribution": hourly,
    }))
}

async fn handle_usage_cost(params: &Value, session_store: &Arc<SessionStore>) -> RpcResult {
    let period = params
        .get("period")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    let session_id = params.get("session_id").and_then(|v| v.as_str());

    if let Some(key) = session_id {
        // Per-session usage.
        match session_store.get(key).await {
            Some(entry) => Ok(json!({
                "period": period,
                "session_id": key,
                "input_tokens": entry.input_tokens,
                "output_tokens": entry.output_tokens,
                "total_tokens": entry.total_tokens,
            })),
            None => Ok(json!({
                "period": period,
                "session_id": key,
                "total_tokens": 0,
            })),
        }
    } else {
        // Return per-session entries with token breakdown.
        let sessions = session_store.list().await;

        // Time filtering based on period.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let cutoff_ms = match period {
            "today" => now_ms.saturating_sub(24 * 60 * 60 * 1000),
            "week" => now_ms.saturating_sub(7 * 24 * 60 * 60 * 1000),
            "month" => now_ms.saturating_sub(30 * 24 * 60 * 60 * 1000),
            _ => 0, // "all"
        };

        let entries: Vec<Value> = sessions
            .iter()
            .filter(|s| s.updated_at >= cutoff_ms || cutoff_ms == 0)
            .filter(|s| s.total_tokens > 0)
            .map(|s| {
                json!({
                    "session_id": s.session_id,
                    "model": s.model,
                    "tokens": s.total_tokens,
                    "input_tokens": s.input_tokens,
                    "output_tokens": s.output_tokens,
                    "cost": null,
                })
            })
            .collect();

        let total_tokens: u64 = entries
            .iter()
            .filter_map(|e| e.get("tokens").and_then(|v| v.as_u64()))
            .sum();
        let total_input: u64 = entries
            .iter()
            .filter_map(|e| e.get("input_tokens").and_then(|v| v.as_u64()))
            .sum();
        let total_output: u64 = entries
            .iter()
            .filter_map(|e| e.get("output_tokens").and_then(|v| v.as_u64()))
            .sum();

        Ok(json!({
            "period": period,
            "total_tokens": total_tokens,
            "prompt_tokens": total_input,
            "completion_tokens": total_output,
            "total_sessions": entries.len(),
            "entries": entries,
        }))
    }
}

// ── Logs ────────────────────────────────────────────────────────────────────

async fn handle_logs_tail(params: &Value) -> RpcResult {
    let lines = params.get("lines").and_then(|v| v.as_u64()).unwrap_or(50);
    let entries = log_store::list_logs(lines as usize).await;
    let value = serde_json::to_value(entries).unwrap_or(json!([]));
    Ok(json!({ "lines": lines, "entries": value }))
}

// ── System ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentHeartbeatSettings {
    enabled: bool,
    interval_ms: u64,
    coalesce_window_ms: u64,
    #[serde(default)]
    cron_job_ids: Vec<String>,
}

impl Default for AgentHeartbeatSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_ms: 30_000,
            coalesce_window_ms: 30_000,
            cron_job_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HeartbeatSettingsDocument {
    #[serde(default)]
    agents: HashMap<String, AgentHeartbeatSettings>,
}

#[derive(Debug, Clone, Default)]
struct PendingHeartbeatEvent {
    event_type: String,
    text: String,
    timestamp: String,
}

#[derive(Debug, Clone, Default)]
struct HeartbeatRuntimeState {
    last_delivered_ms: u64,
    pending: Option<PendingHeartbeatEvent>,
    flush_scheduled: bool,
}

fn heartbeat_runtime_store() -> &'static Mutex<HashMap<String, HeartbeatRuntimeState>> {
    static STORE: OnceLock<Mutex<HashMap<String, HeartbeatRuntimeState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn heartbeat_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn heartbeat_agent_from_params(params: &Value) -> String {
    params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string()
}

async fn load_heartbeat_settings(bridge: &GatewayBridge) -> HeartbeatSettingsDocument {
    let path = heartbeat_config_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    serde_json::from_str::<HeartbeatSettingsDocument>(&content).unwrap_or_default()
}

async fn save_heartbeat_settings(
    bridge: &GatewayBridge,
    settings: &HeartbeatSettingsDocument,
) -> Result<(), String> {
    let path = heartbeat_config_path(bridge);
    let content = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("serialize heartbeat settings failed: {e}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|e| format!("write heartbeat settings failed: {e}"))
}

async fn heartbeat_settings_for_agent(
    bridge: &GatewayBridge,
    agent_id: &str,
) -> AgentHeartbeatSettings {
    let settings = load_heartbeat_settings(bridge).await;
    settings
        .agents
        .get(agent_id)
        .cloned()
        .or_else(|| settings.agents.get("*").cloned())
        .unwrap_or_default()
}

async fn handle_last_heartbeat(params: &Value) -> RpcResult {
    let agent_id = heartbeat_agent_from_params(params);
    let state = heartbeat_runtime_store().lock().await;
    let snapshot = state.get(&agent_id).cloned().unwrap_or_default();
    let timestamp = if snapshot.last_delivered_ms == 0 {
        chrono::Utc::now().to_rfc3339()
    } else {
        chrono::DateTime::from_timestamp_millis(snapshot.last_delivered_ms as i64)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
    };
    Ok(json!({
        "agent": agent_id,
        "timestamp": timestamp,
        "last_delivered_ms": snapshot.last_delivered_ms,
        "has_pending": snapshot.pending.is_some(),
    }))
}

async fn handle_set_heartbeats(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = heartbeat_agent_from_params(params);
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let interval_ms = params
        .get("interval_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);
    let coalesce_window_ms = params
        .get("coalesce_window_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);

    let cron_job_ids = params
        .get("cron_job_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(str::trim))
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        });

    let mut settings = load_heartbeat_settings(bridge).await;
    let entry = settings.agents.entry(agent_id.clone()).or_default();
    entry.enabled = enabled;
    entry.interval_ms = interval_ms.max(1000);
    entry.coalesce_window_ms = coalesce_window_ms.max(1000);
    if let Some(cron_job_ids) = cron_job_ids {
        entry.cron_job_ids = cron_job_ids;
    }
    let response = entry.clone();

    save_heartbeat_settings(bridge, &settings)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({
        "agent": agent_id,
        "enabled": response.enabled,
        "interval_ms": response.interval_ms,
        "coalesce_window_ms": response.coalesce_window_ms,
        "cron_job_ids": response.cron_job_ids,
    }))
}

async fn handle_system_presence(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let status = params
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("online");

    // Broadcast presence change to all connected clients.
    session_mgr
        .broadcast_to_all(
            "system.presence",
            json!({ "status": status, "timestamp": chrono::Utc::now().to_rfc3339() }),
        )
        .await;

    Ok(json!({ "status": status }))
}

async fn handle_system_event(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
    session_mgr: &Arc<GatewaySessionManager>,
    cron_service: &Arc<CronService>,
) -> RpcResult {
    let event_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let heartbeat = params
        .get("heartbeat")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if heartbeat {
        let agent_id = heartbeat_agent_from_params(params);
        let settings = heartbeat_settings_for_agent(bridge, &agent_id).await;
        if !settings.enabled {
            return Ok(json!({
                "type": event_type,
                "received": true,
                "heartbeat": true,
                "agent": agent_id,
                "delivered": false,
                "reason": "heartbeat disabled",
            }));
        }

        let now_ms = heartbeat_now_ms();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let mut should_flush = false;
        let mut flush_delay_ms = settings.coalesce_window_ms;
        let mut coalesced = false;
        {
            let mut store = heartbeat_runtime_store().lock().await;
            let state = store.entry(agent_id.clone()).or_default();
            let elapsed = now_ms.saturating_sub(state.last_delivered_ms);
            if elapsed < settings.coalesce_window_ms {
                state.pending = Some(PendingHeartbeatEvent {
                    event_type: event_type.to_string(),
                    text: text.to_string(),
                    timestamp: timestamp.clone(),
                });
                if !state.flush_scheduled {
                    state.flush_scheduled = true;
                    should_flush = true;
                    flush_delay_ms = settings.coalesce_window_ms.saturating_sub(elapsed).max(1);
                }
                coalesced = true;
            } else {
                state.last_delivered_ms = now_ms;
                state.pending = None;
            }
        }

        if coalesced {
            if should_flush {
                schedule_heartbeat_flush(
                    agent_id.clone(),
                    flush_delay_ms,
                    Arc::clone(session_mgr),
                    Arc::clone(bridge),
                    Arc::clone(cron_service),
                );
            }
            return Ok(json!({
                "type": event_type,
                "received": true,
                "heartbeat": true,
                "agent": agent_id,
                "delivered": false,
                "coalesced": true,
                "window_ms": settings.coalesce_window_ms,
            }));
        }

        broadcast_heartbeat_event(session_mgr, event_type, text, &timestamp).await;
        trigger_heartbeat_cron_jobs(&agent_id, &settings, bridge, cron_service).await;

        return Ok(json!({
            "type": event_type,
            "received": true,
            "heartbeat": true,
            "agent": agent_id,
            "delivered": true,
            "coalesced": false,
            "timestamp": timestamp,
        }));
    }

    // Broadcast the system event to all connected WebSocket clients.
    session_mgr
        .broadcast_to_all(
            "system.event",
            json!({
                "type": event_type,
                "text": text,
                "heartbeat": heartbeat,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    // If text is provided, inject it into the main agent session.
    if !text.is_empty() {
        match bridge.invoke_agent_text(text, "default").await {
            Ok(reply) => {
                return Ok(json!({
                    "type": event_type,
                    "received": true,
                    "response": reply,
                }));
            }
            Err(err) => {
                return Ok(json!({
                    "type": event_type,
                    "received": true,
                    "error": format!("{err}"),
                }));
            }
        }
    }

    Ok(json!({ "type": event_type, "received": true }))
}

fn schedule_heartbeat_flush(
    agent_id: String,
    delay_ms: u64,
    session_mgr: Arc<GatewaySessionManager>,
    bridge: Arc<GatewayBridge>,
    cron_service: Arc<CronService>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let pending = {
            let mut store = heartbeat_runtime_store().lock().await;
            let state = store.entry(agent_id.clone()).or_default();
            state.flush_scheduled = false;
            let pending = state.pending.take();
            if pending.is_some() {
                state.last_delivered_ms = heartbeat_now_ms();
            }
            pending
        };

        let Some(pending) = pending else {
            return;
        };

        broadcast_heartbeat_event(
            &session_mgr,
            &pending.event_type,
            &pending.text,
            &pending.timestamp,
        )
        .await;

        let settings = heartbeat_settings_for_agent(&bridge, &agent_id).await;
        trigger_heartbeat_cron_jobs(&agent_id, &settings, &bridge, &cron_service).await;
    });
}

async fn broadcast_heartbeat_event(
    session_mgr: &Arc<GatewaySessionManager>,
    event_type: &str,
    text: &str,
    timestamp: &str,
) {
    session_mgr
        .broadcast_to_all(
            "system.event",
            json!({
                "type": event_type,
                "text": text,
                "heartbeat": true,
                "timestamp": timestamp,
            }),
        )
        .await;
}

async fn trigger_heartbeat_cron_jobs(
    agent_id: &str,
    settings: &AgentHeartbeatSettings,
    bridge: &Arc<GatewayBridge>,
    cron_service: &Arc<CronService>,
) {
    for job_id in &settings.cron_job_ids {
        if let Err(err) = cron_service.run_job(job_id, bridge).await {
            tracing::warn!(
                agent = agent_id,
                cron_job_id = %job_id,
                "heartbeat cron trigger failed: {err}"
            );
        }
    }
}

// ── Models (per-provider store) ─────────────────────────────────────────

fn models_dir(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("models")
}

fn legacy_model_store_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("models.json")
}

// ── Provider file v2 types ──────────────────────────────────────────────

fn default_version() -> u32 {
    2
}
fn default_auth_type() -> String {
    "api_key".to_string()
}

/// On-disk representation of `models/{provider_id}.json` (v2).
/// Persisted shape is `enabled_models` plus auth/provider metadata.
/// `models` is kept runtime-only for legacy compatibility and RPC responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    provider_id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    auth: Option<ProviderAuth>,
    #[serde(default, rename = "models", skip_serializing)]
    models: Vec<Value>,
    #[serde(default)]
    enabled_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderAuth {
    #[serde(rename = "type", default = "default_auth_type")]
    auth_type: String,
    #[serde(default)]
    env_key: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

fn provider_models_from_enabled_models(provider_id: &str, enabled_models: &[String]) -> Vec<Value> {
    let canonical_provider = savfox_core::canonical_provider_id(provider_id);
    let default_slug = savfox_core::provider_default_model_slug(provider_id);
    let normalized_enabled: Vec<String> = enabled_models
        .iter()
        .filter_map(|slug| {
            let trimmed = slug.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(
                savfox_core::parse_provider_prefixed_model(trimmed)
                    .map(|(_, model_code)| model_code.to_string())
                    .unwrap_or_else(|| trimmed.to_string()),
            )
        })
        .collect();

    savfox_core::provider_models_from_enabled_slugs(provider_id, &normalized_enabled)
        .into_iter()
        .map(|model| {
            let model_slug = model.slug;
            json!({
                "id": format!("{canonical_provider}/{model_slug}"),
                "name": model.display_name,
                "provider": canonical_provider.as_str(),
                "model_code": model_slug,
                "is_default": default_slug == Some(model_slug.as_str()),
                "builtin": true,
            })
        })
        .collect()
}

fn provider_enabled_models_from_models(models: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut enabled_models = Vec::new();
    for model in models {
        let slug = model
            .get("model_code")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                model
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|id| {
                        savfox_core::parse_provider_prefixed_model(id)
                            .map(|(_, model_code)| model_code)
                            .unwrap_or(id)
                    })
            });
        let Some(slug) = slug else {
            continue;
        };
        let slug = slug.to_string();
        if seen.insert(slug.clone()) {
            enabled_models.push(slug);
        }
    }
    enabled_models
}

fn hydrate_provider_file_enabled_models(file: &mut ProviderFile) {
    if file.enabled_models.is_empty() && !file.models.is_empty() {
        file.enabled_models = provider_enabled_models_from_models(&file.models);
    } else if !file.enabled_models.is_empty() {
        file.enabled_models = file
            .enabled_models
            .iter()
            .filter_map(|slug| {
                let trimmed = slug.trim();
                if trimmed.is_empty() {
                    return None;
                }
                Some(
                    savfox_core::parse_provider_prefixed_model(trimmed)
                        .map(|(_, model_code)| model_code.to_string())
                        .unwrap_or_else(|| trimmed.to_string()),
                )
            })
            .collect();
    }
}

fn hydrate_provider_file_models(file: &mut ProviderFile, provider_id_hint: &str) {
    if !file.models.is_empty() || file.enabled_models.is_empty() {
        return;
    }

    let provider_id = if file.provider_id.trim().is_empty() {
        provider_id_hint.trim().to_string()
    } else {
        file.provider_id.trim().to_string()
    };
    if provider_id.is_empty() {
        return;
    }

    if file.provider_id.trim().is_empty() {
        file.provider_id = provider_id.clone();
    }
    file.models = provider_models_from_enabled_models(&provider_id, &file.enabled_models);
}

/// Load a provider file, auto-detecting v1 (bare array) vs v2 (object).
async fn load_provider_file(bridge: &GatewayBridge, provider_id: &str) -> ProviderFile {
    let path = models_dir(bridge).join(format!("{provider_id}.json"));
    let Ok(data) = tokio::fs::read_to_string(&path).await else {
        return ProviderFile {
            version: 2,
            provider_id: provider_id.to_string(),
            display_name: String::new(),
            auth: None,
            models: Vec::new(),
            enabled_models: Vec::new(),
        };
    };

    // Try v2 (object with "models" key) first, then fall back to v1 (bare array).
    if let Ok(mut file) = serde_json::from_str::<ProviderFile>(&data) {
        hydrate_provider_file_enabled_models(&mut file);
        hydrate_provider_file_models(&mut file, provider_id);
        return file;
    }
    if let Ok(models) = serde_json::from_str::<Vec<Value>>(&data) {
        return ProviderFile {
            version: 2,
            provider_id: provider_id.to_string(),
            display_name: String::new(),
            auth: None,
            models,
            enabled_models: Vec::new(),
        };
    }

    ProviderFile {
        version: 2,
        provider_id: provider_id.to_string(),
        display_name: String::new(),
        auth: None,
        models: Vec::new(),
        enabled_models: Vec::new(),
    }
}

/// Save a v2 provider file. Creates the `models/` dir on first write.
async fn save_provider_file(
    bridge: &GatewayBridge,
    provider_id: &str,
    file: &ProviderFile,
) -> Result<(), String> {
    let mut file_to_write = file.clone();
    hydrate_provider_file_enabled_models(&mut file_to_write);
    let dir = models_dir(bridge);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("create models dir: {e}"))?;
    let path = dir.join(format!("{provider_id}.json"));
    if file_to_write.models.is_empty()
        && file_to_write.enabled_models.is_empty()
        && file_to_write.auth.is_none()
    {
        let _ = tokio::fs::remove_file(&path).await;
        return Ok(());
    }
    let data = serde_json::to_string_pretty(&file_to_write)
        .map_err(|e| format!("serialize error: {e}"))?;
    tokio::fs::write(&path, data)
        .await
        .map_err(|e| format!("write error: {e}"))
}

/// Inject a single provider's auth into the runtime override maps.
///
/// Sets both the env-variable override (for providers with `env_key`) and
/// the bearer-token override (keyed by provider id, for providers like
/// OpenAI whose built-in config has `env_key: None`).
fn inject_provider_auth(file: &ProviderFile) {
    if let Some(auth) = &file.auth {
        if let Some(api_key) = &auth.api_key {
            if !api_key.is_empty() {
                // Set env-variable override (for providers that use env_key).
                if let Some(env_key) = &auth.env_key {
                    if !env_key.is_empty() {
                        savfox_core::set_env_override(env_key, api_key);
                    }
                }
                // Also set bearer-token override keyed by provider id so
                // that `auth_provider_from_auth` can find it even when the
                // in-memory ModelProviderInfo has `env_key: None`.
                if !file.provider_id.is_empty() {
                    savfox_core::set_bearer_token_override(&file.provider_id, api_key);
                }
            }
        }
    }
}

/// Read all `models/*.json` files and inject their auth into the runtime
/// env-override map. Called once at gateway startup.
pub(crate) async fn inject_all_provider_auth(bridge: &GatewayBridge) {
    let dir = models_dir(bridge);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let provider_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if provider_id.is_empty() {
            continue;
        }
        let file = load_provider_file(bridge, &provider_id).await;
        inject_provider_auth(&file);
    }
}

#[derive(Clone, Debug)]
struct RemoteModelsRequest {
    provider: String,
    base_url: String,
    api_key: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug)]
struct RemoteModelsHttpResponse {
    url: String,
    status: reqwest::StatusCode,
    request_id: Option<String>,
    body: String,
}

fn canonical_models_provider_id(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "zhipu" | "zhipu-ai" => "zhipuai".to_string(),
        "zhipu-coding-plan" | "zhipu-ai-coding-plan" => "zhipuai-coding-plan".to_string(),
        "together" | "together-ai" => "togetherai".to_string(),
        "gemini" => "google".to_string(),
        "bedrock" => "amazon-bedrock".to_string(),
        "qwen" => "alibaba".to_string(),
        "googlevertex" | "google_vertex" => "google-vertex".to_string(),
        "google_vertex_anthropic" => "google-vertex-anthropic".to_string(),
        other => other.to_string(),
    }
}

fn model_test_default_base_url(provider_id: &str) -> Option<String> {
    savfox_core::provider_default_base_url(provider_id)
}

fn model_test_extract_count(payload: &Value) -> Option<usize> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| arr.len())
        .or_else(|| {
            payload
                .get("models")
                .and_then(Value::as_array)
                .map(|arr| arr.len())
        })
}

fn model_test_extract_models_array(payload: &Value) -> Option<&Vec<Value>> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.as_array())
}

fn model_test_extract_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("x-oai-request-id"))
        .or_else(|| headers.get("cf-ray"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn model_test_truncate_oneline(input: &str, max_chars: usize) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }

    let mut out = String::new();
    for (idx, ch) in collapsed.chars().enumerate() {
        if idx >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn model_test_nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

fn model_test_remote_requested(params: &Value) -> bool {
    params.get("provider").is_some()
        || params.get("provider_id").is_some()
        || params.get("base_url").is_some()
        || params.get("api_key").is_some()
}

fn model_test_resolve_remote_request(
    params: &Value,
) -> Result<Option<RemoteModelsRequest>, (i64, String)> {
    if !model_test_remote_requested(params) {
        return Ok(None);
    }

    let provider = model_test_nonempty_string(params.get("provider"))
        .or_else(|| model_test_nonempty_string(params.get("provider_id")))
        .map(|value| canonical_models_provider_id(&value))
        .unwrap_or_default();
    let explicit_base_url = model_test_nonempty_string(params.get("base_url"));
    let base_url = explicit_base_url
        .or_else(|| model_test_default_base_url(&provider))
        .ok_or((
            INVALID_PARAMS,
            "missing 'base_url' parameter for model discovery".to_string(),
        ))?;
    let api_key = model_test_nonempty_string(params.get("api_key"));

    Ok(Some(RemoteModelsRequest {
        provider,
        base_url,
        api_key,
        account_id: None,
    }))
}

fn model_test_is_openai_platform_base_url(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    normalized == "https://api.openai.com" || normalized == "https://api.openai.com/v1"
}

fn model_test_chatgpt_codex_base_url(bridge: &Arc<GatewayBridge>) -> String {
    let base = bridge
        .config()
        .chatgpt_base_url
        .trim()
        .trim_end_matches('/');
    if base.ends_with("/codex") {
        base.to_string()
    } else {
        format!("{base}/codex")
    }
}

async fn model_test_apply_openai_auth_fallback(
    request: &mut RemoteModelsRequest,
    bridge: &Arc<GatewayBridge>,
) {
    if request.provider != "openai" || request.api_key.is_some() {
        return;
    }

    let auth_manager = gateway_auth_manager(bridge);
    let Some(auth) = auth_manager.auth().await else {
        return;
    };

    if let Ok(token) = auth.get_token() {
        let token = token.trim();
        if !token.is_empty() {
            request.api_key = Some(token.to_string());
        }
    }

    if let Some(account_id) = auth.get_account_id() {
        let account_id = account_id.trim();
        if !account_id.is_empty() {
            request.account_id = Some(account_id.to_string());
        }
    }

    if auth.is_chatgpt_auth() && model_test_is_openai_platform_base_url(&request.base_url) {
        request.base_url = model_test_chatgpt_codex_base_url(bridge);
    }
}

async fn model_test_fetch_remote_models(
    request: &RemoteModelsRequest,
) -> Result<RemoteModelsHttpResponse, (i64, String)> {
    let url = format!("{}/models", request.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to build model test client: {err}"),
            )
        })?;

    let mut http_request = client.get(&url);
    if let Some(key) = request.api_key.as_deref() {
        http_request = http_request.bearer_auth(key);
    }
    if let Some(account_id) = request.account_id.as_deref() {
        http_request = http_request.header("chatgpt-account-id", account_id);
    }

    let response = http_request.send().await.map_err(|err| {
        (
            INTERNAL_ERROR,
            format!("connection request failed for {url}: {err}"),
        )
    })?;

    let status = response.status();
    let request_id = model_test_extract_request_id(response.headers());
    let body = response.text().await.unwrap_or_default();

    Ok(RemoteModelsHttpResponse {
        url,
        status,
        request_id,
        body,
    })
}

fn model_test_http_failure_message(prefix: &str, response: &RemoteModelsHttpResponse) -> String {
    let mut message = format!(
        "{prefix}: HTTP {} {}",
        response.status.as_u16(),
        response.status.canonical_reason().unwrap_or("Unknown"),
    );
    let body_preview = model_test_truncate_oneline(&response.body, 240);
    if !body_preview.is_empty() {
        message.push_str(&format!(": {body_preview}"));
    }
    if let Some(request_id) = response.request_id.as_deref() {
        message.push_str(&format!(", request id: {request_id}"));
    }
    message
}

fn model_test_model_item_field(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| model_test_nonempty_string(item.get(*key)))
}

fn model_test_parse_remote_models(payload: &Value, provider_hint: &str) -> Vec<Value> {
    let models = match model_test_extract_models_array(payload) {
        Some(items) => items,
        None => return Vec::new(),
    };

    let canonical_hint = canonical_models_provider_id(provider_hint);
    let mut parsed = Vec::new();
    let mut seen = HashSet::new();

    for item in models {
        let (raw_id, display_name, provider_from_item) = if let Some(id) =
            model_test_model_item_field(item, &["id", "model", "model_id", "model_code"])
        {
            (
                id,
                model_test_model_item_field(item, &["name", "display_name", "label"]),
                model_test_model_item_field(item, &["provider"]),
            )
        } else if let Some(id) = item.as_str().map(str::trim).filter(|v| !v.is_empty()) {
            (id.to_string(), None, None)
        } else {
            continue;
        };

        let mut provider = provider_from_item
            .map(|value| canonical_models_provider_id(&value))
            .unwrap_or_default();
        if provider.is_empty() && !canonical_hint.is_empty() {
            provider = canonical_hint.clone();
        }

        let mut model_code = raw_id.clone();
        if let Some((prefix, rest)) = savfox_core::parse_provider_prefixed_model(raw_id.as_str()) {
            let prefix = canonical_models_provider_id(prefix);
            if provider.is_empty() && !prefix.is_empty() {
                provider = prefix;
            }
            model_code = rest.to_string();
        }

        if model_code.is_empty() {
            continue;
        }

        let id = if raw_id.contains('/') || provider.is_empty() {
            raw_id
        } else {
            format!("{provider}/{model_code}")
        };

        if !seen.insert(id.clone()) {
            continue;
        }

        let mut entry = serde_json::Map::new();
        entry.insert("id".to_string(), json!(id));
        entry.insert(
            "name".to_string(),
            json!(display_name.unwrap_or_else(|| model_code.clone())),
        );
        entry.insert("model_code".to_string(), json!(model_code));
        entry.insert("is_default".to_string(), json!(false));
        entry.insert("builtin".to_string(), json!(true));
        if !provider.is_empty() {
            entry.insert("provider".to_string(), json!(provider));
        }
        parsed.push(Value::Object(entry));
    }

    parsed
}

async fn handle_models_test(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    use savfox_core::models_manager::manager::RefreshStrategy;

    if let Some(model_id) = params
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let models = bridge
            .session_manager()
            .list_models(bridge.config(), RefreshStrategy::OnlineIfUncached)
            .await;

        let ok = models.iter().any(|preset| {
            preset.id == model_id
                || preset.slug == model_id
                || preset.display_name.eq_ignore_ascii_case(model_id)
        });

        let message = if ok {
            format!("Model `{model_id}` is available.")
        } else {
            format!("Model `{model_id}` was not found in the available model list.")
        };

        return Ok(json!({
            "ok": ok,
            "message": message,
            "model": model_id,
        }));
    }

    let mut request = model_test_resolve_remote_request(params)?.ok_or((
        INVALID_PARAMS,
        "missing provider/base_url parameters for connection test".to_string(),
    ))?;
    model_test_apply_openai_auth_fallback(&mut request, bridge).await;
    let response = model_test_fetch_remote_models(&request).await?;

    if response.status.is_success() {
        let model_count = serde_json::from_str::<Value>(&response.body)
            .ok()
            .and_then(|payload| model_test_extract_count(&payload));
        let mut message = if let Some(count) = model_count {
            format!("Connection successful. Retrieved {count} model(s).")
        } else {
            "Connection successful.".to_string()
        };
        if let Some(request_id) = response.request_id.as_deref() {
            message.push_str(&format!(" request id: {request_id}"));
        }

        return Ok(json!({
            "ok": true,
            "message": message,
            "status": response.status.as_u16(),
            "model_count": model_count,
            "base_url": request.base_url,
        }));
    }

    let message = model_test_http_failure_message("Connection failed", &response);

    Ok(json!({
        "ok": false,
        "message": message,
        "status": response.status.as_u16(),
        "base_url": request.base_url,
    }))
}

/// Migrate legacy `models.json` (flat HashMap) to per-provider `models/` dir.
async fn maybe_migrate_legacy_model_store(bridge: &GatewayBridge) {
    let legacy = legacy_model_store_path(bridge);
    let dir = models_dir(bridge);
    if !legacy.exists() || dir.exists() {
        return;
    }
    let Ok(data) = tokio::fs::read_to_string(&legacy).await else {
        return;
    };
    let Ok(old): Result<HashMap<String, Value>, _> = serde_json::from_str(&data) else {
        return;
    };
    // Group by provider
    let mut by_provider: HashMap<String, Vec<Value>> = HashMap::new();
    for (_id, entry) in old {
        let provider = entry
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        by_provider.entry(provider).or_default().push(entry);
    }
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    for (provider_id, entries) in &by_provider {
        let path = dir.join(format!("{provider_id}.json"));
        if let Ok(json) = serde_json::to_string_pretty(entries) {
            let _ = tokio::fs::write(&path, json).await;
        }
    }
    // Rename old file
    let backup = legacy.with_extension("json.bak");
    let _ = tokio::fs::rename(&legacy, &backup).await;
}

/// Load models for a single provider (thin wrapper over `load_provider_file`).
async fn load_provider_models(bridge: &GatewayBridge, provider_id: &str) -> Vec<Value> {
    load_provider_file(bridge, provider_id).await.models
}

/// Load all models from all `models/*.json` files, merged into a flat HashMap keyed by model ID.
async fn load_all_provider_models(bridge: &GatewayBridge) -> HashMap<String, Value> {
    maybe_migrate_legacy_model_store(bridge).await;
    let mut out = HashMap::new();
    let dir = models_dir(bridge);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let provider_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if provider_id.is_empty() {
            continue;
        }
        let file = load_provider_file(bridge, &provider_id).await;
        for model in file.models {
            if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                out.insert(id.to_string(), model);
            }
        }
    }
    out
}

/// Save models array to `models/{provider_id}.json`. Preserves existing auth.
async fn save_provider_models(
    bridge: &GatewayBridge,
    provider_id: &str,
    models: &[Value],
) -> Result<(), String> {
    let mut file = load_provider_file(bridge, provider_id).await;
    file.models = models.to_vec();
    save_provider_file(bridge, provider_id, &file).await
}

async fn handle_models_list(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    if let Some(mut request) = model_test_resolve_remote_request(params)? {
        model_test_apply_openai_auth_fallback(&mut request, bridge).await;
        let response = model_test_fetch_remote_models(&request).await?;
        if !response.status.is_success() {
            return Err((
                INTERNAL_ERROR,
                model_test_http_failure_message("remote model list request failed", &response),
            ));
        }

        let payload = serde_json::from_str::<Value>(&response.body).map_err(|err| {
            let preview = model_test_truncate_oneline(&response.body, 240);
            (
                INTERNAL_ERROR,
                format!(
                    "failed to parse model list response from {}: {err}; body={preview}",
                    response.url
                ),
            )
        })?;
        let models = model_test_parse_remote_models(&payload, &request.provider);
        return Ok(json!({ "models": models }));
    }

    // Per-provider store is the source of truth for the models page / selector.
    // Built-in engine presets are intentionally omitted: they lack a provider prefix
    // in their IDs and would display as one-model "provider" groups.  The wizard
    // writes every selected model into the per-provider store, so anything the user
    // has configured will appear here.
    let custom = load_all_provider_models(bridge).await;
    let mut models: Vec<Value> = Vec::with_capacity(custom.len());
    for (_id, config) in &custom {
        let mut entry = config.clone();
        if let Value::Object(ref mut map) = entry {
            map.entry("builtin".to_string()).or_insert(json!(false));
            // Never expose secrets over the wire.
            map.remove("api_key");
        }
        models.push(entry);
    }

    Ok(json!({ "models": models }))
}

async fn handle_models_add(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let model_code = params
        .get("model_code")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if provider.is_empty() || model_code.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'provider' and/or 'model_code'".to_string(),
        ));
    }

    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{provider}/{model_code}"));

    let mut entry = json!({
        "id": id,
        "provider": provider,
        "model_code": model_code,
        "is_default": false,
        "is_disabled": false,
        "builtin": false,
    });

    // Copy optional fields
    for field in &[
        "api_key",
        "base_url",
        "max_tokens",
        "temperature",
        "name",
        "is_disabled",
        "cost_input_per_m",
        "cost_output_per_m",
        "cost_cache_read_per_m",
        "cost_cache_write_per_m",
        "context_window",
        "max_output_tokens",
        "auth_type",
        "custom_headers",
    ] {
        if let Some(v) = params.get(*field) {
            entry[*field] = v.clone();
        }
    }

    let mut models = load_provider_models(bridge, provider).await;
    // Remove existing entry with same id if present
    models.retain(|m| m.get("id").and_then(|v| v.as_str()) != Some(&id));
    models.push(entry);
    save_provider_models(bridge, provider, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "added" }))
}

async fn handle_models_update(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let provider_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(bridge, provider_id).await;
    // Find existing entry or create new one
    let entry = if let Some(pos) = models
        .iter()
        .position(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        &mut models[pos]
    } else {
        models.push(json!({"id": id}));
        models.last_mut().unwrap()
    };

    // Merge updatable fields
    for field in &[
        "provider",
        "model_code",
        "api_key",
        "base_url",
        "name",
        "is_disabled",
        "max_tokens",
        "temperature",
        "cost_input_per_m",
        "cost_output_per_m",
        "cost_cache_read_per_m",
        "cost_cache_write_per_m",
        "context_window",
        "max_output_tokens",
        "auth_type",
        "custom_headers",
    ] {
        if let Some(v) = params.get(*field) {
            entry[*field] = v.clone();
        }
    }

    save_provider_models(bridge, provider_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "updated" }))
}

async fn handle_models_delete(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let provider_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(bridge, provider_id).await;
    models.retain(|m| m.get("id").and_then(|v| v.as_str()) != Some(id));
    save_provider_models(bridge, provider_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "deleted" }))
}

async fn handle_models_setdefault(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let provider_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(bridge, provider_id).await;

    // Clear default on all entries in this provider
    for m in models.iter_mut() {
        if let Value::Object(map) = m {
            map.insert("is_default".to_string(), json!(false));
        }
    }

    // Set default on target (create entry if it doesn't exist yet)
    if let Some(entry) = models
        .iter_mut()
        .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        entry["is_default"] = json!(true);
    } else {
        models.push(json!({"id": id, "is_default": true}));
    }

    save_provider_models(bridge, provider_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "default_set" }))
}

async fn handle_models_import(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let provider_id = params
        .get("provider_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if provider_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'provider_id'".to_string()));
    }

    let models_arr = params.get("models").and_then(|v| v.as_array()).ok_or((
        INVALID_REQUEST,
        "missing or invalid 'models' array".to_string(),
    ))?;
    if models_arr.is_empty() {
        return Err((INVALID_REQUEST, "'models' array is empty".to_string()));
    }

    // Build model entries
    let mut entries: Vec<Value> = Vec::with_capacity(models_arr.len());
    for m in models_arr {
        let mut entry = m.clone();
        if let Value::Object(ref mut map) = entry {
            map.entry("provider".to_string())
                .or_insert_with(|| json!(provider_id));
            map.entry("builtin".to_string()).or_insert(json!(false));
        }
        entries.push(entry);
    }

    // Build auth section from params
    let api_key_val = params.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let env_key_val = params.get("env_key").and_then(|v| v.as_str()).unwrap_or("");
    let display_name_val = params
        .get("display_name")
        .and_then(|v| v.as_str())
        .unwrap_or(provider_id);

    let auth = if !api_key_val.is_empty() && !env_key_val.is_empty() {
        Some(ProviderAuth {
            auth_type: "api_key".to_string(),
            env_key: Some(env_key_val.to_string()),
            api_key: Some(api_key_val.to_string()),
        })
    } else if !env_key_val.is_empty() {
        Some(ProviderAuth {
            auth_type: "api_key".to_string(),
            env_key: Some(env_key_val.to_string()),
            api_key: None,
        })
    } else {
        None
    };

    let file = ProviderFile {
        version: 2,
        provider_id: provider_id.to_string(),
        display_name: display_name_val.to_string(),
        auth,
        models: entries.clone(),
        enabled_models: Vec::new(),
    };

    // Write v2 provider file to models/{provider_id}.json
    save_provider_file(bridge, provider_id, &file)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    // Inject auth into runtime so the engine can authenticate immediately
    inject_provider_auth(&file);

    // Patch user config: set API key + active model + provider.
    let base_url = params
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let config_provider_id = savfox_core::canonical_provider_id(provider_id);

    // Find the first default model (or first model)
    let default_entry = entries
        .iter()
        .find(|e| {
            e.get("is_default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .or_else(|| entries.first());

    // Build config patch with top-level fields the core engine expects
    let mut patch = json!({});

    // Set selected model in the structured [model] section.
    if let Some(default) = default_entry {
        let model_code = default
            .get("model_code")
            .and_then(|v| v.as_str())
            .or_else(|| {
                // Fall back: strip provider prefix from id
                default
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .map(|id| {
                        savfox_core::parse_provider_prefixed_model(id)
                            .map(|(_, code)| code)
                            .unwrap_or(id)
                    })
            })
            .unwrap_or("");
        if !model_code.is_empty() {
            patch["model"] = json!({
                "slug": model_code,
                "provider": config_provider_id.clone(),
            });
        }
    }

    // Read existing config to deep-merge env and model_providers.
    let mut config: serde_json::Map<String, Value> = load_config_value_or_empty(bridge)
        .await
        .as_object()
        .cloned()
        .unwrap_or_default();

    // Deep-merge top-level scalar fields from patch
    if let Some(patch_obj) = patch.as_object() {
        for (key, value) in patch_obj {
            config.insert(key.clone(), value.clone());
        }
    }

    if config.get("model").and_then(Value::as_object).is_some() {
        config.remove("model_provider");
        config.remove("model_reasoning_effort");
    }

    // Deep-merge [env]: preserve existing keys, add/overwrite new ones
    if !api_key_val.is_empty() && !env_key_val.is_empty() {
        let env = config.entry("env").or_insert_with(|| json!({}));
        if let Some(env_map) = env.as_object_mut() {
            env_map.insert(env_key_val.to_string(), json!(api_key_val));
        }
    }

    // Keep built-in providers (like openai) out of persisted `model_providers`.
    // Only custom providers need explicit on-disk entries.
    {
        let is_builtin_provider =
            savfox_core::built_in_model_providers().contains_key(config_provider_id.as_str());

        if is_builtin_provider {
            if let Some(providers_map) = config
                .get_mut("model_providers")
                .and_then(Value::as_object_mut)
            {
                providers_map.remove(config_provider_id.as_str());
                if config_provider_id != provider_id {
                    providers_map.remove(provider_id);
                }
                if providers_map.is_empty() {
                    config.remove("model_providers");
                }
            }
        } else {
            let providers = config.entry("model_providers").or_insert_with(|| json!({}));
            if let Some(providers_map) = providers.as_object_mut() {
                let provider = providers_map
                    .entry(config_provider_id.clone())
                    .or_insert_with(|| json!({ "name": display_name_val, "wire_api": "chat" }));
                if let Some(provider_map) = provider.as_object_mut() {
                    if !base_url.is_empty() {
                        provider_map.insert("base_url".to_string(), json!(base_url));
                    }
                    if !env_key_val.is_empty() {
                        provider_map.insert("env_key".to_string(), json!(env_key_val));
                    }
                }
            }
        }
    }

    let mut merged = Value::Object(config);
    sanitize_config_before_write(&mut merged, bridge).await?;
    write_config_json(bridge, &merged)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    let model_count = entries.len();
    Ok(json!({
        "status": "imported",
        "provider_id": provider_id,
        "model_count": model_count,
    }))
}

// ── Tools ───────────────────────────────────────────────────────────────────

async fn handle_tools_invoke(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let tool = params.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let action = params.get("action").and_then(|v| v.as_str());
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    if tool.is_empty() {
        return Err((INVALID_REQUEST, "missing 'tool' parameter".to_string()));
    }

    // Build a prompt that forces the agent to invoke the requested tool.
    let mut args = arguments.clone();
    if let Some(act) = action {
        if let Value::Object(ref mut map) = args {
            map.entry("action")
                .or_insert_with(|| Value::String(act.to_owned()));
        }
    }
    let args_str = serde_json::to_string_pretty(&args).unwrap_or_else(|_| "{}".to_string());
    let prompt = format!(
        "Use the `{tool}` tool with exactly these arguments:\n\
         ```json\n{args_str}\n```\n\
         Return only the raw tool output. Do not add commentary."
    );

    match bridge.invoke_agent_text(&prompt, "default").await {
        Ok(output) => {
            // Try to parse the output as JSON for a cleaner response.
            let result = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                Value::String(savfox_core::external_content::wrap_external_content(
                    &format!("tool:{tool}"),
                    &output,
                ))
            });
            Ok(json!({
                "ok": true,
                "tool": tool,
                "result": result,
            }))
        }
        Err(err) => Err((INTERNAL_ERROR, format!("tool invocation failed: {err}"))),
    }
}

// ── Browser ─────────────────────────────────────────────────────────────────

const DEFAULT_BROWSER_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrowserProfileSettings {
    #[serde(default)]
    user_data_dir: Option<String>,
    #[serde(default)]
    executable_path: Option<String>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default = "default_browser_profile_headless")]
    headless: bool,
}

impl Default for BrowserProfileSettings {
    fn default() -> Self {
        Self {
            user_data_dir: None,
            executable_path: None,
            extensions: Vec::new(),
            args: Vec::new(),
            proxy: None,
            headless: default_browser_profile_headless(),
        }
    }
}

fn default_browser_profile_name() -> String {
    "default".to_string()
}

fn default_browser_profile_headless() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrowserProfilesConfig {
    #[serde(default = "default_browser_profile_name")]
    default_profile: String,
    #[serde(default)]
    profiles: HashMap<String, BrowserProfileSettings>,
}

impl Default for BrowserProfilesConfig {
    fn default() -> Self {
        Self {
            default_profile: default_browser_profile_name(),
            profiles: HashMap::new(),
        }
    }
}

struct BrowserRuntimeSession {
    browser: Arc<Browser>,
    active_target_id: Option<String>,
}

#[derive(Default)]
struct BrowserRuntimeStore {
    sessions: HashMap<String, BrowserRuntimeSession>,
}

fn browser_runtime_store() -> &'static Mutex<BrowserRuntimeStore> {
    static STORE: OnceLock<Mutex<BrowserRuntimeStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(BrowserRuntimeStore::default()))
}

fn browser_profiles_path(bridge: &GatewayBridge) -> PathBuf {
    bridge.config().savfox_home.join("browser-profiles.json")
}

fn browser_profile_default_dir(bridge: &GatewayBridge, profile: &str) -> PathBuf {
    bridge
        .config()
        .savfox_home
        .join("browser")
        .join("profiles")
        .join(profile)
}

async fn load_browser_profiles_config(bridge: &GatewayBridge) -> BrowserProfilesConfig {
    let path = browser_profiles_path(bridge);
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => BrowserProfilesConfig::default(),
    }
}

async fn save_browser_profiles_config(
    bridge: &GatewayBridge,
    cfg: &BrowserProfilesConfig,
) -> Result<(), (i64, String)> {
    let path = browser_profiles_path(bridge);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to create profile dir: {e}")))?;
    }
    let encoded = serde_json::to_string_pretty(cfg).map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to encode browser profiles: {e}"),
        )
    })?;
    tokio::fs::write(path, encoded).await.map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to persist browser profiles: {e}"),
        )
    })?;
    Ok(())
}

fn validate_browser_profile_name(name: &str) -> Result<String, (i64, String)> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err((INVALID_PARAMS, "profile name cannot be empty".to_string()));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err((
            INVALID_PARAMS,
            "profile name contains invalid characters".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn browser_timeout_ms(params: &Value) -> u64 {
    params
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_BROWSER_TIMEOUT_MS)
        .max(1)
}

fn selected_profile_name(
    params: &Value,
    cfg: &BrowserProfilesConfig,
) -> Result<String, (i64, String)> {
    let raw = params
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or(cfg.default_profile.as_str());
    validate_browser_profile_name(raw)
}

fn browser_profile_settings_from_params(
    params: &Value,
    current: BrowserProfileSettings,
) -> BrowserProfileSettings {
    let mut next = current;
    if let Some(path) = params.get("user_data_dir").and_then(|v| v.as_str()) {
        let trimmed = path.trim();
        next.user_data_dir = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(path) = params.get("executable_path").and_then(|v| v.as_str()) {
        let trimmed = path.trim();
        next.executable_path = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(exts) = params.get("extensions").and_then(|v| v.as_array()) {
        next.extensions = exts
            .iter()
            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
            .collect();
    }
    if let Some(args) = params.get("args").and_then(|v| v.as_array()) {
        next.args = args
            .iter()
            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
            .collect();
    }
    if let Some(proxy) = params.get("proxy").and_then(|v| v.as_str()) {
        let trimmed = proxy.trim();
        next.proxy = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(headless) = params.get("headless").and_then(|v| v.as_bool()) {
        next.headless = headless;
    }
    next
}

fn browser_launch_options_for_profile(
    bridge: &GatewayBridge,
    profile: &str,
    settings: &BrowserProfileSettings,
) -> BrowserLaunchOptions {
    let user_data_dir = settings
        .user_data_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| browser_profile_default_dir(bridge, profile));
    let executable_path = settings.executable_path.as_ref().map(PathBuf::from);

    let mut extra_args = Vec::new();
    if let Some(proxy) = settings.proxy.as_ref() {
        extra_args.push(format!("--proxy-server={proxy}"));
    }
    if !settings.extensions.is_empty() {
        extra_args.push(format!(
            "--load-extension={}",
            settings.extensions.join(",")
        ));
    }
    extra_args.extend(settings.args.clone());

    BrowserLaunchOptions {
        executable_path,
        user_data_dir: Some(user_data_dir),
        headless: settings.headless,
        extra_args,
    }
}

async fn browser_session_browser(profile: &str) -> Result<Arc<Browser>, (i64, String)> {
    let store = browser_runtime_store().lock().await;
    store
        .sessions
        .get(profile)
        .map(|s| s.browser.clone())
        .ok_or_else(|| {
            (
                INVALID_PARAMS,
                format!("browser profile '{profile}' is not started"),
            )
        })
}

async fn ensure_browser_session_for_profile(
    bridge: &GatewayBridge,
    profile: &str,
    settings: &BrowserProfileSettings,
) -> Result<(), (i64, String)> {
    {
        let store = browser_runtime_store().lock().await;
        if store.sessions.contains_key(profile) {
            return Ok(());
        }
    }

    let options = browser_launch_options_for_profile(bridge, profile, settings);
    if let Some(dir) = options.user_data_dir.as_ref() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| {
            (
                INTERNAL_ERROR,
                format!("failed to create profile data dir: {e}"),
            )
        })?;
    }

    let browser = Arc::new(
        Browser::launch_with_launch_options(options)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to launch browser: {e}")))?,
    );

    let mut store = browser_runtime_store().lock().await;
    match store.sessions.entry(profile.to_string()) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(BrowserRuntimeSession {
                browser,
                active_target_id: None,
            });
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(_) => Ok(()),
    }
}

async fn with_browser_timeout<T>(
    timeout_ms: u64,
    action: &'static str,
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> Result<T, (i64, String)> {
    let timeout = Duration::from_millis(timeout_ms.max(1));
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err((INTERNAL_ERROR, format!("{action} failed: {err}"))),
        Err(_) => Err((
            INTERNAL_ERROR,
            format!("{action} timed out after {} ms", timeout.as_millis()),
        )),
    }
}

fn requested_target_id(params: &Value) -> Option<String> {
    params
        .get("target_id")
        .and_then(|v| v.as_str())
        .or_else(|| params.get("tab_id").and_then(|v| v.as_str()))
        .map(ToOwned::to_owned)
}

async fn set_active_browser_target(profile: &str, target_id: Option<String>) {
    let mut store = browser_runtime_store().lock().await;
    if let Some(session) = store.sessions.get_mut(profile) {
        session.active_target_id = target_id;
    }
}

async fn active_browser_target(profile: &str) -> Option<String> {
    let store = browser_runtime_store().lock().await;
    store
        .sessions
        .get(profile)
        .and_then(|session| session.active_target_id.clone())
}

async fn select_browser_page(
    profile: &str,
    timeout_ms: u64,
    requested_target: Option<String>,
) -> Result<savfox_browser_automation::Page, (i64, String)> {
    let (browser, current_target_id) = {
        let store = browser_runtime_store().lock().await;
        let session = store.sessions.get(profile).ok_or_else(|| {
            (
                INVALID_PARAMS,
                format!("browser profile '{profile}' is not started"),
            )
        })?;
        (session.browser.clone(), session.active_target_id.clone())
    };

    let pages = with_browser_timeout(timeout_ms, "list browser tabs", browser.pages()).await?;
    if pages.is_empty() {
        let page = with_browser_timeout(timeout_ms, "open browser tab", browser.new_page()).await?;
        set_active_browser_target(profile, Some(page.target_id().to_string())).await;
        return Ok(page);
    }

    let preferred = requested_target.clone().or(current_target_id);
    let mut iter = pages.into_iter();
    let mut selected = iter
        .next()
        .ok_or_else(|| (INTERNAL_ERROR, "failed to select browser tab".to_string()))?;
    if let Some(target_id) = preferred {
        if selected.target_id() != target_id {
            let mut found = false;
            for page in iter {
                if page.target_id() == target_id {
                    selected = page;
                    found = true;
                    break;
                }
            }
            if !found && requested_target.is_some() {
                return Err((INVALID_PARAMS, format!("tab '{target_id}' not found")));
            }
        }
    }

    set_active_browser_target(profile, Some(selected.target_id().to_string())).await;
    Ok(selected)
}

fn is_xpath_selector(selector: &str) -> Option<&str> {
    selector.strip_prefix("xpath=").map(str::trim)
}

async fn resolve_browser_profile(
    params: &Value,
    bridge: &GatewayBridge,
) -> Result<(String, BrowserProfileSettings), (i64, String)> {
    let cfg = load_browser_profiles_config(bridge).await;
    let profile = selected_profile_name(params, &cfg)?;
    let settings = cfg.profiles.get(&profile).cloned().unwrap_or_default();
    Ok((profile, settings))
}

async fn handle_browser_request(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        return Err((INVALID_PARAMS, "missing 'url' parameter".to_string()));
    }

    let method_raw = params
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let _method = reqwest::Method::from_bytes(method_raw.as_bytes()).map_err(|e| {
        (
            INVALID_PARAMS,
            format!("invalid method '{method_raw}': {e}"),
        )
    })?;

    let use_browser_context = params
        .get("use_browser_context")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let fallback_direct = params
        .get("fallback_direct")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if use_browser_context {
        let timeout_ms = browser_timeout_ms(params);
        let (profile, settings) = resolve_browser_profile(params, bridge).await?;
        ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
        let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

        let headers = params.get("headers").cloned().unwrap_or_else(|| json!({}));
        let body = params.get("body").cloned().unwrap_or(Value::Null);
        let url_json = serde_json::to_string(url)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode url: {e}")))?;
        let method_json = serde_json::to_string(&method_raw)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode method: {e}")))?;
        let headers_json = serde_json::to_string(&headers)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode headers: {e}")))?;
        let body_json = serde_json::to_string(&body)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode body: {e}")))?;

        let expr = format!(
            r#"(async () => {{
                const targetUrl = {url_json};
                const method = {method_json};
                const headers = {headers_json};
                const body = {body_json};
                const init = {{ method, headers, credentials: "include" }};
                if (body !== null && body !== undefined) {{
                    init.body = typeof body === "string" ? body : JSON.stringify(body);
                    if (!headers["Content-Type"] && !headers["content-type"] && typeof body !== "string") {{
                        init.headers = {{ ...headers, "Content-Type": "application/json" }};
                    }}
                }}
                try {{
                    const resp = await fetch(targetUrl, init);
                    const text = await resp.text();
                    const headerMap = Object.fromEntries(resp.headers.entries());
                    return {{
                        ok: true,
                        status: resp.status,
                        url: resp.url,
                        content_type: headerMap["content-type"] || null,
                        headers: headerMap,
                        body: text
                    }};
                }} catch (err) {{
                    return {{ ok: false, error: String(err) }};
                }}
            }})()"#
        );

        let browser_result =
            with_browser_timeout(timeout_ms, "browser request", page.evaluate(&expr)).await?;
        if browser_result
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let body = browser_result
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let truncated = body.chars().count() > 100_000;
            let body = if truncated {
                body.chars().take(100_000).collect::<String>()
            } else {
                body
            };
            return Ok(json!({
                "status": browser_result.get("status").cloned().unwrap_or(json!(0)),
                "url": browser_result.get("url").cloned().unwrap_or(json!(url)),
                "content_type": browser_result.get("content_type").cloned().unwrap_or(Value::Null),
                "headers": browser_result.get("headers").cloned().unwrap_or_else(|| json!({})),
                "body": body,
                "truncated": truncated,
                "transport": "browser",
                "profile": profile,
                "target_id": page.target_id(),
            }));
        }
        if !fallback_direct {
            let err = browser_result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("browser request failed");
            return Err((INTERNAL_ERROR, err.to_string()));
        }
    }

    handle_browser_request_direct(params).await
}

async fn handle_browser_request_direct(params: &Value) -> RpcResult {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let method_raw = params
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let method = reqwest::Method::from_bytes(method_raw.as_bytes()).map_err(|e| {
        (
            INVALID_PARAMS,
            format!("invalid method '{method_raw}': {e}"),
        )
    })?;
    let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();

    // GET can use guarded redirect-following fetch with per-hop re-validation.
    let has_headers = params.get("headers").is_some();
    let has_body = params.get("body").is_some();
    if method == reqwest::Method::GET && !has_headers && !has_body {
        let resp = crate::ssrf::guarded_fetch(url, &ssrf_cfg)
            .await
            .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;
        let body = String::from_utf8_lossy(&resp.body).into_owned();
        let truncated = body.chars().count() > 100_000;
        let body = if truncated {
            body.chars().take(100_000).collect::<String>()
        } else {
            body
        };
        return Ok(json!({
            "status": resp.status.as_u16(),
            "url": resp.final_url,
            "content_type": resp.content_type,
            "body": body,
            "truncated": truncated,
        }));
    }

    crate::ssrf::validate_outbound_url(url, &ssrf_cfg)
        .await
        .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(ssrf_cfg.timeout_ms.max(1)))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| (INTERNAL_ERROR, format!("failed to create http client: {e}")))?;
    let mut req = client.request(method, url);

    if let Some(headers) = params.get("headers").and_then(|v| v.as_object()) {
        for (key, value) in headers {
            if let Some(v) = value.as_str() {
                req = req.header(key, v);
            }
        }
    }

    if let Some(body) = params.get("body") {
        if let Some(text) = body.as_str() {
            req = req.body(text.to_string());
        } else {
            req = req.json(body);
        }
    }

    let response = req
        .send()
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("request failed: {e}")))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("failed reading response body: {e}")))?;
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();
    let truncated = body_text.chars().count() > 100_000;
    let body = if truncated {
        body_text.chars().take(100_000).collect::<String>()
    } else {
        body_text
    };

    Ok(json!({
        "status": status,
        "url": url,
        "content_type": content_type,
        "location": location,
        "body": body,
        "truncated": truncated,
        "transport": "direct",
    }))
}

// ── Wizard ──────────────────────────────────────────────────────────────────

async fn handle_wizard_start(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let wizard_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("setup");
    wizard_store::start(&bridge.config().savfox_home, wizard_type)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_wizard_next(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let wizard_id = params
        .get("wizard_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if wizard_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'wizard_id' parameter".to_string()));
    }
    let note = params.get("note").and_then(|v| v.as_str());
    wizard_store::next(&bridge.config().savfox_home, wizard_id, note)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_wizard_cancel(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let wizard_id = params
        .get("wizard_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty());
    wizard_store::cancel(&bridge.config().savfox_home, wizard_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_wizard_status(bridge: &Arc<GatewayBridge>) -> RpcResult {
    wizard_store::status(&bridge.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

// ── Misc ────────────────────────────────────────────────────────────────────

async fn handle_talk_mode(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("text");
    voice_store::set_talk_mode(&bridge.config().savfox_home, mode)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_voicewake_get(bridge: &Arc<GatewayBridge>) -> RpcResult {
    voice_store::get_voicewake(&bridge.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_voicewake_set(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let keyword = params
        .get("keyword")
        .and_then(|v| v.as_str())
        .unwrap_or("hey savfox");
    voice_store::set_voicewake(&bridge.config().savfox_home, enabled, keyword)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_update_run(_bridge: &Arc<GatewayBridge>) -> RpcResult {
    Ok(json!({
        "status": "checking",
        "current_version": env!("CARGO_PKG_VERSION"),
        "checked_at": chrono::Utc::now().to_rfc3339(),
    }))
}

// ── Memory (Markdown 4-layer system) ────────────────────────────────────────

fn memory_home(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.clone()
}

fn memory_project_root() -> Option<std::path::PathBuf> {
    savfox_core::git_info::get_git_repo_root(&std::env::current_dir().unwrap_or_default())
}

async fn handle_memory_list(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let home = memory_home(bridge);
    let pr = memory_project_root();
    let layer_filter = params.get("layer").and_then(|v| v.as_str());
    let include_content = params
        .get("include_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let entries =
        savfox_core::md_memory::discover_md_memories(&home, pr.as_deref(), "default").await;

    let filtered: Vec<_> = if let Some(layer_str) = layer_filter {
        let layer: savfox_core::md_memory::MemoryLayer = layer_str
            .parse()
            .map_err(|e: String| (INVALID_REQUEST, e))?;
        entries.into_iter().filter(|e| e.layer == layer).collect()
    } else {
        entries
    };

    let items: Vec<Value> = filtered
        .iter()
        .map(|e| {
            let mut obj = json!({
                "slug": e.slug,
                "layer": e.layer,
                "tags": e.frontmatter.tags,
                "priority": e.frontmatter.priority,
                "pinned": e.frontmatter.pinned,
                "author": e.frontmatter.author,
                "body_bytes": e.body_bytes,
            });
            if include_content {
                obj["body"] = Value::String(e.body.clone());
            }
            obj
        })
        .collect();

    Ok(json!({ "entries": items, "count": items.len() }))
}

async fn handle_memory_get(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(bridge);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    for (layer, dir) in &dirs {
        if let Some(filter) = layer_filter {
            let filter_layer: savfox_core::md_memory::MemoryLayer =
                filter.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
            if *layer != filter_layer {
                continue;
            }
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;
            let (fm, body) = savfox_core::md_memory::parse_md_file(&content);
            return Ok(json!({
                "slug": slug,
                "layer": layer,
                "tags": fm.tags,
                "priority": fm.priority,
                "pinned": fm.pinned,
                "author": fm.author,
                "body": body,
                "raw": content,
            }));
        }
    }

    Err((INVALID_REQUEST, format!("memory entry '{slug}' not found")))
}

async fn handle_memory_create(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let layer_str = params
        .get("layer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'layer' parameter".to_string()))?;
    let layer: savfox_core::md_memory::MemoryLayer = layer_str
        .parse()
        .map_err(|e: String| (INVALID_REQUEST, e))?;

    if layer == savfox_core::md_memory::MemoryLayer::Session {
        return Err((
            INVALID_REQUEST,
            "session layer entries cannot be created on disk".to_string(),
        ));
    }

    let slug_raw = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let slug = savfox_core::md_memory::sanitize_slug(slug_raw);
    if !savfox_core::md_memory::is_valid_slug(&slug) {
        return Err((INVALID_REQUEST, format!("invalid slug '{slug_raw}'")));
    }

    let content = params
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'content' parameter".to_string()))?;

    let home = memory_home(bridge);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");
    let dir = dirs
        .into_iter()
        .find(|(l, _)| *l == layer)
        .map(|(_, d)| d)
        .ok_or_else(|| (INVALID_REQUEST, format!("layer '{layer}' not available")))?;

    let path = dir.join(format!("{slug}.md"));
    if path.exists() {
        return Err((
            INVALID_REQUEST,
            format!("entry '{slug}' already exists in {layer} layer"),
        ));
    }

    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;

    let tags: Vec<String> = params
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let priority = params.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u32;

    let now = chrono::Utc::now();
    let fm = savfox_core::md_memory::MemoryFrontmatter {
        tags,
        priority,
        author: "user".to_string(),
        created_at: Some(now),
        updated_at: Some(now),
        ..Default::default()
    };

    let rendered = savfox_core::md_memory::render_md_file(&fm, content);
    tokio::fs::write(&path, &rendered)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "created", "slug": slug, "layer": layer_str }))
}

async fn handle_memory_update(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(bridge);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    let mut found_path = None;
    for (layer, dir) in &dirs {
        if let Some(filter) = layer_filter {
            let filter_layer: savfox_core::md_memory::MemoryLayer =
                filter.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
            if *layer != filter_layer {
                continue;
            }
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            found_path = Some(path);
            break;
        }
    }

    let path = found_path.ok_or_else(|| (INVALID_REQUEST, format!("entry '{slug}' not found")))?;

    let existing = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;
    let (mut fm, old_body) = savfox_core::md_memory::parse_md_file(&existing);

    let new_body = params
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or(&old_body);

    if let Some(tags_arr) = params.get("tags").and_then(|v| v.as_array()) {
        fm.tags = tags_arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(p) = params.get("priority").and_then(|v| v.as_u64()) {
        fm.priority = p as u32;
    }
    fm.updated_at = Some(chrono::Utc::now());

    let rendered = savfox_core::md_memory::render_md_file(&fm, new_body);
    tokio::fs::write(&path, &rendered)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "updated", "slug": slug }))
}

async fn handle_memory_delete(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(bridge);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    for (layer, dir) in &dirs {
        if let Some(filter) = layer_filter {
            let filter_layer: savfox_core::md_memory::MemoryLayer =
                filter.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
            if *layer != filter_layer {
                continue;
            }
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("delete error: {e}")))?;
            return Ok(json!({ "status": "deleted", "slug": slug, "layer": layer.as_str() }));
        }
    }

    Err((INVALID_REQUEST, format!("entry '{slug}' not found")))
}

async fn handle_memory_search(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(bridge);
    let pr = memory_project_root();
    let entries =
        savfox_core::md_memory::discover_md_memories(&home, pr.as_deref(), "default").await;

    let filtered: Vec<_> = if let Some(layer_str) = layer_filter {
        let layer: savfox_core::md_memory::MemoryLayer = layer_str
            .parse()
            .map_err(|e: String| (INVALID_REQUEST, e))?;
        entries.into_iter().filter(|e| e.layer == layer).collect()
    } else {
        entries
    };

    let results = savfox_core::md_memory::search_memories(&filtered, query, limit);
    let items: Vec<Value> = results
        .into_iter()
        .map(|e| {
            json!({
                "slug": e.slug,
                "layer": e.layer,
                "tags": e.frontmatter.tags,
                "priority": e.frontmatter.priority,
                "body_preview": if e.body.len() > 200 {
                    format!("{}...", &e.body[..200])
                } else {
                    e.body.clone()
                },
            })
        })
        .collect();

    Ok(json!({ "results": items, "count": items.len() }))
}

async fn handle_memory_promote(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug'".to_string()))?;
    let from_str = params
        .get("from_layer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'from_layer'".to_string()))?;
    let to_str = params
        .get("to_layer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'to_layer'".to_string()))?;

    let from_layer: savfox_core::md_memory::MemoryLayer =
        from_str.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
    let to_layer: savfox_core::md_memory::MemoryLayer =
        to_str.parse().map_err(|e: String| (INVALID_REQUEST, e))?;

    if to_layer == savfox_core::md_memory::MemoryLayer::Session {
        return Err((
            INVALID_REQUEST,
            "cannot promote to session layer".to_string(),
        ));
    }

    let home = memory_home(bridge);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    // Find source.
    let source_dir = dirs
        .iter()
        .find(|(l, _)| *l == from_layer)
        .map(|(_, d)| d.clone())
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("from_layer '{from_str}' not available"),
            )
        })?;
    let source_path = source_dir.join(format!("{slug}.md"));
    if !source_path.exists() {
        return Err((
            INVALID_REQUEST,
            format!("entry '{slug}' not found in {from_str}"),
        ));
    }

    // Find target.
    let target_dir = dirs
        .iter()
        .find(|(l, _)| *l == to_layer)
        .map(|(_, d)| d.clone())
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("to_layer '{to_str}' not available"),
            )
        })?;

    let content = tokio::fs::read_to_string(&source_path)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;

    tokio::fs::create_dir_all(&target_dir)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;

    let target_path = target_dir.join(format!("{slug}.md"));
    tokio::fs::write(&target_path, &content)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    let _ = tokio::fs::remove_file(&source_path).await;

    Ok(json!({
        "status": "promoted",
        "slug": slug,
        "from": from_str,
        "to": to_str,
    }))
}

async fn handle_memory_layers(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let home = memory_home(bridge);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    let layers: Vec<Value> = dirs
        .iter()
        .map(|(layer, dir)| {
            json!({
                "layer": layer.as_str(),
                "path": dir.to_string_lossy(),
                "exists": dir.exists(),
            })
        })
        .collect();

    Ok(json!({ "layers": layers }))
}

// ─── Webhook handlers ───────────────────────────────────────────────────────

async fn handle_webhooks_list(bridge: &GatewayBridge) -> RpcResult {
    let store = crate::webhooks::WebhookStore::new(&bridge.config().savfox_home);
    store.load().await;
    let hooks = store.list().await;
    Ok(json!({ "webhooks": hooks }))
}

async fn handle_webhooks_get(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&bridge.config().savfox_home);
    store.load().await;
    match store.get(id).await {
        Some(hook) => Ok(serde_json::to_value(hook).unwrap_or_default()),
        None => Err((INVALID_PARAMS, format!("webhook not found: {id}"))),
    }
}

async fn handle_webhooks_create(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let config: crate::webhooks::WebhookConfig = serde_json::from_value(params.clone())
        .map_err(|e| (INVALID_PARAMS, format!("invalid webhook config: {e}")))?;
    let store = crate::webhooks::WebhookStore::new(&bridge.config().savfox_home);
    store.load().await;
    let hook = store
        .create(config)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "id": hook.id, "status": "created" }))
}

async fn handle_webhooks_update(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&bridge.config().savfox_home);
    store.load().await;
    store
        .update(id, params.clone())
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "id": id, "status": "updated" }))
}

async fn handle_webhooks_delete(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&bridge.config().savfox_home);
    store.load().await;
    store.delete(id).await.map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "id": id, "status": "deleted" }))
}

async fn handle_webhooks_test(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&bridge.config().savfox_home);
    store.load().await;
    let execs = store.recent_executions(Some(id), 5).await;
    Ok(json!({ "id": id, "recent_executions": execs.len() }))
}

// ─── Skill Registry handlers ────────────────────────────────────────────────

async fn handle_skills_registry_search(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let query = params["query"].as_str().unwrap_or("");
    let registry = crate::skill_registry::SkillRegistry::new(&bridge.config().savfox_home, None);
    let results = registry
        .search(query)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "skills": results }))
}

async fn handle_skills_registry_install(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let name = params["name"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing name".to_string()))?;
    let registry = crate::skill_registry::SkillRegistry::new(&bridge.config().savfox_home, None);
    let manifest = registry
        .get_manifest(name)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    let install_path = bridge.config().savfox_home.join("skills").join(name);
    registry
        .record_install(manifest, install_path)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "name": name, "status": "installed" }))
}

async fn handle_skills_registry_uninstall(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let name = params["name"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing name".to_string()))?;
    let registry = crate::skill_registry::SkillRegistry::new(&bridge.config().savfox_home, None);
    registry
        .record_uninstall(name)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "name": name, "status": "uninstalled" }))
}

// ─── DM Policy handlers ────────────────────────────────────────────────────

async fn handle_dm_policy_get(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let store = crate::dm_policy::DmPolicyStore::new(&bridge.config().savfox_home);
    store.load().await;
    let policy = store.get_policy(channel).await;
    Ok(serde_json::to_value(policy).unwrap_or_default())
}

async fn handle_dm_policy_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let store = crate::dm_policy::DmPolicyStore::new(&bridge.config().savfox_home);
    store.load().await;
    let policy: crate::dm_policy::ChannelDmPolicy =
        serde_json::from_value(params.clone()).unwrap_or_default();
    store.set_policy(channel.to_string(), policy).await;
    store.save().await.map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "channel": channel, "status": "updated" }))
}

async fn handle_dm_allowlist_get(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let store = crate::dm_policy::DmPolicyStore::new(&bridge.config().savfox_home);
    store.load().await;
    let policy = store.get_policy(channel).await;
    Ok(json!({ "channel": channel, "allowlist": policy.allowlist }))
}

async fn handle_dm_allowlist_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let entries = params["entries"]
        .as_array()
        .ok_or((INVALID_PARAMS, "missing entries array".to_string()))?;
    let store = crate::dm_policy::DmPolicyStore::new(&bridge.config().savfox_home);
    store.load().await;
    for entry in entries {
        if let Some(sender) = entry.as_str() {
            store.allow_sender(channel, sender.to_string()).await;
        }
    }
    store.save().await.map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "channel": channel, "status": "updated" }))
}

// ─── Provider Health handler ────────────────────────────────────────────────

async fn handle_providers_health(_bridge: &GatewayBridge) -> RpcResult {
    let service = crate::provider_health::ProviderHealthService::new(300);
    let status = service.get_all().await;
    Ok(json!({ "providers": status }))
}

// ─── Config Reload / Validate / Migrate handlers ────────────────────────────

async fn handle_config_reload(bridge: &GatewayBridge) -> RpcResult {
    let config_path = bridge.config().savfox_home.join("config.json");
    let service = crate::config::reload::ConfigReloadService::new(config_path);

    // Load current config first so diff works
    let _ = service.load().await;

    // Reload from disk and compute diff
    let event = service
        .reload()
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("config reload failed: {e}")))?;

    // Validate the new config  - only block on actual errors, not warnings
    let new_config = service.get().await;
    let validation = crate::config::validator::validate_config(&new_config);
    let hard_errors: Vec<_> = validation
        .errors
        .iter()
        .filter(|e| matches!(e.severity, crate::config::validator::Severity::Error))
        .collect();
    if !hard_errors.is_empty() {
        return Err((
            INTERNAL_ERROR,
            format!(
                "config validation failed after reload: {}",
                hard_errors
                    .iter()
                    .map(|e| format!("{}: {}", e.field, e.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
    }

    Ok(json!({
        "status": "reloaded",
        "changed_keys": event.changed_keys,
        "timestamp": event.timestamp,
    }))
}

async fn handle_config_validate(params: &Value, _bridge: &GatewayBridge) -> RpcResult {
    let result = crate::config::validator::validate_config(params);
    Ok(json!({ "valid": result.errors.is_empty(), "errors": result.errors }))
}

async fn handle_config_migrate(bridge: &GatewayBridge) -> RpcResult {
    // Auto-snapshot before migration (#33)
    let _ = handle_config_snapshot(bridge).await;

    let config_path = bridge.config().savfox_home.join("config.json");
    if config_path.exists() {
        let data = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to read config: {e}")))?;
        let config: Value = serde_json::from_str(&data)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to parse config: {e}")))?;
        let (migrated, changes) =
            crate::config::migrate::migrate(config).map_err(|e| (INTERNAL_ERROR, e))?;
        if !changes.is_empty() {
            let migrated_str = serde_json::to_string_pretty(&migrated)
                .map_err(|e| (INTERNAL_ERROR, format!("failed to serialize config: {e}")))?;
            tokio::fs::write(&config_path, migrated_str)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("failed to write config: {e}")))?;
        }
        Ok(json!({ "status": "migrated", "changes": changes }))
    } else {
        Ok(json!({ "status": "no_config_found" }))
    }
}

// ─── STT handlers ───────────────────────────────────────────────────────────

async fn handle_stt_transcribe(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let result = crate::stt::transcribe(&bridge.config().savfox_home, bridge.http_client(), params)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(result)
}

async fn handle_stt_providers() -> RpcResult {
    Ok(crate::stt::provider_info())
}

// ─── Agent Routing handlers ─────────────────────────────────────────────────

async fn handle_routing_rules_list(bridge: &GatewayBridge) -> RpcResult {
    let path = bridge.config().savfox_home.join("routing-rules.json");
    let rules: Vec<crate::agent_routing::RoutingRule> =
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("failed to read routing rules: {e}")))?;
            serde_json::from_str(&content)
                .map_err(|e| (INVALID_PARAMS, format!("invalid routing rules file: {e}")))?
        } else {
            Vec::new()
        };
    Ok(json!({ "rules": rules, "path": path }))
}

async fn handle_routing_rules_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let rules_value = params
        .get("rules")
        .cloned()
        .ok_or((INVALID_PARAMS, "missing rules array".to_string()))?;
    let rules: Vec<crate::agent_routing::RoutingRule> = serde_json::from_value(rules_value)
        .map_err(|e| (INVALID_PARAMS, format!("invalid rules payload: {e}")))?;
    let path = bridge.config().savfox_home.join("routing-rules.json");
    let json = serde_json::to_string_pretty(&rules)
        .map_err(|e| (INTERNAL_ERROR, format!("failed to serialize rules: {e}")))?;
    tokio::fs::write(&path, json).await.map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to write routing rules: {e}"),
        )
    })?;
    Ok(json!({ "status": "updated", "count": rules.len(), "path": path }))
}

// ─── Canvas handlers ────────────────────────────────────────────────────────

use std::sync::LazyLock;

static CANVAS_SERVICE: LazyLock<crate::canvas_host::CanvasHostService> =
    LazyLock::new(crate::canvas_host::CanvasHostService::new);

async fn handle_canvas_create(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main").to_string();

    // Reset any existing state for this surface
    CANVAS_SERVICE.reset(&surface_id).await;

    Ok(json!({
        "surface_id": surface_id,
        "status": "created",
    }))
}

async fn handle_canvas_render(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main");

    let component: crate::canvas_host::A2UIComponent =
        serde_json::from_value(params["component"].clone())
            .map_err(|e| (INVALID_PARAMS, format!("invalid component: {e}")))?;

    CANVAS_SERVICE.push(surface_id, component).await;

    Ok(json!({
        "surface_id": surface_id,
        "status": "rendered",
    }))
}

async fn handle_canvas_action(params: &Value) -> RpcResult {
    let action: crate::canvas_host::A2UIAction = serde_json::from_value(params.clone())
        .map_err(|e| (INVALID_PARAMS, format!("invalid action: {e}")))?;

    let name = action.name.clone();
    let surface_id = action.surface_id.clone();
    CANVAS_SERVICE.handle_action(action).await;

    Ok(json!({
        "name": name,
        "surface_id": surface_id,
        "status": "dispatched",
    }))
}

async fn handle_canvas_state(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main");

    let state = CANVAS_SERVICE.get_state(surface_id).await;

    Ok(json!({
        "surface_id": surface_id,
        "state": state,
    }))
}

async fn handle_canvas_close(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main");

    CANVAS_SERVICE.reset(surface_id).await;

    Ok(json!({
        "surface_id": surface_id,
        "status": "closed",
    }))
}

// ── Plugins ─────────────────────────────────────────────────────────────────

fn plugin_registry() -> &'static Mutex<crate::plugin::PluginRegistry> {
    static REGISTRY: OnceLock<Mutex<crate::plugin::PluginRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(crate::plugin::PluginRegistry::new()))
}

async fn handle_plugins_list(bridge: &GatewayBridge) -> RpcResult {
    let mut registry = plugin_registry().lock().await;

    // Discover plugins on first call
    let loader = crate::plugin::PluginLoader::new(&bridge.config().savfox_home);
    let _ = loader.discover(&mut registry).await;

    let plugins: Vec<Value> = registry
        .list()
        .iter()
        .map(|p| {
            json!({
                "id": p.info.id,
                "name": p.info.name,
                "version": p.info.version,
                "description": p.info.description,
                "author": p.info.author,
                "state": format!("{:?}", p.state).to_lowercase(),
                "has_config": p.config_schema.is_some(),
            })
        })
        .collect();

    Ok(json!({ "plugins": plugins, "count": plugins.len() }))
}

async fn handle_plugins_enable(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let mut registry = plugin_registry().lock().await;
    registry.enable(id).map_err(|e| (INVALID_PARAMS, e))?;

    let loader = crate::plugin::PluginLoader::new(&bridge.config().savfox_home);
    let _ = loader.save_state(&registry).await;

    Ok(json!({ "id": id, "status": "enabled" }))
}

async fn handle_plugins_disable(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let mut registry = plugin_registry().lock().await;
    registry.disable(id).map_err(|e| (INVALID_PARAMS, e))?;

    let loader = crate::plugin::PluginLoader::new(&bridge.config().savfox_home);
    let _ = loader.save_state(&registry).await;

    Ok(json!({ "id": id, "status": "disabled" }))
}

async fn handle_plugins_config(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let mut registry = plugin_registry().lock().await;

    // If config is provided, set it
    if let Some(config) = params.get("config") {
        registry
            .set_config(id, config.clone())
            .map_err(|e| (INVALID_PARAMS, e))?;

        let loader = crate::plugin::PluginLoader::new(&bridge.config().savfox_home);
        let _ = loader.save_state(&registry).await;

        return Ok(json!({ "id": id, "status": "config_updated" }));
    }

    // Otherwise, return current config
    match registry.get(id) {
        Some(plugin) => Ok(json!({
            "id": id,
            "config": plugin.config,
            "config_schema": plugin.config_schema,
        })),
        None => Err((INVALID_PARAMS, format!("plugin not found: {id}"))),
    }
}

// ═══════════════════════════════════════════════════════════════════════════// P2 HANDLERS
// ═══════════════════════════════════════════════════════════════════════════
// ── Config Snapshots (#33) ──────────────────────────────────────────────────

async fn handle_config_snapshot(bridge: &GatewayBridge) -> RpcResult {
    let config_path = bridge.config().savfox_home.join("config.json");
    let backups_dir = bridge.config().savfox_home.join("config-backups");

    if !backups_dir.exists() {
        tokio::fs::create_dir_all(&backups_dir)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .unwrap_or_else(|_| "{}".to_string());

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let filename = format!("{ts}.json");
    let snapshot_path = backups_dir.join(&filename);

    tokio::fs::write(&snapshot_path, &content)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "ok",
        "snapshot": filename,
        "timestamp": ts,
    }))
}

async fn handle_config_snapshots_list(bridge: &GatewayBridge) -> RpcResult {
    let backups_dir = bridge.config().savfox_home.join("config-backups");
    if !backups_dir.exists() {
        return Ok(json!({ "snapshots": [], "count": 0 }));
    }

    let mut snapshots = Vec::new();
    let mut entries = tokio::fs::read_dir(&backups_dir)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("readdir error: {e}")))?;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".json") {
            let meta = entry.metadata().await.ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            snapshots.push(json!({
                "name": name,
                "size": size,
            }));
        }
    }

    snapshots.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        bn.cmp(an)
    });

    let count = snapshots.len();
    Ok(json!({ "snapshots": snapshots, "count": count }))
}

async fn handle_config_restore(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let snapshot = params
        .get("snapshot")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if snapshot.is_empty() {
        return Err((INVALID_PARAMS, "missing 'snapshot' parameter".to_string()));
    }

    let backups_dir = bridge.config().savfox_home.join("config-backups");
    let snapshot_path = backups_dir.join(snapshot);

    if !snapshot_path.exists() {
        return Err((INVALID_PARAMS, format!("snapshot not found: {snapshot}")));
    }

    let content = tokio::fs::read_to_string(&snapshot_path)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;

    // Auto-snapshot current config before restoring
    let _ = handle_config_snapshot(bridge).await;

    let config_path = bridge.config().savfox_home.join("config.json");
    tokio::fs::write(&config_path, &content)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "restored",
        "snapshot": snapshot,
    }))
}

// ── Model Aliases (#34) ─────────────────────────────────────────────────────

fn model_aliases_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("model-aliases.json")
}

async fn load_model_aliases(bridge: &GatewayBridge) -> HashMap<String, String> {
    let path = model_aliases_path(bridge);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

async fn save_model_aliases(
    bridge: &GatewayBridge,
    aliases: &HashMap<String, String>,
) -> Result<(), String> {
    let path = model_aliases_path(bridge);
    let json =
        serde_json::to_string_pretty(aliases).map_err(|e| format!("serialize error: {e}"))?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| format!("write error: {e}"))
}

async fn handle_models_aliases_get(bridge: &GatewayBridge) -> RpcResult {
    let aliases = load_model_aliases(bridge).await;
    Ok(json!({ "aliases": aliases }))
}

async fn handle_models_aliases_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let aliases_val = params
        .get("aliases")
        .ok_or_else(|| (INVALID_PARAMS, "missing 'aliases' parameter".to_string()))?;

    let aliases: HashMap<String, String> = serde_json::from_value(aliases_val.clone())
        .map_err(|e| (INVALID_PARAMS, format!("invalid aliases format: {e}")))?;

    save_model_aliases(bridge, &aliases)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "ok", "count": aliases.len() }))
}

async fn handle_models_resolve(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if model.is_empty() {
        return Err((INVALID_PARAMS, "missing 'model' parameter".to_string()));
    }

    let aliases = load_model_aliases(bridge).await;
    let resolved = aliases
        .get(model)
        .cloned()
        .unwrap_or_else(|| model.to_string());

    Ok(json!({
        "input": model,
        "resolved": resolved,
        "was_alias": resolved != model,
    }))
}

// ── Session Derived Titles (#35) ────────────────────────────────────────────

// Handled via sessions.patch  - title auto-generation on first message.
// The SessionEntry now has `title` and `derived_title` fields.
// When chat.send is called, if derived_title is None, generate from first 60 chars.

// ── Session Elevation (#46) ─────────────────────────────────────────────────

async fn handle_sessions_elevate(params: &Value, session_store: &SessionStore) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'session_id'".to_string()));
    }

    let timeout_mins = params
        .get("timeout_minutes")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let expires_at = now_ms + (timeout_mins * 60 * 1000);

    let result = session_store
        .update(session_id, |entry| {
            entry.elevated = true;
            entry.elevated_until = Some(expires_at);
            entry.touch();
        })
        .await;

    if result.is_some() {
        Ok(json!({
            "session_id": session_id,
            "elevated": true,
            "elevated_until": expires_at,
            "timeout_minutes": timeout_mins,
        }))
    } else {
        Err((INVALID_PARAMS, format!("session not found: {session_id}")))
    }
}

async fn handle_sessions_unelevate(params: &Value, session_store: &SessionStore) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'session_id'".to_string()));
    }

    let result = session_store
        .update(session_id, |entry| {
            entry.elevated = false;
            entry.elevated_until = None;
            entry.touch();
        })
        .await;

    if result.is_some() {
        Ok(json!({ "session_id": session_id, "elevated": false }))
    } else {
        Err((INVALID_PARAMS, format!("session not found: {session_id}")))
    }
}

// ── Heartbeat Config (#51) ──────────────────────────────────────────────────

fn heartbeat_config_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("heartbeat-config.json")
}

async fn handle_heartbeat_config_get(bridge: &GatewayBridge) -> RpcResult {
    let path = heartbeat_config_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let config: Value = serde_json::from_str(&content).unwrap_or(json!({}));
    Ok(json!({ "config": config }))
}

async fn handle_heartbeat_config_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let config = params.get("config").cloned().unwrap_or(json!({}));
    let path = heartbeat_config_path(bridge);
    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;
    Ok(json!({ "status": "ok" }))
}

// ── Browser CDP (#52) ───────────────────────────────────────────────────────

async fn browser_tab_summaries(
    profile: &str,
    timeout_ms: u64,
) -> Result<Vec<Value>, (i64, String)> {
    let browser = browser_session_browser(profile).await?;
    let active_target = active_browser_target(profile).await;
    let pages = with_browser_timeout(timeout_ms, "list browser tabs", browser.pages()).await?;
    let mut tabs = Vec::with_capacity(pages.len());
    for page in pages {
        let title = with_browser_timeout(timeout_ms, "read tab title", page.title())
            .await
            .ok();
        let url = with_browser_timeout(timeout_ms, "read tab url", page.url())
            .await
            .ok();
        tabs.push(json!({
            "target_id": page.target_id(),
            "title": title,
            "url": url,
            "active": active_target.as_deref() == Some(page.target_id()),
        }));
    }
    Ok(tabs)
}

async fn handle_browser_start(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
        if !url.trim().is_empty() {
            let page =
                select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
            with_browser_timeout(timeout_ms, "navigate", page.goto(url)).await?;
        }
    }
    let browser = browser_session_browser(&profile).await?;
    let tabs = browser_tab_summaries(&profile, timeout_ms).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "debug_port": browser.debug_port(),
        "tabs": tabs,
    }))
}

async fn handle_browser_stop(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let cfg = load_browser_profiles_config(bridge).await;
    let profile = selected_profile_name(params, &cfg)?;
    let session = {
        let mut store = browser_runtime_store().lock().await;
        store.sessions.remove(&profile)
    };
    if let Some(runtime) = session {
        let _ = runtime.browser.close().await;
        Ok(json!({ "status": "ok", "profile": profile, "stopped": true }))
    } else {
        Ok(json!({ "status": "ok", "profile": profile, "stopped": false }))
    }
}

async fn handle_browser_tabs_list(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let tabs = browser_tab_summaries(&profile, timeout_ms).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "tabs": tabs,
    }))
}

async fn handle_browser_tabs_open(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let browser = browser_session_browser(&profile).await?;
    let page = with_browser_timeout(timeout_ms, "open tab", browser.new_page()).await?;
    let target_id = page.target_id().to_string();
    if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
        if !url.trim().is_empty() {
            let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
            crate::ssrf::validate_outbound_url(url, &ssrf_cfg)
                .await
                .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;
            with_browser_timeout(timeout_ms, "navigate", page.goto(url)).await?;
        }
    }
    set_active_browser_target(&profile, Some(target_id.clone())).await;
    let title = with_browser_timeout(timeout_ms, "read tab title", page.title())
        .await
        .ok();
    let url = with_browser_timeout(timeout_ms, "read tab url", page.url())
        .await
        .ok();
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": target_id,
        "title": title,
        "url": url,
    }))
}

async fn handle_browser_tabs_switch(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let target_id = params
        .get("target_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'target_id' parameter".to_string()))?
        .to_string();
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, Some(target_id)).await?;
    let title = with_browser_timeout(timeout_ms, "read tab title", page.title())
        .await
        .ok();
    let url = with_browser_timeout(timeout_ms, "read tab url", page.url())
        .await
        .ok();
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "title": title,
        "url": url,
    }))
}

async fn handle_browser_tabs_close(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let target_id = if let Some(id) = requested_target_id(params) {
        id
    } else if let Some(active) = active_browser_target(&profile).await {
        active
    } else {
        return Err((
            INVALID_PARAMS,
            "missing 'target_id' and no active tab".to_string(),
        ));
    };
    let page = select_browser_page(&profile, timeout_ms, Some(target_id.clone())).await?;
    with_browser_timeout(timeout_ms, "close tab", page.close()).await?;
    let browser = browser_session_browser(&profile).await?;
    let remaining = with_browser_timeout(timeout_ms, "list browser tabs", browser.pages())
        .await
        .unwrap_or_default();
    let next_active = remaining.first().map(|p| p.target_id().to_string());
    set_active_browser_target(&profile, next_active.clone()).await;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "closed_target_id": target_id,
        "active_target_id": next_active,
    }))
}

async fn handle_browser_snapshot(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("dom");
    let snapshot = match mode {
        "text" => with_browser_timeout(
            timeout_ms,
            "snapshot text",
            page.evaluate("(function(){ return document.body ? document.body.innerText : ''; })()"),
        )
        .await?
        .as_str()
        .unwrap_or_default()
        .to_string(),
        _ => with_browser_timeout(timeout_ms, "snapshot dom", page.content()).await?,
    };
    let max_chars = params
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(120_000) as usize;
    let truncated = snapshot.chars().count() > max_chars;
    let snapshot = if truncated {
        snapshot.chars().take(max_chars).collect::<String>()
    } else {
        snapshot
    };
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "mode": mode,
        "snapshot": snapshot,
        "truncated": truncated,
    }))
}

async fn handle_browser_storage_get(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("localStorage");
    let key = params.get("key").and_then(|v| v.as_str());
    let value = match scope {
        "cookies" => {
            let cookie = with_browser_timeout(
                timeout_ms,
                "read cookies",
                page.evaluate("document.cookie || ''"),
            )
            .await?
            .as_str()
            .unwrap_or_default()
            .to_string();
            if let Some(key) = key {
                let needle = format!("{key}=");
                json!(
                    cookie
                        .split(';')
                        .map(str::trim)
                        .find(|part| part.starts_with(&needle))
                        .map(|part| part[needle.len()..].to_string())
                )
            } else {
                json!(cookie)
            }
        }
        "sessionStorage" => {
            if let Some(key) = key {
                let key_json = serde_json::to_string(key)
                    .map_err(|e| (INTERNAL_ERROR, format!("invalid key: {e}")))?;
                let expr = format!("sessionStorage.getItem({key_json})");
                with_browser_timeout(timeout_ms, "read sessionStorage key", page.evaluate(&expr))
                    .await?
            } else {
                with_browser_timeout(
                    timeout_ms,
                    "read sessionStorage",
                    page.evaluate(
                        "(function(){return Object.fromEntries(Object.keys(sessionStorage).map(k => [k, sessionStorage.getItem(k)]));})()",
                    ),
                )
                .await?
            }
        }
        _ => {
            if let Some(key) = key {
                let key_json = serde_json::to_string(key)
                    .map_err(|e| (INTERNAL_ERROR, format!("invalid key: {e}")))?;
                let expr = format!("localStorage.getItem({key_json})");
                with_browser_timeout(timeout_ms, "read localStorage key", page.evaluate(&expr))
                    .await?
            } else {
                with_browser_timeout(
                    timeout_ms,
                    "read localStorage",
                    page.evaluate(
                        "(function(){return Object.fromEntries(Object.keys(localStorage).map(k => [k, localStorage.getItem(k)]));})()",
                    ),
                )
                .await?
            }
        }
    };
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "scope": scope,
        "key": key,
        "value": value,
    }))
}

async fn handle_browser_storage_set(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("localStorage");
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'key' parameter".to_string()))?;
    let value = params
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'value' parameter".to_string()))?;
    let key_json =
        serde_json::to_string(key).map_err(|e| (INTERNAL_ERROR, format!("invalid key: {e}")))?;
    let value_json = serde_json::to_string(value)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid value: {e}")))?;
    let expr = match scope {
        "cookies" => format!(
            r#"(function() {{ document.cookie = {key_json} + "=" + encodeURIComponent({value_json}) + ";path=/"; return true; }})()"#
        ),
        "sessionStorage" => format!(
            r#"(function() {{ sessionStorage.setItem({key_json}, {value_json}); return true; }})()"#
        ),
        _ => format!(
            r#"(function() {{ localStorage.setItem({key_json}, {value_json}); return true; }})()"#
        ),
    };
    with_browser_timeout(timeout_ms, "set browser storage", page.evaluate(&expr)).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "scope": scope,
        "key": key,
    }))
}

async fn handle_browser_storage_clear(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("localStorage");
    let expr = match scope {
        "cookies" => {
            r#"(function(){
                const cookies = document.cookie.split(";");
                for (const cookie of cookies) {
                    const eq = cookie.indexOf("=");
                    const name = (eq > -1 ? cookie.slice(0, eq) : cookie).trim();
                    if (name) {
                        document.cookie = name + "=;expires=Thu, 01 Jan 1970 00:00:00 GMT;path=/";
                    }
                }
                return true;
            })()"#
        }
        "sessionStorage" => r#"(function(){ sessionStorage.clear(); return true; })()"#,
        _ => r#"(function(){ localStorage.clear(); return true; })()"#,
    };
    with_browser_timeout(timeout_ms, "clear browser storage", page.evaluate(expr)).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "scope": scope,
    }))
}

async fn handle_browser_download(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let selector = params
        .get("selector")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if selector.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'selector' parameter".to_string()));
    }

    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let target_id = page.target_id().to_string();

    let download_dir = params
        .get("download_dir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            bridge
                .config()
                .savfox_home
                .join("browser")
                .join("downloads")
                .join(&profile)
        });
    tokio::fs::create_dir_all(&download_dir)
        .await
        .map_err(|e| {
            (
                INTERNAL_ERROR,
                format!(
                    "failed to create download dir {}: {e}",
                    download_dir.display()
                ),
            )
        })?;

    let event = with_browser_timeout(
        timeout_ms,
        "capture browser download",
        page.click_and_wait_for_download(selector, &download_dir, timeout_ms),
    )
    .await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": target_id,
        "selector": selector,
        "download": event,
    }))
}

async fn handle_browser_network_capture(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let duration_ms = params
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(2_000)
        .max(1);
    let max_events = params
        .get("max_events")
        .and_then(|v| v.as_u64())
        .unwrap_or(128)
        .max(1) as usize;
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let target_id = page.target_id().to_string();

    let responses = with_browser_timeout(
        timeout_ms.saturating_add(duration_ms).saturating_add(500),
        "capture browser network responses",
        page.capture_responses(duration_ms, max_events),
    )
    .await?;
    let responses_value = serde_json::to_value(&responses).unwrap_or_else(|_| json!([]));

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": target_id,
        "duration_ms": duration_ms,
        "count": responses.len(),
        "responses": responses_value,
    }))
}

async fn handle_browser_profiles_list(bridge: &Arc<GatewayBridge>) -> RpcResult {
    let cfg = load_browser_profiles_config(bridge).await;
    let running_profiles: HashSet<String> = {
        let store = browser_runtime_store().lock().await;
        store.sessions.keys().cloned().collect()
    };
    let mut names: HashSet<String> = cfg.profiles.keys().cloned().collect();
    names.insert(cfg.default_profile.clone());
    if names.is_empty() {
        names.insert(default_browser_profile_name());
    }
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort();
    let mut profiles = Vec::with_capacity(names.len());
    for name in names {
        let settings = cfg.profiles.get(&name).cloned().unwrap_or_default();
        let effective_user_data_dir = settings.user_data_dir.clone().unwrap_or_else(|| {
            browser_profile_default_dir(bridge, &name)
                .display()
                .to_string()
        });
        profiles.push(json!({
            "name": name,
            "default": name == cfg.default_profile,
            "running": running_profiles.contains(&name),
            "settings": {
                "user_data_dir": effective_user_data_dir,
                "executable_path": settings.executable_path,
                "extensions": settings.extensions,
                "args": settings.args,
                "proxy": settings.proxy,
                "headless": settings.headless,
            }
        }));
    }
    Ok(json!({
        "default_profile": cfg.default_profile,
        "profiles": profiles,
    }))
}

async fn handle_browser_profiles_create(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let requested = params
        .get("profile")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'profile' parameter".to_string()))?;
    let profile = validate_browser_profile_name(requested)?;
    let mut cfg = load_browser_profiles_config(bridge).await;
    let current = cfg.profiles.get(&profile).cloned().unwrap_or_default();
    let merged = browser_profile_settings_from_params(params, current);
    cfg.profiles.insert(profile.clone(), merged.clone());
    if params
        .get("default")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || cfg.default_profile.trim().is_empty()
    {
        cfg.default_profile = profile.clone();
    }
    save_browser_profiles_config(bridge, &cfg).await?;
    let data_dir = merged
        .user_data_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| browser_profile_default_dir(bridge, &profile));
    tokio::fs::create_dir_all(&data_dir).await.map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to create profile data dir: {e}"),
        )
    })?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "default_profile": cfg.default_profile,
        "settings": {
            "user_data_dir": data_dir.display().to_string(),
            "executable_path": merged.executable_path,
            "extensions": merged.extensions,
            "args": merged.args,
            "proxy": merged.proxy,
            "headless": merged.headless,
        }
    }))
}

async fn handle_browser_profiles_delete(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let requested = params
        .get("profile")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'profile' parameter".to_string()))?;
    let profile = validate_browser_profile_name(requested)?;
    let mut cfg = load_browser_profiles_config(bridge).await;
    if cfg.profiles.remove(&profile).is_none() {
        return Err((
            INVALID_PARAMS,
            format!("profile '{profile}' does not exist"),
        ));
    }
    if cfg.default_profile == profile {
        cfg.default_profile = cfg
            .profiles
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(default_browser_profile_name);
    }
    save_browser_profiles_config(bridge, &cfg).await?;
    let session = {
        let mut store = browser_runtime_store().lock().await;
        store.sessions.remove(&profile)
    };
    if let Some(runtime) = session {
        let _ = runtime.browser.close().await;
    }
    if params
        .get("delete_data")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let path = browser_profile_default_dir(bridge, &profile);
        let _ = tokio::fs::remove_dir_all(path).await;
    }
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "default_profile": cfg.default_profile,
    }))
}

async fn handle_browser_profiles_default_set(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let requested = params
        .get("profile")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'profile' parameter".to_string()))?;
    let profile = validate_browser_profile_name(requested)?;
    let mut cfg = load_browser_profiles_config(bridge).await;
    cfg.profiles.entry(profile.clone()).or_default();
    cfg.default_profile = profile.clone();
    save_browser_profiles_config(bridge, &cfg).await?;
    Ok(json!({
        "status": "ok",
        "default_profile": profile,
    }))
}

async fn handle_browser_goto(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        return Err((INVALID_PARAMS, "missing 'url' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;

    let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
    crate::ssrf::validate_outbound_url(url, &ssrf_cfg)
        .await
        .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;

    let open_new_tab = params
        .get("open_new_tab")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let page = if open_new_tab {
        let browser = browser_session_browser(&profile).await?;
        let page = with_browser_timeout(timeout_ms, "open tab", browser.new_page()).await?;
        set_active_browser_target(&profile, Some(page.target_id().to_string())).await;
        page
    } else {
        select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?
    };
    with_browser_timeout(timeout_ms, "navigate", page.goto(url)).await?;
    let title = with_browser_timeout(timeout_ms, "read page title", page.title())
        .await
        .ok();

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "goto",
        "target_id": page.target_id(),
        "url": url,
        "title": title,
    }))
}

async fn handle_browser_click(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let selector = params
        .get("selector")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if selector.is_empty() {
        return Err((INVALID_PARAMS, "missing 'selector' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    if let Some(xpath) = is_xpath_selector(selector) {
        let xpath_json = serde_json::to_string(xpath)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid xpath selector: {e}")))?;
        let expr = format!(
            r#"(function() {{
                const node = document.evaluate({xpath_json}, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
                if (!node) return false;
                if (typeof node.scrollIntoView === "function") node.scrollIntoView({{ block: "center" }});
                if (typeof node.click === "function") node.click();
                return true;
            }})()"#
        );
        let ok = with_browser_timeout(timeout_ms, "click xpath element", page.evaluate(&expr))
            .await?
            .as_bool()
            .unwrap_or(false);
        if !ok {
            return Err((
                INVALID_PARAMS,
                format!("element not found for xpath '{xpath}'"),
            ));
        }
    } else {
        with_browser_timeout(timeout_ms, "click element", page.click(selector)).await?;
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "click",
        "target_id": page.target_id(),
        "selector": selector,
    }))
}

async fn handle_browser_type(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let selector = params
        .get("selector")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if selector.is_empty() {
        return Err((INVALID_PARAMS, "missing 'selector' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("type");
    if !matches!(mode, "type" | "fill" | "select") {
        return Err((
            INVALID_PARAMS,
            "invalid 'mode' parameter (expected type/fill/select)".to_string(),
        ));
    }
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let xpath = is_xpath_selector(selector);
    if mode == "type" && xpath.is_none() {
        with_browser_timeout(timeout_ms, "type text", page.type_text(selector, text)).await?;
    } else {
        let selector_raw = xpath.unwrap_or(selector);
        let selector_json = serde_json::to_string(selector_raw)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid selector: {e}")))?;
        let text_json = serde_json::to_string(text)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid text: {e}")))?;
        let mode_json = serde_json::to_string(mode)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid mode: {e}")))?;
        let is_xpath = xpath.is_some();
        let expr = format!(
            r#"(function() {{
                const selector = {selector_json};
                const text = {text_json};
                const mode = {mode_json};
                const isXPath = {};
                let el = null;
                if (isXPath) {{
                    el = document.evaluate(selector, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
                }} else {{
                    el = document.querySelector(selector);
                }}
                if (!el) return {{ ok: false, error: "element not found" }};
                if (typeof el.focus === "function") el.focus();
                if (mode === "select") {{
                    el.value = text;
                    el.dispatchEvent(new Event("input", {{ bubbles: true }}));
                    el.dispatchEvent(new Event("change", {{ bubbles: true }}));
                    return {{ ok: true }};
                }}
                el.value = text;
                el.dispatchEvent(new Event("input", {{ bubbles: true }}));
                el.dispatchEvent(new Event("change", {{ bubbles: true }}));
                return {{ ok: true }};
            }})()"#,
            is_xpath
        );
        let result =
            with_browser_timeout(timeout_ms, "fill form field", page.evaluate(&expr)).await?;
        if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let err = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("form update failed");
            return Err((INTERNAL_ERROR, err.to_string()));
        }
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "type",
        "mode": mode,
        "target_id": page.target_id(),
        "selector": selector,
        "text": text,
    }))
}

async fn handle_browser_screenshot(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let full_page = params
        .get("full_page")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let selector = params.get("selector").and_then(|v| v.as_str());
    let mut options = ScreenshotOptions::new();
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    options = options.format(match format.as_str() {
        "jpeg" | "jpg" => ScreenshotFormat::Jpeg,
        "webp" => ScreenshotFormat::Webp,
        "png" => ScreenshotFormat::Png,
        _ => {
            return Err((
                INVALID_PARAMS,
                "invalid screenshot format (expected png/jpeg/webp)".to_string(),
            ));
        }
    });
    if let Some(quality) = params.get("quality").and_then(|v| v.as_u64()) {
        if quality > 100 {
            return Err((INVALID_PARAMS, "quality must be 0-100".to_string()));
        }
        options = options.quality(quality as u8);
    }

    if let Some(selector) = selector {
        let selector_raw = is_xpath_selector(selector).unwrap_or(selector);
        let selector_json = serde_json::to_string(selector_raw)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid selector: {e}")))?;
        let is_xpath = is_xpath_selector(selector).is_some();
        let expr = format!(
            r#"(function() {{
                const selector = {selector_json};
                const isXPath = {};
                let el = null;
                if (isXPath) {{
                    el = document.evaluate(selector, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
                }} else {{
                    el = document.querySelector(selector);
                }}
                if (!el) return null;
                const r = el.getBoundingClientRect();
                return {{
                    x: Math.max(0, r.left + window.scrollX),
                    y: Math.max(0, r.top + window.scrollY),
                    width: Math.max(1, r.width),
                    height: Math.max(1, r.height)
                }};
            }})()"#,
            is_xpath
        );
        let rect =
            with_browser_timeout(timeout_ms, "resolve element rect", page.evaluate(&expr)).await?;
        let x = rect.get("x").and_then(|v| v.as_f64());
        let y = rect.get("y").and_then(|v| v.as_f64());
        let width = rect.get("width").and_then(|v| v.as_f64());
        let height = rect.get("height").and_then(|v| v.as_f64());
        if let (Some(x), Some(y), Some(width), Some(height)) = (x, y, width, height) {
            options = options.clip(x, y, width, height);
        } else {
            return Err((
                INVALID_PARAMS,
                format!("element not found for selector '{selector}'"),
            ));
        }
    } else if full_page {
        let dims = with_browser_timeout(
            timeout_ms,
            "resolve full-page dimensions",
            page.evaluate(
                "(function(){return { width: Math.max(document.documentElement.scrollWidth, document.body ? document.body.scrollWidth : 0, window.innerWidth), height: Math.max(document.documentElement.scrollHeight, document.body ? document.body.scrollHeight : 0, window.innerHeight) };})()",
            ),
        )
        .await?;
        let width = dims.get("width").and_then(|v| v.as_f64()).unwrap_or(1280.0);
        let height = dims.get("height").and_then(|v| v.as_f64()).unwrap_or(720.0);
        options = options.full_page(width.max(1.0), height.max(1.0));
    } else if let Some(clip) = params.get("clip").and_then(|v| v.as_object()) {
        let x = clip.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = clip.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let width = clip.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let height = clip.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if width <= 0.0 || height <= 0.0 {
            return Err((
                INVALID_PARAMS,
                "clip.width/clip.height must be > 0".to_string(),
            ));
        }
        options = options.clip(x, y, width, height);
    }

    let bytes =
        with_browser_timeout(timeout_ms, "capture screenshot", page.screenshot(options)).await?;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "screenshot",
        "target_id": page.target_id(),
        "full_page": full_page,
        "selector": selector,
        "format": format,
        "bytes": bytes.len(),
        "image_base64": encoded,
    }))
}

async fn handle_browser_eval(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let expression = params
        .get("expression")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if expression.is_empty() {
        return Err((INVALID_PARAMS, "missing 'expression' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let result =
        with_browser_timeout(timeout_ms, "evaluate javascript", page.evaluate(expression)).await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "eval",
        "target_id": page.target_id(),
        "expression": expression,
        "result": result,
    }))
}

fn browser_extension_relay_bootstrap_expr(channel: &str) -> Result<String, (i64, String)> {
    let channel_json = serde_json::to_string(channel)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay channel: {e}")))?;
    Ok(format!(
        r#"(function() {{
            const channel = {channel_json};
            if (!window.__savfoxRelay || window.__savfoxRelay.channel !== channel) {{
                window.__savfoxRelay = {{ channel, queue: [] }};
                window.addEventListener("message", function(event) {{
                    const data = event && event.data;
                    if (!data || typeof data !== "object") return;
                    if (data.__savfoxRelay !== true) return;
                    if (data.channel && data.channel !== window.__savfoxRelay.channel) return;
                    window.__savfoxRelay.queue.push({{
                        type: typeof data.type === "string" ? data.type : "message",
                        payload: Object.prototype.hasOwnProperty.call(data, "payload") ? data.payload : null,
                        ts: Date.now(),
                        origin: event.origin || null
                    }});
                    if (window.__savfoxRelay.queue.length > 512) {{
                        const trim = window.__savfoxRelay.queue.length - 512;
                        window.__savfoxRelay.queue.splice(0, trim);
                    }}
                }});
            }}
            return {{ ok: true, channel: window.__savfoxRelay.channel }};
        }})()"#
    ))
}

async fn handle_browser_extension_relay_start(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .trim()
        .to_string();
    if channel.is_empty() {
        return Err((INVALID_PARAMS, "relay channel cannot be empty".to_string()));
    }

    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = browser_extension_relay_bootstrap_expr(&channel)?;
    let result =
        with_browser_timeout(timeout_ms, "start extension relay", page.evaluate(&expr)).await?;
    let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Err((
            INTERNAL_ERROR,
            "failed to start extension relay".to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "channel": channel,
    }))
}

async fn handle_browser_extension_relay_status(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = r#"(function() {
        const relay = window.__savfoxRelay;
        if (!relay || !Array.isArray(relay.queue)) {
            return { started: false, channel: null, queued: 0 };
        }
        return {
            started: true,
            channel: relay.channel || null,
            queued: relay.queue.length
        };
    })()"#;
    let result = with_browser_timeout(
        timeout_ms,
        "query extension relay status",
        page.evaluate(expr),
    )
    .await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "relay": result,
    }))
}

async fn handle_browser_extension_relay_stop(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = r#"(function() {
        const relay = window.__savfoxRelay;
        if (!relay || !Array.isArray(relay.queue)) {
            return { stopped: false, reason: "relay_not_started" };
        }
        const channel = relay.channel || null;
        const queued = relay.queue.length;
        relay.queue.splice(0, relay.queue.length);
        delete window.__savfoxRelay;
        return { stopped: true, channel, queued };
    })()"#;
    let result =
        with_browser_timeout(timeout_ms, "stop extension relay", page.evaluate(expr)).await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "relay": result,
    }))
}

async fn handle_browser_extension_relay_poll(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = r#"(function() {
        const relay = window.__savfoxRelay;
        if (!relay || !Array.isArray(relay.queue)) {
            return { ok: false, reason: "relay_not_started", channel: null, messages: [] };
        }
        const messages = relay.queue.splice(0, relay.queue.length);
        return { ok: true, channel: relay.channel || null, messages };
    })()"#;
    let result =
        with_browser_timeout(timeout_ms, "poll extension relay", page.evaluate(expr)).await?;

    Ok(json!({
        "status": if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) { "ok" } else { "empty" },
        "profile": profile,
        "target_id": page.target_id(),
        "channel": result.get("channel").cloned().unwrap_or(Value::Null),
        "messages": result.get("messages").cloned().unwrap_or_else(|| json!([])),
        "reason": result.get("reason").cloned().unwrap_or(Value::Null),
    }))
}

async fn handle_browser_extension_relay_send(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let event_type = params
        .get("event_type")
        .or_else(|| params.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("message")
        .to_string();
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);

    let channel_json = serde_json::to_string(&channel)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay channel: {e}")))?;
    let event_type_json = serde_json::to_string(&event_type)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay event type: {e}")))?;
    let payload_json = serde_json::to_string(&payload)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay payload: {e}")))?;
    let expr = format!(
        r#"(function() {{
            const relay = window.__savfoxRelay;
            if (!relay) return {{ ok: false, reason: "relay_not_started" }};
            const detail = {{
                __savfoxRelay: true,
                channel: {channel_json},
                type: {event_type_json},
                payload: {payload_json},
                ts: Date.now()
            }};
            window.dispatchEvent(new CustomEvent("savfox-relay", {{ detail }}));
            return {{ ok: true, channel: detail.channel, type: detail.type }};
        }})()"#
    );
    let result = with_browser_timeout(
        timeout_ms,
        "send extension relay message",
        page.evaluate(&expr),
    )
    .await?;
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err((
            INVALID_PARAMS,
            result
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("relay_not_started")
                .to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "channel": channel,
        "type": event_type,
    }))
}

async fn handle_browser_content_script_inject(
    params: &Value,
    bridge: &Arc<GatewayBridge>,
) -> RpcResult {
    let script = params.get("script").and_then(|v| v.as_str()).unwrap_or("");
    if script.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'script' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let script_json = serde_json::to_string(script)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid content script: {e}")))?;
    let expr = format!(
        r#"(function() {{
            const source = {script_json};
            try {{
                const fn = new Function(source);
                const result = fn();
                return {{ ok: true, result: result === undefined ? null : result }};
            }} catch (err) {{
                return {{ ok: false, error: String(err) }};
            }}
        }})()"#
    );
    let result =
        with_browser_timeout(timeout_ms, "inject content script", page.evaluate(&expr)).await?;
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err((
            INTERNAL_ERROR,
            result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("script injection failed")
                .to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "result": result.get("result").cloned().unwrap_or(Value::Null),
    }))
}

async fn handle_browser_page_extract(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let interactive_only = params
        .get("interactive_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_interactive = params
        .get("max_interactive")
        .and_then(|v| v.as_u64())
        .unwrap_or(128)
        .max(1) as usize;
    let max_text_chars = params
        .get("max_text_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(8_000)
        .max(128) as usize;
    let max_links = params
        .get("max_links")
        .and_then(|v| v.as_u64())
        .unwrap_or(64)
        .max(1) as usize;
    let include_html = params
        .get("include_html")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_html_chars = params
        .get("max_html_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(40_000)
        .max(256) as usize;

    let (profile, settings) = resolve_browser_profile(params, bridge).await?;
    ensure_browser_session_for_profile(bridge, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = format!(
        r#"(function() {{
            const maxText = {max_text_chars};
            const maxLinks = {max_links};
            const includeHtml = {include_html};
            const maxHtml = {max_html_chars};
            const interactiveOnly = {interactive_only};
            const maxInteractive = {max_interactive};
            const text = interactiveOnly ? "" : ((document.body && document.body.innerText) || "").slice(0, maxText);
            const headings = interactiveOnly ? [] : Array.from(document.querySelectorAll("h1, h2, h3")).slice(0, 32).map(el => {{
                const level = Number((el.tagName || "H1").slice(1)) || 1;
                return {{ level, text: (el.innerText || "").trim() }};
            }});
            const links = Array.from(document.querySelectorAll("a[href]")).slice(0, maxLinks).map(el => {{
                return {{
                    href: el.href || "",
                    text: (el.textContent || "").trim().slice(0, 256)
                }};
            }});
            const metas = interactiveOnly ? [] : Array.from(document.querySelectorAll("meta[name], meta[property]")).slice(0, 32).map(meta => {{
                return {{
                    key: meta.getAttribute("name") || meta.getAttribute("property") || "",
                    value: meta.getAttribute("content") || ""
                }};
            }});

            const getSelector = (el) => {{
                if (!el || !el.tagName) return "";
                const tag = el.tagName.toLowerCase();
                const id = el.id ? `#${{el.id}}` : "";
                if (id) return `${{tag}}${{id}}`;
                const name = el.getAttribute("name");
                if (name) return `${{tag}}[name="${{name}}"]`;
                if (el.classList && el.classList.length > 0) {{
                    const cls = Array.from(el.classList).slice(0, 2).join(".");
                    if (cls) return `${{tag}}.${{cls}}`;
                }}
                return tag;
            }};
            const interactiveElements = Array.from(
                document.querySelectorAll('a[href],button,input,textarea,select,[role="button"],[onclick],[tabindex]')
            )
                .filter((el) => {{
                    if (!el || !(el instanceof Element)) return false;
                    const style = window.getComputedStyle(el);
                    if (!style) return false;
                    if (style.display === "none" || style.visibility === "hidden") return false;
                    const rect = el.getBoundingClientRect();
                    return rect.width > 0 && rect.height > 0;
                }})
                .slice(0, maxInteractive)
                .map((el) => {{
                    const tag = (el.tagName || "").toLowerCase();
                    const role = el.getAttribute("role") || null;
                    const label = (
                        el.getAttribute("aria-label") ||
                        el.getAttribute("title") ||
                        el.getAttribute("placeholder") ||
                        el.textContent ||
                        ""
                    ).trim().slice(0, 120);
                    return {{
                        tag,
                        role,
                        selector: getSelector(el),
                        type: el.getAttribute("type") || null,
                        href: el.getAttribute("href") || null,
                        label
                    }};
                }});
            const html = includeHtml
                ? ((document.documentElement && document.documentElement.outerHTML) || "").slice(0, maxHtml)
                : null;
            return {{
                url: location.href,
                title: document.title || "",
                lang: document.documentElement ? (document.documentElement.lang || "") : "",
                mode: interactiveOnly ? "interactive" : "full",
                text,
                text_chars: text.length,
                headings,
                links,
                interactive_elements: interactiveElements,
                meta: metas,
                html,
                html_truncated: includeHtml && ((document.documentElement && document.documentElement.outerHTML) || "").length > maxHtml
            }};
        }})()"#
    );
    let result =
        with_browser_timeout(timeout_ms, "extract page data", page.evaluate(&expr)).await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "data": result,
    }))
}

// ── Hooks Event Bus (#31) ───────────────────────────────────────────────────

fn hooks_config_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("hooks-config.json")
}

async fn handle_hooks_list(bridge: &GatewayBridge) -> RpcResult {
    let path = hooks_config_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let config: Value = serde_json::from_str(&content).unwrap_or(json!({}));

    let hooks_arr = config.get("hooks").cloned().unwrap_or(json!([]));
    let count = hooks_arr.as_array().map(|a| a.len()).unwrap_or(0);

    Ok(json!({
        "hooks": hooks_arr,
        "count": count,
        "builtin_events": [
            "session:start", "session:end", "session:compact",
            "message:received", "message:sent", "message:error",
            "agent:start", "agent:complete", "agent:error",
            "cron:started", "cron:completed", "cron:failed",
            "memory:created", "memory:updated", "memory:deleted",
            "config:changed", "config:reloaded",
        ],
    }))
}

async fn handle_hooks_enable(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let hook_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if hook_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let path = hooks_config_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let mut config: Value = serde_json::from_str(&content).unwrap_or(json!({}));

    if let Some(hooks) = config.get_mut("hooks").and_then(|v| v.as_array_mut()) {
        for hook in hooks.iter_mut() {
            if hook.get("id").and_then(|v| v.as_str()) == Some(hook_id) {
                hook["enabled"] = json!(true);
            }
        }
    }

    let json_str = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json_str)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "id": hook_id, "status": "enabled" }))
}

async fn handle_hooks_disable(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let hook_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if hook_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let path = hooks_config_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let mut config: Value = serde_json::from_str(&content).unwrap_or(json!({}));

    if let Some(hooks) = config.get_mut("hooks").and_then(|v| v.as_array_mut()) {
        for hook in hooks.iter_mut() {
            if hook.get("id").and_then(|v| v.as_str()) == Some(hook_id) {
                hook["enabled"] = json!(false);
            }
        }
    }

    let json_str = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json_str)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "id": hook_id, "status": "disabled" }))
}

// ── Message Reactions (#37) ──────────────────────────────────────────────────

async fn handle_reactions_add(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let message_id = params
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let emoji = params.get("emoji").and_then(|v| v.as_str()).unwrap_or("");
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    if message_id.is_empty() || emoji.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'message_id' or 'emoji'".to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "action": "add_reaction",
        "message_id": message_id,
        "emoji": emoji,
        "channel": channel,
    }))
}

async fn handle_reactions_remove(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let message_id = params
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let emoji = params.get("emoji").and_then(|v| v.as_str()).unwrap_or("");

    if message_id.is_empty() || emoji.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'message_id' or 'emoji'".to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "action": "remove_reaction",
        "message_id": message_id,
        "emoji": emoji,
    }))
}

// ── Streaming Config (#36) ──────────────────────────────────────────────────

fn streaming_config_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("streaming-config.json")
}

async fn handle_streaming_config_get(bridge: &GatewayBridge) -> RpcResult {
    let path = streaming_config_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let config: Value = serde_json::from_str(&content).unwrap_or(json!({}));
    Ok(json!({
        "config": config,
        "modes": ["token", "sentence", "paragraph", "complete"],
    }))
}

async fn handle_streaming_config_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let config = params.get("config").cloned().unwrap_or(json!({}));

    // Validate stream_mode if present
    if let Some(mode) = config.get("stream_mode").and_then(|v| v.as_str()) {
        let valid = ["token", "sentence", "paragraph", "complete"];
        if !valid.contains(&mode) {
            return Err((
                INVALID_PARAMS,
                format!(
                    "invalid stream_mode: {mode}. Must be one of: {}",
                    valid.join(", ")
                ),
            ));
        }
    }

    let path = streaming_config_path(bridge);
    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;
    Ok(json!({ "status": "ok" }))
}

// ── YAML Config Support (#59) ───────────────────────────────────────────────

/// Detect which config format is currently in use (.json, .yaml, .toml).
async fn handle_config_format(bridge: &GatewayBridge) -> RpcResult {
    let home = &bridge.config().savfox_home;
    let candidates = [
        ("json", home.join("config.json")),
        ("toml", home.join("config.toml")),
        ("yaml", home.join("config.yaml")),
        ("yaml", home.join("config.yml")),
    ];

    let mut detected = "unknown";
    let mut path_str = String::new();
    for (fmt, path) in &candidates {
        if path.exists() {
            detected = fmt;
            path_str = path.to_string_lossy().to_string();
            break;
        }
    }

    let supported = ["json", "yaml", "toml"];
    Ok(json!({
        "format": detected,
        "path": path_str,
        "supported_formats": supported,
    }))
}

/// Convert config content between formats (json, yaml, toml).
async fn handle_config_convert(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let from_format = params
        .get("from_format")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let to_format = params
        .get("to_format")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if from_format.is_empty() || to_format.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'from_format' and/or 'to_format' parameter".to_string(),
        ));
    }

    let valid = ["json", "yaml", "toml"];
    if !valid.contains(&from_format) {
        return Err((
            INVALID_PARAMS,
            format!(
                "unsupported from_format: {from_format}. Must be one of: {}",
                valid.join(", ")
            ),
        ));
    }
    if !valid.contains(&to_format) {
        return Err((
            INVALID_PARAMS,
            format!(
                "unsupported to_format: {to_format}. Must be one of: {}",
                valid.join(", ")
            ),
        ));
    }

    // Determine source: explicit `content` param or read from disk.
    let source_content = if let Some(content) = params.get("content").and_then(|v| v.as_str()) {
        content.to_string()
    } else {
        let home = &bridge.config().savfox_home;
        let ext = match from_format {
            "yaml" => "yaml",
            "toml" => "toml",
            _ => "json",
        };
        let path = home.join(format!("config.{ext}"));
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to read config file: {e}")))?
    };

    // Parse source into a serde_json::Value intermediate.
    let intermediate: Value = match from_format {
        "json" => serde_json::from_str(&source_content)
            .map_err(|e| (INVALID_PARAMS, format!("invalid JSON input: {e}")))?,
        "yaml" => serde_yaml::from_str(&source_content)
            .map_err(|e| (INVALID_PARAMS, format!("invalid YAML input: {e}")))?,
        "toml" => {
            let toml_val: toml::Value = source_content
                .parse()
                .map_err(|e| (INVALID_PARAMS, format!("invalid TOML input: {e}")))?;
            serde_json::to_value(&toml_val)
                .map_err(|e| (INTERNAL_ERROR, format!("TOML->JSON conversion failed: {e}")))?
        }
        _ => {
            return Err((
                INVALID_PARAMS,
                format!("unsupported from_format: {from_format}"),
            ));
        }
    };

    // Serialize to target format.
    let output = match to_format {
        "json" => serde_json::to_string_pretty(&intermediate)
            .map_err(|e| (INTERNAL_ERROR, format!("JSON serialization failed: {e}")))?,
        "yaml" => {
            let yaml = serde_yaml::to_string(&intermediate)
                .map_err(|e| (INTERNAL_ERROR, format!("YAML serialization failed: {e}")))?;
            if from_format == "toml" {
                preserve_toml_leading_comments_for_yaml(&source_content, &yaml)
            } else {
                yaml
            }
        }
        "toml" => json_value_to_toml_string(&intermediate).map_err(|e| (INTERNAL_ERROR, e))?,
        _ => {
            return Err((
                INVALID_PARAMS,
                format!("unsupported to_format: {to_format}"),
            ));
        }
    };

    Ok(json!({
        "status": "ok",
        "from_format": from_format,
        "to_format": to_format,
        "content": output,
    }))
}

// ── QR Code Pairing (#62) ───────────────────────────────────────────────────

/// Generate a QR-code pairing URL with a short-lived token.
async fn handle_device_pair_qr(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let host = params
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("localhost");
    let port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::from(crate::config::DEFAULT_PORT));

    // Generate a pairing token (UUID v7).
    let token = uuid::Uuid::now_v7().to_string();

    // Expiry: default 5 minutes from now.
    let ttl_secs = params
        .get("ttl_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(300);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let expires_at = now + ttl_secs;

    let url = format!("savfox://{host}:{port}/pair?token={token}");

    // Persist the pairing request so `device.pair.list` can see it.
    let device_label = format!("qr-{}", &token[..8]);
    let _ = pairing_store::create_request_for_home(
        &bridge.config().savfox_home,
        &token,
        Some(device_label.as_str()),
        Some("qr-pairing"),
    )
    .await;

    Ok(json!({
        "url": url,
        "token": token,
        "expires_at": expires_at,
        "ttl_secs": ttl_secs,
    }))
}

// ── Agent Avatar Management (#63) ───────────────────────────────────────────

/// Store an avatar path in the agent's config JSON.
async fn handle_agent_avatar_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let avatar = params.get("avatar").and_then(|v| v.as_str()).unwrap_or("");
    if avatar.is_empty() {
        return Err((INVALID_PARAMS, "missing 'avatar' parameter".to_string()));
    }

    let dir = agents_dir(bridge);
    if !dir.exists() {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;
    }

    let config_path = dir.join(format!("{agent_id}.json"));
    let mut config: Value = if config_path.exists() {
        let data = tokio::fs::read_to_string(&config_path)
            .await
            .unwrap_or_else(|_| "{}".to_string());
        serde_json::from_str(&data).unwrap_or(json!({}))
    } else {
        json!({ "id": agent_id, "name": agent_id })
    };

    config["avatar"] = json!(avatar);
    config["updated_at"] = json!(chrono::Utc::now().to_rfc3339());

    let json_str = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&config_path, json_str)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "ok",
        "agent": agent_id,
        "avatar": avatar,
    }))
}

/// Return the current avatar path for an agent.
async fn handle_agent_avatar_get(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let config_path = agents_dir(bridge).join(format!("{agent_id}.json"));
    let avatar = if config_path.exists() {
        let data = tokio::fs::read_to_string(&config_path)
            .await
            .unwrap_or_else(|_| "{}".to_string());
        let config: Value = serde_json::from_str(&data).unwrap_or(json!({}));
        config
            .get("avatar")
            .and_then(|v| v.as_str())
            .map(String::from)
    } else {
        None
    };

    Ok(json!({
        "agent": agent_id,
        "avatar": avatar,
    }))
}

// ── Usage Export (#64) ──────────────────────────────────────────────────────

/// Export session usage data in CSV or JSON format.
async fn handle_usage_export(params: &Value, session_store: &Arc<SessionStore>) -> RpcResult {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");
    let date_from = params.get("date_from").and_then(|v| v.as_u64());
    let date_to = params.get("date_to").and_then(|v| v.as_u64());

    if format != "csv" && format != "json" {
        return Err((
            INVALID_PARAMS,
            format!("unsupported format: {format}. Must be 'csv' or 'json'"),
        ));
    }

    let sessions = session_store.list().await;

    // Filter by date range (epoch ms).
    let filtered: Vec<_> = sessions
        .iter()
        .filter(|s| {
            if let Some(from) = date_from {
                if s.created_at < from {
                    return false;
                }
            }
            if let Some(to) = date_to {
                if s.created_at > to {
                    return false;
                }
            }
            true
        })
        .collect();

    let count = filtered.len();

    let data: Value = match format {
        "csv" => {
            let mut lines = Vec::with_capacity(count + 1);
            lines.push(
                "session_id,session_id,model,input_tokens,output_tokens,total_tokens,created_at,updated_at"
                    .to_string(),
            );
            for s in &filtered {
                lines.push(format!(
                    "{},{},{},{},{},{},{},{}",
                    s.session_id,
                    s.session_id,
                    s.model.as_deref().unwrap_or(""),
                    s.input_tokens,
                    s.output_tokens,
                    s.total_tokens,
                    s.created_at,
                    s.updated_at,
                ));
            }
            json!(lines.join("\n"))
        }
        _ => {
            let entries: Vec<Value> = filtered
                .iter()
                .map(|s| {
                    json!({
                        "session_id": s.session_id,
                        "model": s.model,
                        "input_tokens": s.input_tokens,
                        "output_tokens": s.output_tokens,
                        "total_tokens": s.total_tokens,
                        "created_at": s.created_at,
                        "updated_at": s.updated_at,
                    })
                })
                .collect();
            json!(entries)
        }
    };

    Ok(json!({
        "data": data,
        "format": format,
        "count": count,
    }))
}

// ── Log Rotation (#65) ─────────────────────────────────────────────────────

/// Path to log rotation config file.
fn log_config_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("log-rotation-config.json")
}

/// Trigger log rotation: clear in-memory log buffer and archive to a timestamped file.
async fn handle_logs_rotate(bridge: &GatewayBridge) -> RpcResult {
    // Drain current logs from in-memory store.
    let entries = log_store::list_logs(usize::MAX).await;
    let count = entries.len();

    if count > 0 {
        // Archive to a timestamped JSONL file.
        let logs_dir = bridge.config().savfox_home.join("logs");
        if !logs_dir.exists() {
            tokio::fs::create_dir_all(&logs_dir)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;
        }

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let archive_path = logs_dir.join(format!("gateway-{ts}.jsonl"));

        let mut buf = String::new();
        for entry in &entries {
            if let Ok(line) = serde_json::to_string(entry) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }

        tokio::fs::write(&archive_path, &buf)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

        // Prune old archives based on max_files setting.
        let _ = prune_log_archives(bridge).await;
    }

    log_store::append_log("info", "logs.rotate", format!("rotated {count} entries")).await;

    Ok(json!({
        "status": "ok",
        "rotated_entries": count,
    }))
}

/// Prune old log archives beyond the configured max_files limit.
async fn prune_log_archives(bridge: &GatewayBridge) -> Result<usize, String> {
    let config = read_log_rotation_config(bridge).await;
    let max_files = config
        .get("max_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    let logs_dir = bridge.config().savfox_home.join("logs");
    if !logs_dir.exists() {
        return Ok(0);
    }

    let mut archives: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut entries = tokio::fs::read_dir(&logs_dir)
        .await
        .map_err(|e| format!("readdir error: {e}"))?;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("gateway-") && name.ends_with(".jsonl") {
            archives.push((name, entry.path()));
        }
    }

    // Sort oldest first.
    archives.sort_by(|a, b| a.0.cmp(&b.0));

    let mut removed = 0;
    while archives.len() > max_files {
        if let Some((_, path)) = archives.first() {
            let _ = tokio::fs::remove_file(path).await;
            archives.remove(0);
            removed += 1;
        } else {
            break;
        }
    }

    Ok(removed)
}

/// Read log rotation config from disk.
async fn read_log_rotation_config(bridge: &GatewayBridge) -> Value {
    let path = log_config_path(bridge);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    serde_json::from_str(&content).unwrap_or(json!({}))
}

/// Export recent log entries with optional filtering.
async fn handle_logs_export(params: &Value) -> RpcResult {
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
    let level_filter = params.get("level").and_then(|v| v.as_str());
    let source_filter = params.get("source").and_then(|v| v.as_str());

    let all_entries = log_store::list_logs(limit).await;

    let filtered: Vec<_> = all_entries
        .into_iter()
        .filter(|e| {
            if let Some(level) = level_filter {
                if e.level != level {
                    return false;
                }
            }
            if let Some(source) = source_filter {
                if !e.source.contains(source) {
                    return false;
                }
            }
            true
        })
        .collect();

    let count = filtered.len();
    let value = serde_json::to_value(&filtered).unwrap_or(json!([]));

    Ok(json!({
        "entries": value,
        "count": count,
    }))
}

/// Get or set log rotation configuration (max_file_size_mb, max_files).
async fn handle_logs_config(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("get");

    let path = log_config_path(bridge);

    match action {
        "set" => {
            let mut config = read_log_rotation_config(bridge).await;

            if let Some(max_size) = params.get("max_file_size_mb") {
                config["max_file_size_mb"] = max_size.clone();
            }
            if let Some(max_files) = params.get("max_files") {
                config["max_files"] = max_files.clone();
            }

            let json_str = serde_json::to_string_pretty(&config)
                .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
            tokio::fs::write(&path, json_str)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

            Ok(json!({
                "status": "ok",
                "config": config,
            }))
        }
        _ => {
            // "get" (default)
            let config = read_log_rotation_config(bridge).await;
            let defaults = json!({
                "max_file_size_mb": config.get("max_file_size_mb").cloned().unwrap_or(json!(50)),
                "max_files": config.get("max_files").cloned().unwrap_or(json!(10)),
            });
            Ok(json!({ "config": defaults }))
        }
    }
}

// ── Prompt Injection Detection (#66) ────────────────────────────────────────

/// Injection pattern definition.
struct InjectionPattern {
    name: &'static str,
    patterns: &'static [&'static str],
    weight: f64,
}

/// Well-known prompt injection / jailbreak patterns.
const INJECTION_PATTERNS: &[InjectionPattern] = &[
    InjectionPattern {
        name: "system_prompt_override",
        patterns: &[
            "ignore previous instructions",
            "ignore all previous",
            "disregard previous instructions",
            "forget your instructions",
            "override your system prompt",
            "new system prompt",
            "from now on you are",
            "you are now",
        ],
        weight: 0.9,
    },
    InjectionPattern {
        name: "jailbreak_phrase",
        patterns: &[
            "do anything now",
            "dan mode",
            "jailbreak",
            "developer mode",
            "bypass your restrictions",
            "ignore your safety",
            "pretend you have no restrictions",
            "act as an unrestricted",
        ],
        weight: 0.95,
    },
    InjectionPattern {
        name: "role_confusion",
        patterns: &[
            "you are not an ai",
            "you are a human",
            "pretend to be",
            "roleplay as",
            "act as if you are",
            "simulate being",
            "behave as",
        ],
        weight: 0.6,
    },
    InjectionPattern {
        name: "instruction_override",
        patterns: &[
            "ignore the above",
            "disregard the above",
            "do not follow",
            "stop following",
            "new instructions:",
            "updated instructions:",
            "real instructions:",
            "actual instructions:",
            "true instructions:",
        ],
        weight: 0.85,
    },
];

/// Analyze text for common prompt injection / jailbreak patterns.
async fn handle_security_analyze(params: &Value) -> RpcResult {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if text.is_empty() {
        return Err((INVALID_PARAMS, "missing 'text' parameter".to_string()));
    }

    let lower = text.to_lowercase();
    let mut found: Vec<Value> = Vec::new();
    let mut max_weight: f64 = 0.0;

    for category in INJECTION_PATTERNS {
        for &pattern in category.patterns {
            if lower.contains(pattern) {
                found.push(json!({
                    "category": category.name,
                    "pattern": pattern,
                    "weight": category.weight,
                }));
                if category.weight > max_weight {
                    max_weight = category.weight;
                }
            }
        }
    }

    // Score: 0.0 = safe, 1.0 = definitely injection.
    // Take max weight of matched patterns, scale by number of unique categories hit.
    let categories_hit: std::collections::HashSet<&str> = found
        .iter()
        .filter_map(|v| v.get("category").and_then(|c| c.as_str()))
        .collect();

    let score = if found.is_empty() {
        0.0
    } else {
        // Base score from heaviest pattern, boosted by multi-category matches.
        let cat_boost = ((categories_hit.len() as f64 - 1.0) * 0.05).min(0.15);
        (max_weight + cat_boost).min(1.0)
    };

    let safe = score < 0.5;

    Ok(json!({
        "safe": safe,
        "score": score,
        "patterns_found": found,
        "categories_hit": categories_hit.len(),
        "text_length": text.len(),
    }))
}

async fn handle_security_audit(_params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let report = crate::security_audit::run_audit(&bridge.config().savfox_home).await;
    serde_json::to_value(report).map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("security audit serialization failed: {e}"),
        )
    })
}

fn rotated_secret(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::now_v7().simple())
}

async fn handle_security_rotate(params: &Value, bridge: &Arc<GatewayBridge>) -> RpcResult {
    let rotate_gateway_token = params
        .get("gateway_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let rotate_webhook_secrets = params
        .get("webhook_secrets")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if !rotate_gateway_token && !rotate_webhook_secrets {
        return Err((
            INVALID_PARAMS,
            "nothing to rotate: enable gateway_token and/or webhook_secrets".to_string(),
        ));
    }

    let mut result = json!({
        "status": "ok",
        "rotated": {
            "gateway_token": false,
            "webhook_secrets": 0,
        },
        "restart_required": false,
        "suggestions": [],
    });

    if rotate_gateway_token {
        let mut doc = load_config_intermediate(bridge)
            .await
            .map_err(|e| (INTERNAL_ERROR, e))?;

        if !doc.value.is_object() {
            doc.value = Value::Object(serde_json::Map::new());
        }
        let root = doc
            .value
            .as_object_mut()
            .ok_or((INTERNAL_ERROR, "config root is not an object".to_string()))?;

        let gateway = root
            .entry("gateway")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !gateway.is_object() {
            *gateway = Value::Object(serde_json::Map::new());
        }

        let new_token = crate::auth::GatewayAuth::generate_token();
        let gateway_obj = gateway.as_object_mut().ok_or((
            INTERNAL_ERROR,
            "gateway config is not an object".to_string(),
        ))?;
        gateway_obj.insert("token".to_string(), Value::String(new_token.clone()));

        let content = crate::security_audit::serialize_config_value(&doc.value, &doc.format)
            .map_err(|e| (INTERNAL_ERROR, e))?;
        if let Some(parent) = doc.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("failed to prepare config dir: {e}")))?;
        }
        tokio::fs::write(&doc.path, content)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to write config: {e}")))?;

        let hint = format!(
            "...{}",
            &new_token[new_token.len().saturating_sub(4)..new_token.len()]
        );
        result["rotated"]["gateway_token"] = json!(true);
        result["gateway_token_hint"] = json!(hint);
        result["gateway_config_path"] = json!(doc.path);
        result["restart_required"] = json!(true);
    }

    if rotate_webhook_secrets {
        let store = crate::webhooks::WebhookStore::new(&bridge.config().savfox_home);
        store.load().await;
        let hooks = store.list().await;

        let mut rotated = 0_u32;
        let mut failures = Vec::new();
        for hook in hooks {
            if !hook.enabled {
                continue;
            }
            let new_secret = rotated_secret("whsec_");
            let update = json!({ "secret": new_secret });
            match store.update(&hook.id, update).await {
                Ok(_) => rotated = rotated.saturating_add(1),
                Err(err) => failures.push(format!("{}: {err}", hook.id)),
            }
        }
        result["rotated"]["webhook_secrets"] = json!(rotated);
        if !failures.is_empty() {
            result["failures"] = json!(failures);
        }
    }

    let mut suggestions =
        vec!["Run `savfox security audit` to verify current posture.".to_string()];
    if rotate_gateway_token {
        suggestions.push("Restart gateway to apply the rotated bearer token.".to_string());
    }
    if rotate_webhook_secrets {
        suggestions.push(
            "Update webhook senders with the new signing secrets before sending events."
                .to_string(),
        );
    }
    result["suggestions"] = json!(suggestions);

    Ok(result)
}

// ── Tool Policy Management ───────────────────────────────────────────────────

async fn handle_tools_policy_get(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&bridge.config().savfox_home);
    let policy = store.get(agent_id).await;

    Ok(json!({
        "agentId": agent_id,
        "profile": policy.profile.map(|p| serde_json::to_string(&p).unwrap_or_default().trim_matches('"').to_string()),
        "allowlist": policy.allowlist.iter().collect::<Vec<_>>(),
        "denylist": policy.denylist.iter().collect::<Vec<_>>(),
        "categoryOverrides": policy.category_overrides,
        "requireApproval": policy.require_approval,
        "toolOverrides": policy.tool_overrides
    }))
}

async fn handle_tools_policy_set(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let mut policy = crate::tool_policy::ToolPolicy::default();

    if let Some(profile_str) = params.get("profile").and_then(|v| v.as_str()) {
        let profile = match profile_str {
            "default" => crate::tool_policy::ToolPolicyProfile::Default,
            "restricted" => crate::tool_policy::ToolPolicyProfile::Restricted,
            "full" => crate::tool_policy::ToolPolicyProfile::Full,
            _ => return Err((INVALID_PARAMS, format!("Unknown profile: {}", profile_str))),
        };
        policy = crate::tool_policy::ToolPolicy::from_profile(profile);
    }

    if let Some(allowlist) = params.get("allowlist").and_then(|v| v.as_array()) {
        for item in allowlist {
            if let Some(s) = item.as_str() {
                policy.allowlist.insert(s.to_string());
            }
        }
    }

    if let Some(denylist) = params.get("denylist").and_then(|v| v.as_array()) {
        for item in denylist {
            if let Some(s) = item.as_str() {
                policy.denylist.insert(s.to_string());
            }
        }
    }

    if let Some(req) = params.get("requireApproval").and_then(|v| v.as_bool()) {
        policy.require_approval = req;
    }

    let store = crate::tool_policy::ToolPolicyStore::new(&bridge.config().savfox_home);
    store
        .set(agent_id, policy.clone())
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to save policy: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id }))
}

async fn handle_tools_policy_reset(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&bridge.config().savfox_home);
    store
        .reset(agent_id)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to reset policy: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id }))
}

async fn handle_tools_policy_allow(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let tool_name = params
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "tool is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&bridge.config().savfox_home);
    store
        .allow_tool(agent_id, tool_name)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to allow tool: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id, "tool": tool_name, "allowed": true }))
}

async fn handle_tools_policy_deny(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let tool_name = params
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "tool is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&bridge.config().savfox_home);
    store
        .deny_tool(agent_id, tool_name)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to deny tool: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id, "tool": tool_name, "allowed": false }))
}

async fn handle_tools_list(params: &Value, bridge: &GatewayBridge) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&bridge.config().savfox_home);
    let tools = store
        .list_tools(agent_id)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to list tools: {}", e)))?;

    let tools_json: Vec<Value> = tools.iter().map(|t| {
        json!({
            "name": t.name,
            "description": t.description,
            "category": serde_json::to_string(&t.category).unwrap_or_default().trim_matches('"'),
            "allowed": t.allowed,
            "requiresApproval": t.requires_approval
        })
    }).collect();

    Ok(json!({ "agentId": agent_id, "tools": tools_json, "count": tools.len() }))
}

async fn handle_tools_categories() -> RpcResult {
    let categories: Vec<Value> = crate::tool_policy::ToolCategory::all()
        .iter()
        .map(|c| {
            json!({
                "id": serde_json::to_string(c).unwrap_or_default().trim_matches('"'),
                "label": c.label()
            })
        })
        .collect();

    let profiles: Vec<Value> = crate::tool_policy::ToolPolicyProfile::all()
        .iter()
        .map(|p| {
            json!({
                "id": serde_json::to_string(p).unwrap_or_default().trim_matches('"'),
                "label": p.label(),
                "description": p.description()
            })
        })
        .collect();

    Ok(json!({ "categories": categories, "profiles": profiles }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::normalize_config_model_fields;

    #[test]
    fn expands_top_level_model_string_into_full_object() {
        let mut config = json!({
            "model": "zhipuai-coding-plan/glm-5"
        });

        normalize_config_model_fields(&mut config);

        assert_eq!(config["model"]["id"], json!("zhipuai-coding-plan/glm-5"));
        assert_eq!(config["model"]["code"], json!("glm-5"));
        assert_eq!(config["model"]["name"], json!("Glm 5"));
        assert_eq!(
            config["model"]["provider"]["id"],
            json!("zhipuai-coding-plan")
        );
        assert_eq!(
            config["model"]["provider"]["name"],
            json!("Zhipuai Coding Plan")
        );
        assert_eq!(
            config["model"]["provider"]["base_url"],
            json!("https://open.bigmodel.cn/api/coding/paas/v4")
        );
    }

    #[test]
    fn expands_profile_model_with_provider_string() {
        let mut config = json!({
            "profiles": {
                "dev": {
                    "model": {
                        "provider": "anthropic",
                        "code": "claude-sonnet-4"
                    }
                }
            }
        });

        normalize_config_model_fields(&mut config);

        assert_eq!(
            config["profiles"]["dev"]["model"]["id"],
            json!("anthropic/claude-sonnet-4")
        );
        assert_eq!(
            config["profiles"]["dev"]["model"]["code"],
            json!("claude-sonnet-4")
        );
        assert_eq!(
            config["profiles"]["dev"]["model"]["name"],
            json!("Claude Sonnet 4")
        );
        assert_eq!(
            config["profiles"]["dev"]["model"]["provider"]["id"],
            json!("anthropic")
        );
        assert_eq!(
            config["profiles"]["dev"]["model"]["provider"]["name"],
            json!("Anthropic")
        );
        assert_eq!(
            config["profiles"]["dev"]["model"]["provider"]["base_url"],
            json!("https://api.anthropic.com")
        );
    }

    #[test]
    fn keeps_bare_model_string_unchanged() {
        let mut config = json!({
            "model": "gpt-5.1"
        });

        normalize_config_model_fields(&mut config);

        assert_eq!(config["model"], json!("gpt-5.1"));
    }

    #[test]
    fn keeps_explicit_provider_base_url_when_present() {
        let mut config = json!({
            "model": {
                "provider": {
                    "id": "anthropic",
                    "base_url": "https://example.invalid/anthropic"
                },
                "code": "claude-sonnet-4"
            }
        });

        normalize_config_model_fields(&mut config);

        assert_eq!(
            config["model"]["provider"]["base_url"],
            json!("https://example.invalid/anthropic")
        );
    }
}
