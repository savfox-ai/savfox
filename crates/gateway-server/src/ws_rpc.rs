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
use crate::channel::GatewayChannel;
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

mod types;
mod utils;
use self::types::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcRequest, METHOD_NOT_FOUND, PARSE_ERROR,
    PERMISSION_DENIED, PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE, RpcResult,
};
use self::utils::{now_ms, rpc_error, rpc_success};

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
    channel: &Arc<GatewayChannel>,
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
        "status" => handle_status(session_mgr, channel).await,
        "account/login/start" => handle_account_login_start(&params, channel).await,
        "account/login/cancel" => handle_account_login_cancel(&params).await,
        "account/read" => handle_account_read(&params, channel).await,

        // ── Agent (single-agent operations) ─────────────────────────────
        "agent" => handle_agent(&params, channel).await,
        "agent.identity" | "agent.identity.get" => handle_agent_identity().await,
        "agent.wait" => handle_agent_wait(&params, channel).await,
        "agent.capabilities" => handle_agent_capabilities(&params, channel).await,
        "agent.delegation.list" => handle_agent_delegation_list().await,
        "agent.delegation.chain" => handle_agent_delegation_chain(&params).await,
        "agent.delegation.record" => handle_agent_delegation_record(&params).await,
        "agent.delegation.remove" => handle_agent_delegation_remove(&params).await,

        // ── Agents (multi-agent CRUD) ───────────────────────────────────
        "agents.list" => handle_agents_list(channel).await,
        "agents.get" => handle_agents_get(&params, channel).await,
        "agents.create" => handle_agents_create(&params, channel).await,
        "agents.update" => handle_agents_update(&params, channel).await,
        "agents.delete" => handle_agents_delete(&params, channel).await,
        "agents.files.list" => handle_agents_files_list(&params, channel).await,
        "agents.files.get" => handle_agents_files_get(&params, channel).await,
        "agents.files.set" => handle_agents_files_set(&params, channel).await,
        "agents.files.delete" => handle_agents_files_delete(&params, channel).await,

        // ── Chat ────────────────────────────────────────────────────────
        "chat.send" => handle_chat_send(&params, channel, session_mgr, session_store).await,
        "chat.history" => handle_chat_history(&params, session_store, channel).await,
        "chat.abort" => handle_chat_abort(&params, channel, session_store).await,
        "chat.inject" => handle_chat_inject(&params, session_store).await,

        // ── Sessions ────────────────────────────────────────────────────
        "sessions.list" => handle_sessions_list(session_mgr, session_store, channel).await,
        "sessions.preview" => handle_sessions_preview(&params, session_store, channel).await,
        "sessions.patch" => handle_sessions_patch(&params, session_store).await,
        "sessions.reset" => {
            handle_sessions_reset(&params, session_mgr, session_store, channel).await
        }
        "sessions.delete" => {
            handle_sessions_delete(&params, session_mgr, session_store, channel).await
        }
        "sessions.compact" => handle_sessions_compact(&params, session_store).await,
        "sessions.overrides.get" => handle_sessions_overrides_get(&params, session_store).await,
        "sessions.overrides.set" => handle_sessions_overrides_set(&params, session_store).await,
        "sessions.identity_links.get" => handle_identity_links_get(channel).await,
        "sessions.identity_links.set" => handle_identity_links_set(&params, channel).await,
        "identity.link" => handle_identity_link(&params, channel).await,
        "sessions.dm_scope.get" => handle_dm_scope_policy_get(channel).await,
        "sessions.dm_scope.set" => handle_dm_scope_policy_set(&params, channel).await,
        "sessions.dm_scope.migrate" => handle_dm_scope_migrate(&params, session_store).await,
        "sessions.usage" => handle_sessions_usage(&params, session_store).await,
        "media.staging.list" => handle_media_staging_list(&params, channel).await,
        "media.staging.import" => handle_media_staging_import(&params, channel).await,
        "media.staging.cleanup" => handle_media_staging_cleanup(&params, channel).await,

        // ── Typing indicators ────────────────────────────────────────────
        "typing.start" => handle_typing_start(&params, session_mgr).await,
        "typing.stop" => handle_typing_stop(&params, session_mgr).await,

        // ── Events (server-push subscriptions) ──────────────────────────
        "events.subscribe" => handle_events_subscribe(&params).await,
        "events.unsubscribe" => handle_events_unsubscribe(&params).await,
        "events.list" => handle_events_list().await,

        // ── Send / Wake / Channels ──────────────────────────────────────
        "send" => handle_send(&params, channel).await,
        "send.metrics" => handle_send_metrics().await,
        "wake" => handle_wake(&params, channel).await,
        "channels.list" => handle_channels_list(channel).await,
        "channels.status" => handle_channels_status(&params, channel).await,
        "channels.login" => handle_channels_login(&params, channel).await,
        "channels.logout" => handle_channels_logout(&params, channel).await,
        "channels.test" => handle_channels_test(&params, channel).await,
        "channels.account.update" => handle_channels_account_update(&params, channel).await,
        "web.login.start" => handle_web_login_start(&params, channel).await,
        "web.login.wait" => handle_web_login_wait(&params, channel).await,
        "channels.nostr.profile.get" => handle_channels_nostr_profile_get(channel).await,
        "channels.nostr.profile.set" => handle_channels_nostr_profile_set(&params, channel).await,
        "channels.nostr.profile.import" => {
            handle_channels_nostr_profile_import(&params, channel).await
        }
        "channels.nostr.profile.export" => handle_channels_nostr_profile_export(channel).await,
        "channels.nostr.relays.get" => handle_channels_nostr_relays_get(channel).await,
        "channels.nostr.relays.set" => handle_channels_nostr_relays_set(&params, channel).await,
        "channels.config.list" => handle_channels_config_list(channel).await,
        "channels.config.get" => handle_channels_config_get(&params, channel).await,
        "channels.config.save" => handle_channels_config_save(&params, channel).await,
        "channels.config.delete" => handle_channels_config_delete(&params, channel).await,

        // ── Directory service ────────────────────────────────────────
        "directory.self" => handle_directory_self(&params, channel, session_store).await,
        "directory.peers.list" => handle_directory_peers_list(&params, session_store).await,
        "directory.groups.list" => handle_directory_groups_list(&params, session_store).await,
        "directory.groups.members" => handle_directory_groups_members(&params, session_store).await,

        // ── Config ──────────────────────────────────────────────────────
        "config.get" => handle_config_get(channel).await,
        "config.set" => handle_config_set(&params, channel).await,
        "config.apply" => handle_config_apply(&params, channel).await,
        "config.patch" => handle_config_patch(&params, channel).await,
        "config.export" => handle_config_export(&params, channel).await,
        "config.schema" => handle_config_schema().await,

        // ── Cron ────────────────────────────────────────────────────────
        "cron.list" => handle_cron_list(cron_service).await,
        "cron.status" => handle_cron_status(cron_service).await,
        "cron.add" => handle_cron_add(&params, cron_service).await,
        "cron.update" => handle_cron_update(&params, cron_service).await,
        "cron.remove" => handle_cron_remove(&params, cron_service).await,
        "cron.run" => handle_cron_run(&params, cron_service, channel).await,
        "cron.runs" => handle_cron_runs(&params, cron_service).await,

        // ── Nodes ───────────────────────────────────────────────────────
        "node.list" => handle_node_list().await,
        "node.describe" => handle_node_describe(&params).await,
        "node.capabilities.list" => handle_node_capabilities_list().await,
        "node.invoke" => handle_node_invoke(&params, channel).await,
        "node.invoke.result" => handle_node_invoke_result(&params).await,
        "node.event" => handle_node_event(&params, channel).await,
        "node.rename" => handle_node_rename(&params, channel).await,
        "node.camera.snap" => handle_node_tool_alias("camera.snap", &params, channel).await,
        "node.camera.clip" => handle_node_tool_alias("camera.clip", &params, channel).await,
        "node.screen.record" => handle_node_tool_alias("screen.record", &params, channel).await,
        "node.location.get" => handle_node_tool_alias("location.get", &params, channel).await,
        "node.notify" => handle_node_tool_alias("notify", &params, channel).await,

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
        "tts.status" => handle_tts_status(channel).await,
        "tts.providers" => handle_tts_providers().await,
        "tts.enable" => handle_tts_enable(&params, channel).await,
        "tts.disable" => handle_tts_disable(channel).await,
        "tts.convert" => handle_tts_convert(&params, channel).await,
        "tts.setProvider" => handle_tts_set_provider(&params, channel).await,

        // ── Skills ──────────────────────────────────────────────────────
        "skills.status" => handle_skills_status(channel).await,
        "skills.bins" => handle_skills_bins(channel).await,
        "skills.install" => handle_skills_install(&params, channel).await,
        "skills.update" => handle_skills_update(&params, channel).await,
        "skills.setEnv" => handle_skills_set_env(&params, channel).await,

        // ── Exec approvals ──────────────────────────────────────────────
        "exec.approvals.get" => handle_exec_approvals_get(channel).await,
        "exec.approvals.set" => handle_exec_approvals_set(&params, channel).await,
        "exec.approvals.node.get" => handle_exec_approvals_node_get(&params, channel).await,
        "exec.approvals.node.set" => handle_exec_approvals_node_set(&params, channel).await,
        "exec.approval.request" => handle_exec_approval_request(&params, channel, session_mgr).await,
        "exec.approval.resolve" => handle_exec_approval_resolve(&params, channel, session_mgr).await,

        // ── Usage ───────────────────────────────────────────────────────
        "usage.status" => handle_usage_status(session_store).await,
        "usage.cost" => handle_usage_cost(&params, session_store).await,

        // ── Logs ────────────────────────────────────────────────────────
        "logs.tail" => handle_logs_tail(&params).await,

        // ── System ──────────────────────────────────────────────────────
        "last-heartbeat" => handle_last_heartbeat(&params).await,
        "set-heartbeats" => handle_set_heartbeats(&params, channel).await,
        "system-presence" => handle_system_presence(&params, session_mgr).await,
        "system-event" => handle_system_event(&params, channel, session_mgr, cron_service).await,

        // ── Models ──────────────────────────────────────────────────────
        "models.list" => handle_models_list(&params, channel).await,
        "models.test" => handle_models_test(&params, channel).await,
        "models.add" => handle_models_add(&params, channel).await,
        "models.update" => handle_models_update(&params, channel).await,
        "models.delete" => handle_models_delete(&params, channel).await,
        "models.setdefault" => handle_models_setdefault(&params, channel).await,
        "models.import" => handle_models_import(&params, channel).await,

        // ── Tools ───────────────────────────────────────────────────────
        "tools.invoke" => handle_tools_invoke(&params, channel).await,
        "tools.policy.get" => handle_tools_policy_get(&params, channel).await,
        "tools.policy.set" => handle_tools_policy_set(&params, channel).await,
        "tools.policy.reset" => handle_tools_policy_reset(&params, channel).await,
        "tools.policy.allow" => handle_tools_policy_allow(&params, channel).await,
        "tools.policy.deny" => handle_tools_policy_deny(&params, channel).await,
        "tools.list" => handle_tools_list(&params, channel).await,
        "tools.categories" => handle_tools_categories().await,

        // ── Browser ─────────────────────────────────────────────────────
        "browser.request" => handle_browser_request(&params, channel).await,
        "browser.start" => handle_browser_start(&params, channel).await,
        "browser.stop" => handle_browser_stop(&params, channel).await,
        "browser.tabs.list" => handle_browser_tabs_list(&params, channel).await,
        "browser.tabs.open" => handle_browser_tabs_open(&params, channel).await,
        "browser.tabs.switch" => handle_browser_tabs_switch(&params, channel).await,
        "browser.tabs.close" => handle_browser_tabs_close(&params, channel).await,
        "browser.snapshot" => handle_browser_snapshot(&params, channel).await,
        "browser.storage.get" => handle_browser_storage_get(&params, channel).await,
        "browser.storage.set" => handle_browser_storage_set(&params, channel).await,
        "browser.storage.clear" => handle_browser_storage_clear(&params, channel).await,
        "browser.download" => handle_browser_download(&params, channel).await,
        "browser.network.capture" => handle_browser_network_capture(&params, channel).await,
        "browser.profiles.list" => handle_browser_profiles_list(channel).await,
        "browser.profiles.create" => handle_browser_profiles_create(&params, channel).await,
        "browser.profiles.delete" => handle_browser_profiles_delete(&params, channel).await,
        "browser.profiles.default.set" => {
            handle_browser_profiles_default_set(&params, channel).await
        }

        // ── Wizard ──────────────────────────────────────────────────────
        "wizard.start" => handle_wizard_start(&params, channel).await,
        "wizard.next" => handle_wizard_next(&params, channel).await,
        "wizard.cancel" => handle_wizard_cancel(&params, channel).await,
        "wizard.status" => handle_wizard_status(channel).await,

        // ── Memory (Markdown 4-layer system) ────────────────────────────
        "memory.list" => handle_memory_list(&params, channel).await,
        "memory.get" => handle_memory_get(&params, channel).await,
        "memory.create" => handle_memory_create(&params, channel).await,
        "memory.update" => handle_memory_update(&params, channel).await,
        "memory.delete" => handle_memory_delete(&params, channel).await,
        "memory.search" => handle_memory_search(&params, channel).await,
        "memory.promote" => handle_memory_promote(&params, channel).await,
        "memory.layers" => handle_memory_layers(channel).await,

        // ── Misc ────────────────────────────────────────────────────────
        "talk.mode" => handle_talk_mode(&params, channel).await,
        "voicewake.get" => handle_voicewake_get(channel).await,
        "voicewake.set" => handle_voicewake_set(&params, channel).await,
        "update.run" => handle_update_run(channel).await,

        // ── Webhooks ─────────────────────────────────────────────────────
        "webhooks.list" => handle_webhooks_list(channel).await,
        "webhooks.get" => handle_webhooks_get(&params, channel).await,
        "webhooks.create" => handle_webhooks_create(&params, channel).await,
        "webhooks.update" => handle_webhooks_update(&params, channel).await,
        "webhooks.delete" => handle_webhooks_delete(&params, channel).await,
        "webhooks.test" => handle_webhooks_test(&params, channel).await,

        // ── Skill Registry ──────────────────────────────────────────────
        "skills.registry.search" => handle_skills_registry_search(&params, channel).await,
        "skills.registry.install" => handle_skills_registry_install(&params, channel).await,
        "skills.registry.uninstall" => handle_skills_registry_uninstall(&params, channel).await,

        // ── Plugins ──────────────────────────────────────────────────────
        "plugins.list" => handle_plugins_list(channel).await,
        "plugins.enable" => handle_plugins_enable(&params, channel).await,
        "plugins.disable" => handle_plugins_disable(&params, channel).await,
        "plugins.config" => handle_plugins_config(&params, channel).await,

        // ── DM Policy ───────────────────────────────────────────────────
        "dm.policy.get" => handle_dm_policy_get(&params, channel).await,
        "dm.policy.set" => handle_dm_policy_set(&params, channel).await,
        "dm.allowlist.get" => handle_dm_allowlist_get(&params, channel).await,
        "dm.allowlist.set" => handle_dm_allowlist_set(&params, channel).await,

        // ── Provider Health ─────────────────────────────────────────────
        "providers.health" => handle_providers_health(channel).await,

        // ── Config Reload ───────────────────────────────────────────────
        "config.reload" => handle_config_reload(channel).await,
        "config.validate" => handle_config_validate(&params, channel).await,
        "config.migrate" => handle_config_migrate(channel).await,

        // ── STT (speech-to-text) ────────────────────────────────────────
        "stt.transcribe" => handle_stt_transcribe(&params, channel).await,
        "stt.providers" => handle_stt_providers().await,

        // ── Agent Routing ───────────────────────────────────────────────
        "routing.rules.list" => handle_routing_rules_list(channel).await,
        "routing.rules.set" => handle_routing_rules_set(&params, channel).await,

        // ── Canvas ─────────────────────────────────────────────────────
        "canvas.create" => handle_canvas_create(&params).await,
        "canvas.render" => handle_canvas_render(&params).await,
        "canvas.action" => handle_canvas_action(&params).await,
        "canvas.state" => handle_canvas_state(&params).await,
        "canvas.close" => handle_canvas_close(&params).await,

        // ── Config Snapshots (#33) ────────────────────────────────────
        "config.snapshot" => handle_config_snapshot(channel).await,
        "config.snapshots.list" => handle_config_snapshots_list(channel).await,
        "config.restore" => handle_config_restore(&params, channel).await,

        // ── Model Aliases (#34) ───────────────────────────────────────
        "models.aliases.get" => handle_models_aliases_get(channel).await,
        "models.aliases.set" => handle_models_aliases_set(&params, channel).await,
        "models.resolve" => handle_models_resolve(&params, channel).await,

        // ── Session Elevation (#46) ───────────────────────────────────
        "sessions.elevate" => handle_sessions_elevate(&params, session_store).await,
        "sessions.unelevate" => handle_sessions_unelevate(&params, session_store).await,

        // ── Heartbeat Config (#51) ────────────────────────────────────
        "heartbeat.config.get" => handle_heartbeat_config_get(channel).await,
        "heartbeat.config.set" => handle_heartbeat_config_set(&params, channel).await,

        // ── Browser CDP (#52) ─────────────────────────────────────────
        "browser.goto" => handle_browser_goto(&params, channel).await,
        "browser.click" => handle_browser_click(&params, channel).await,
        "browser.type" => handle_browser_type(&params, channel).await,
        "browser.screenshot" => handle_browser_screenshot(&params, channel).await,
        "browser.eval" => handle_browser_eval(&params, channel).await,
        "browser.extension.relay.start" => {
            handle_browser_extension_relay_start(&params, channel).await
        }
        "browser.extension.relay.status" => {
            handle_browser_extension_relay_status(&params, channel).await
        }
        "browser.extension.relay.stop" => {
            handle_browser_extension_relay_stop(&params, channel).await
        }
        "browser.extension.relay.poll" => {
            handle_browser_extension_relay_poll(&params, channel).await
        }
        "browser.extension.relay.send" => {
            handle_browser_extension_relay_send(&params, channel).await
        }
        "browser.content_script.inject" => {
            handle_browser_content_script_inject(&params, channel).await
        }
        "browser.page.extract" => handle_browser_page_extract(&params, channel).await,

        // ── Hooks Event Bus (#31) ─────────────────────────────────────
        "hooks.list" => handle_hooks_list(channel).await,
        "hooks.enable" => handle_hooks_enable(&params, channel).await,
        "hooks.disable" => handle_hooks_disable(&params, channel).await,

        // ── Message Reactions (#37) ───────────────────────────────────
        "reactions.add" => handle_reactions_add(&params, channel).await,
        "reactions.remove" => handle_reactions_remove(&params, channel).await,

        // ── Streaming Config (#36) ────────────────────────────────────
        "streaming.config.get" => handle_streaming_config_get(channel).await,
        "streaming.config.set" => handle_streaming_config_set(&params, channel).await,

        // ── YAML Config Support (#59) ────────────────────────────────
        "config.format" => handle_config_format(channel).await,
        "config.convert" => handle_config_convert(&params, channel).await,

        // ── QR Code Pairing (#62) ────────────────────────────────────
        "device.pair.qr" => handle_device_pair_qr(&params, channel).await,

        // ── Agent Avatar Management (#63) ────────────────────────────
        "agent.avatar.set" => handle_agent_avatar_set(&params, channel).await,
        "agent.avatar.get" => handle_agent_avatar_get(&params, channel).await,

        // ── Usage Export (#64) ───────────────────────────────────────
        "usage.export" => handle_usage_export(&params, session_store).await,

        // ── Log Rotation (#65) ──────────────────────────────────────
        "logs.rotate" => handle_logs_rotate(channel).await,
        "logs.export" => handle_logs_export(&params).await,
        "logs.config" => handle_logs_config(&params, channel).await,

        // ── Security (#66, #79) ──────────────────────────────────────
        "security.audit" => handle_security_audit(&params, channel).await,
        "security.rotate" => handle_security_rotate(&params, channel).await,
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

#[derive(Debug, Clone, Serialize)]
struct NodeInvokeRecord {
    request_id: String,
    node_id: String,
    method: String,
    status: String,
    result: Value,
    updated_at_ms: u64,
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

fn gateway_auth_manager(channel: &Arc<GatewayChannel>) -> Arc<AuthManager> {
    AuthManager::shared(
        channel.config().savfox_home.clone(),
        false,
        channel.config().cli_auth_credentials_store_mode,
    )
}

fn chatgpt_server_options(channel: &Arc<GatewayChannel>) -> ServerOptions {
    ServerOptions::new(
        channel.config().savfox_home.clone(),
        CLIENT_ID.to_string(),
        channel.config().forced_chatgpt_workspace_id.clone(),
        channel.config().cli_auth_credentials_store_mode,
    )
}

async fn handle_account_login_start(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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
                &channel.config().savfox_home,
                &api_key,
                channel.config().cli_auth_credentials_store_mode,
            )
            .map_err(|err| (INTERNAL_ERROR, format!("failed to save api key: {err}")))?;

            Ok(json!({ "type": "apiKey" }))
        }
        "chatgpt" => {
            let opts = chatgpt_server_options(channel);
            let server = run_login_server(opts).map_err(|err| {
                (
                    INTERNAL_ERROR,
                    format!("failed to start login server: {err}"),
                )
            })?;

            let login_id = uuid::Uuid::new_v4().to_string();
            let auth_url = server.auth_url.clone();
            let shutdown_handle = server.cancel_handle();
            let auth_manager = gateway_auth_manager(channel);
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
            let opts = chatgpt_server_options(channel);
            let device_code = request_device_code(&opts).await.map_err(|err| {
                (
                    INTERNAL_ERROR,
                    format!("failed to request device code: {err}"),
                )
            })?;

            let login_id = uuid::Uuid::new_v4().to_string();
            let verification_url = device_code.verification_url.clone();
            let user_code = device_code.user_code.clone();
            let auth_manager = gateway_auth_manager(channel);
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

async fn handle_account_read(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let refresh_token = params
        .get("refreshToken")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let auth_manager = gateway_auth_manager(channel);
    if refresh_token {
        auth_manager.reload();
    }

    let requires_openai_auth = channel.config().model_provider.requires_openai_auth;
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
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let count = session_mgr.session_count().await;
    let ids = session_mgr.session_ids().await;
    let audit_summary = crate::security_audit::run_audit(&channel.config().savfox_home)
        .await
        .summary;
    let plugins = plugin::discover_snapshot(&channel.config().savfox_home)
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

async fn handle_agent(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_string()));
    }

    match channel.invoke_agent_text(message, agent).await {
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

async fn handle_agent_wait(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_string()));
    }

    match channel.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "response": reply, "done": true })),
        Err(err) => Err((INTERNAL_ERROR, format!("agent.wait error: {err}"))),
    }
}

// ── Agent capabilities & delegation ─────────────────────────────────────────

/// Returns the capabilities of a specific agent, including its tools,
/// skills, connected channels, and current status.
async fn handle_agent_capabilities(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let agent_id = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    // Collect tools from agent config (if it exists).
    let agents_dir = channel.config().savfox_home.join("agents");
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
    let skills: Vec<String> = match skills_store::status(&channel.config().savfox_home).await {
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

    // Channels: derive from configured channel secrets.
    let channels: Vec<String> = {
        let runtime = channel.runtime_channel_secrets().await;
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
    let active_sessions = channel.websocket_manager().session_ids().await;
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
fn agents_dir(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("agents")
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

async fn resolve_agent_file_stem(channel: &GatewayChannel, agent_ref: &str) -> Option<String> {
    let dir = agents_dir(channel);
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

async fn resolve_agent_files_dir(channel: &GatewayChannel, agent_ref: &str) -> PathBuf {
    let base = agents_dir(channel);
    let safe_ref = sanitize_agent_file_stem(agent_ref).unwrap_or_else(|| "default".to_string());
    let by_ref = base.join(&safe_ref);
    if by_ref.exists() {
        return by_ref;
    }

    if let Some(stem) = resolve_agent_file_stem(channel, agent_ref).await {
        let by_stem = base.join(&stem);
        if by_stem.exists() {
            return by_stem;
        }
    }

    by_ref
}

async fn handle_agents_list(channel: &Arc<GatewayChannel>) -> RpcResult {
    let dir = agents_dir(channel);
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

async fn handle_agents_get(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let Some(file_stem) = resolve_agent_file_stem(channel, agent_ref).await else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };
    let path = agents_dir(channel).join(format!("{file_stem}.json"));
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

async fn handle_agents_create(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    if name.is_empty() {
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
        "name": name,
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

    let dir = agents_dir(channel);
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
        "name": name,
        "status": "created",
    }))
}

async fn handle_agents_update(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let dir = agents_dir(channel);
    let resolved_id = resolve_agent_file_stem(channel, agent_ref)
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

async fn handle_agents_delete(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let resolved_id = resolve_agent_file_stem(channel, agent_ref)
        .await
        .or_else(|| sanitize_agent_file_stem(agent_ref))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("invalid agent reference: {agent_ref}"),
            )
        })?;

    let dir = agents_dir(channel);
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

async fn handle_agents_files_list(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
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

async fn handle_agents_files_get(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
    let path = dir.join(safe_name);
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "content": content })),
        Err(_) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "content": null })),
    }
}

async fn handle_agents_files_set(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let path = dir.join(safe_name);

    if let Err(err) = tokio::fs::write(&path, content).await {
        return Err((INTERNAL_ERROR, format!("failed to write file: {err}")));
    }

    Ok(json!({ "agent_id": agent_ref, "path": safe_name, "status": "saved" }))
}

async fn handle_agents_files_delete(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
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
    channel: &Arc<GatewayChannel>,
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
            &channel.config().savfox_home,
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
        let candidates: Vec<String> = channel
            .session_manager()
            .list_models(channel.config(), RefreshStrategy::Offline)
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

    let mut config = (**channel.config()).clone();
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
            && channel
                .session_manager()
                .get_session(parsed_thread_id)
                .await
                .is_ok()
        {
            thread_session_id = Some(parsed_thread_id);
        }

        if thread_session_id.is_none()
            && let Ok(parsed_requested_id) = SessionId::from_string(requested)
            && channel
                .session_manager()
                .get_session(parsed_requested_id)
                .await
                .is_ok()
        {
            thread_session_id = Some(parsed_requested_id);
        }
    }

    if thread_session_id.is_none() {
        let new_thread = channel
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

    let thread = channel
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
        let _ = channel
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
        &channel.config().savfox_home,
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
    channel: &Arc<GatewayChannel>,
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

    Ok(build_history_payload(session_id, limit, source_channel, session_store, channel).await)
}

async fn handle_chat_abort(
    params: &Value,
    channel: &Arc<GatewayChannel>,
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
    if let Some(thread_id) = abort_first_active_candidate(channel.as_ref(), &candidates).await {
        return Ok(json!({ "status": "aborted", "thread_id": thread_id }));
    }

    let aborted = abort_all_active_threads(channel.as_ref()).await;
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
    channel: &Arc<GatewayChannel>,
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
            .or_else(|| entry.sender.as_ref().and_then(|s| s.name.clone()));
        if label.is_none() {
            label =
                derive_session_label_from_history(&entry.session_id, session_store, channel).await;
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
    channel: &GatewayChannel,
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
    let links = load_identity_links(&channel.config().savfox_home).await;
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
    channel: &Arc<GatewayChannel>,
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
    let staging_cleaned = MediaStore::from_home(&channel.config().savfox_home)
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
    channel: &Arc<GatewayChannel>,
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
    let staging_cleaned = MediaStore::from_home(&channel.config().savfox_home)
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

async fn handle_identity_links_get(channel: &GatewayChannel) -> RpcResult {
    let links = load_identity_links(&channel.config().savfox_home).await;
    Ok(json!({ "links": links }))
}

async fn handle_identity_links_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
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

    save_identity_links(&channel.config().savfox_home, &merged)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "updated", "count": merged.len(), "links": merged }))
}

async fn handle_identity_link(params: &Value, channel: &GatewayChannel) -> RpcResult {
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

    let mut links = load_identity_links(&channel.config().savfox_home).await;
    let summary = upsert_link(&mut links, canonical, &peers)
        .ok_or_else(|| (INVALID_PARAMS, "invalid canonical or ids".to_string()))?;
    save_identity_links(&channel.config().savfox_home, &links)
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

fn dm_scope_policy_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("dm-scope.json")
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

async fn handle_dm_scope_policy_get(channel: &GatewayChannel) -> RpcResult {
    let path = dm_scope_policy_path(channel);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let mut policy = serde_json::from_str::<DmScopePolicyConfig>(&content).unwrap_or_default();
    policy.normalize();
    Ok(json!({ "policy": policy }))
}

async fn handle_dm_scope_policy_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let raw = params
        .get("policy")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let mut policy: DmScopePolicyConfig = serde_json::from_value(raw)
        .map_err(|e| (INVALID_PARAMS, format!("invalid policy: {e}")))?;
    policy.normalize();

    let content = serde_json::to_string_pretty(&policy)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(dm_scope_policy_path(channel), content)
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
        left.name = right.name.clone().or(left.name);
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
            .or_else(|| entry.sender.as_ref().and_then(|s| s.name.clone()));
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

async fn handle_media_staging_list(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let session_id = params.get("session_id").and_then(|v| v.as_str());
    let store = MediaStore::from_home(&channel.config().savfox_home);
    let entries = store.list_staging(session_id).await;
    Ok(json!({
        "entries": entries,
        "count": entries.len(),
        "session_id": session_id,
    }))
}

async fn handle_media_staging_import(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let store = MediaStore::from_home(&channel.config().savfox_home);
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

async fn handle_media_staging_cleanup(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'session_id' parameter".to_string()));
    }

    let store = MediaStore::from_home(&channel.config().savfox_home);
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

async fn handle_send(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if channel.is_empty() || text.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'channel' or 'text' parameter".to_string(),
        ));
    }

    match channel
        .send_platform_message(channel, text, None, None, None)
        .await
    {
        Ok(()) => Ok(json!({ "status": "sent" })),
        Err(err) => Err((INTERNAL_ERROR, format!("send error: {err}"))),
    }
}

async fn handle_send_metrics() -> RpcResult {
    let metrics = crate::channels::runtime::send_metrics_snapshot().await;
    Ok(json!({ "metrics": metrics }))
}

async fn handle_wake(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    match channel.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "status": "awake", "response": reply })),
        Err(err) => Err((INTERNAL_ERROR, format!("wake error: {err}"))),
    }
}

async fn handle_channels_list(_channel: &Arc<GatewayChannel>) -> RpcResult {
    // List all supported platforms with their webhook endpoints.
    let channels = vec![
        json!({"platform": "discord", "endpoint": "/webhooks/discord", "type": "channel"}),
        json!({"platform": "dingtalk", "endpoint": "/webhooks/dingtalk", "type": "webhook"}),
        json!({"platform": "telegram", "endpoint": "/webhooks/telegram", "type": "channel"}),
        json!({"platform": "slack", "endpoint": "/webhooks/slack", "type": "channel"}),
        json!({"platform": "msteams", "endpoint": "/webhooks/msteams", "type": "channel"}),
        json!({"platform": "webhook", "endpoint": "/webhooks/webhook", "type": "generic"}),
        json!({"platform": "matrix", "endpoint": "/webhooks/matrix", "type": "webhook"}),
        json!({"platform": "mattermost", "endpoint": "/webhooks/mattermost", "type": "webhook"}),
        json!({"platform": "googlechat", "endpoint": "/webhooks/googlechat", "type": "webhook"}),
        json!({"platform": "line", "endpoint": "/webhooks/line", "type": "webhook"}),
        json!({"platform": "feishu", "endpoint": "/webhooks/feishu", "type": "channel"}),
        json!({"platform": "irc", "endpoint": "/webhooks/irc", "type": "webhook"}),
        json!({"platform": "nostr", "endpoint": "/webhooks/nostr", "type": "channel"}),
        json!({"platform": "zalo", "endpoint": "/webhooks/zalo", "type": "webhook"}),
    ];
    Ok(json!({ "channels": channels }))
}

async fn handle_channels_status(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let runtime = channel.runtime_channel_secrets().await;
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
    let health_metrics = crate::channels::runtime::channel_health_snapshot().await;
    let send_metrics = crate::channels::runtime::send_metrics_snapshot().await;
    let nostr_profile = load_nostr_profile(channel).await;
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
        "dingtalk": {
            "configured": std::env::var("DINGTALK_WEBHOOK_URL").is_ok()
                || std::env::var("DINGTALK_ACCESS_TOKEN").is_ok(),
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
        savfox_core::config::channel_store::list_channel_configs(&channel.config().savfox_home).await
        && let Some(channels_map) = channels.as_object_mut()
    {
        for saved in saved_configs {
            let key = saved.kind.to_ascii_lowercase();
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
                crate::channels::runtime::record_channel_probe(platform, &probe_status).await;
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

async fn handle_channels_login(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_string()));
    }

    let runtime = channel.runtime_channel_secrets().await;
    let saved_channel_enabled = savfox_core::config::channel_store::get_channel_config(
        &channel.config().savfox_home,
        platform,
    )
    .await
    .ok()
    .flatten()
    .is_some_and(|cfg| cfg.enabled);
    let nostr_profile = load_nostr_profile(channel).await;
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
        "dingtalk" => {
            std::env::var("DINGTALK_WEBHOOK_URL").is_ok()
                || std::env::var("DINGTALK_ACCESS_TOKEN").is_ok()
        }
        "feishu" | "lark" => {
            saved_channel_enabled
                || std::env::var("FEISHU_TENANT_ACCESS_TOKEN").is_ok()
                || std::env::var("FEISHU_APP_ACCESS_TOKEN").is_ok()
        }
        "matrix" => saved_channel_enabled || std::env::var("MATRIX_ACCESS_TOKEN").is_ok(),
        "nostr" => nostr_configured,
        _ => saved_channel_enabled,
    };

    Ok(json!({
        "platform": platform,
        "status": if is_configured { "already_configured" } else { "needs_config" },
        "configured": is_configured,
        "message": if is_configured {
            format!("{} is already configured", platform)
        } else {
            format!("Please configure {} in the channel settings", platform)
        }
    }))
}

async fn handle_channels_logout(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_string()));
    }

    let mut secrets = channel.runtime_channel_secrets().await;
    match platform {
        "discord" => secrets.discord_bot_token = None,
        "telegram" => secrets.telegram_bot_token = None,
        "slack" => {
            secrets.slack_bot_token = None;
            secrets.slack_signing_secret = None;
        }
        "webhook" => secrets.webhook_secret = None,
        "nostr" => {
            let mut profile = load_nostr_profile(channel).await;
            profile["private_key"] = json!("");
            profile["public_key"] = json!("");
            let _ = save_nostr_profile(channel, &profile).await;
        }
        "matrix" | "whatsapp" | "signal" | "mattermost" | "googlechat" | "irc" | "line"
        | "feishu" | "dingtalk" => {
            // These platforms may not have runtime secrets yet
        }
        _ => {
            return Err((INVALID_REQUEST, format!("unknown platform: {platform}")));
        }
    }
    channel.set_runtime_channel_secrets(secrets).await;

    Ok(json!({ "platform": platform, "status": "logged_out" }))
}

async fn handle_channels_test(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_string()));
    }

    let runtime = channel.runtime_channel_secrets().await;
    let saved_channel_enabled = savfox_core::config::channel_store::get_channel_config(
        &channel.config().savfox_home,
        platform,
    )
    .await
    .ok()
    .flatten()
    .is_some_and(|cfg| cfg.enabled);
    let nostr_profile = load_nostr_profile(channel).await;
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
        "dingtalk" => {
            std::env::var("DINGTALK_WEBHOOK_URL").is_ok()
                || std::env::var("DINGTALK_ACCESS_TOKEN").is_ok()
        }
        "feishu" | "lark" => {
            saved_channel_enabled
                || std::env::var("FEISHU_TENANT_ACCESS_TOKEN").is_ok()
                || std::env::var("FEISHU_APP_ACCESS_TOKEN").is_ok()
        }
        "matrix" => saved_channel_enabled || std::env::var("MATRIX_ACCESS_TOKEN").is_ok(),
        "nostr" => nostr_configured,
        "whatsapp" => true,
        _ => saved_channel_enabled,
    };

    Ok(json!({
        "platform": platform,
        "ok": configured,
        "message": if configured {
            format!("{platform} test passed")
        } else {
            format!("{platform} is not configured. Please add configuration in the channel settings.")
        }
    }))
}

async fn handle_channels_account_update(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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

    let path = channel
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
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            latest_provenance(entry)
                .map(|item| item.name.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn channel_accounts_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel
        .config()
        .savfox_home
        .join("gateway")
        .join("channel-accounts.json")
}

async fn load_channel_accounts(channel: &GatewayChannel) -> Value {
    let path = channel_accounts_path(channel);
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
    runtime: &crate::channel::RuntimeBridgeSecrets,
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
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let channels = parse_directory_channels(params)?;
    let runtime = channel.runtime_channel_secrets().await;
    let account_doc = load_channel_accounts(channel).await;
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
        let name = session_display_name(&entry).unwrap_or_else(|| peer_id.clone());
        let identity = entry.identity.clone().unwrap_or_default();
        let chat_type = entry.chat_type.clone().unwrap_or_else(|| "dm".to_string());

        if !directory_query_match(
            query.as_deref(),
            &[&channel, &peer_id, &name, &identity, &chat_type],
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
            "name": name,
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
    name: String,
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
                let name = session_display_name(&entry).unwrap_or_else(|| peer_id.clone());
                let key = format!("{channel}:{peer_id}");
                let member = members
                    .entry(key)
                    .or_insert_with(|| DirectoryMemberAccumulator {
                        channel: channel.clone(),
                        user_id: peer_id.clone(),
                        name: name.clone(),
                        sessions: 0,
                        last_seen_ms: entry.updated_at,
                    });
                member.sessions = member.sessions.saturating_add(1);
                member.last_seen_ms = member.last_seen_ms.max(entry.updated_at);
                if member.name.trim().is_empty() && !name.trim().is_empty() {
                    member.name = name;
                }
            }
            continue;
        }

        for provenance in &entry.provenance {
            let user_id = provenance.user_id.trim();
            if user_id.is_empty() {
                continue;
            }
            let name = provenance.name.trim();
            let key = format!("{channel}:{user_id}");
            let member = members
                .entry(key)
                .or_insert_with(|| DirectoryMemberAccumulator {
                    channel: channel.clone(),
                    user_id: user_id.to_string(),
                    name: if name.is_empty() {
                        user_id.to_string()
                    } else {
                        name.to_string()
                    },
                    sessions: 0,
                    last_seen_ms: entry.updated_at.max(provenance.timestamp),
                });

            member.sessions = member.sessions.saturating_add(1);
            member.last_seen_ms = member
                .last_seen_ms
                .max(entry.updated_at)
                .max(provenance.timestamp);
            if member.name == member.user_id && !name.is_empty() {
                member.name = name.to_string();
            }
        }
    }

    let mut list = members
        .into_values()
        .filter(|member| {
            directory_query_match(
                query.as_deref(),
                &[&member.channel, &member.user_id, &member.name],
            )
        })
        .map(|member| {
            json!({
                "channel": member.channel,
                "user_id": member.user_id,
                "name": member.name,
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

async fn handle_web_login_start(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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
    handle_channels_login(&json!({ "platform": platform }), channel).await
}

async fn handle_web_login_wait(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("whatsapp");

    let status = handle_channels_status(&json!({ "channel": platform }), channel).await?;
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

fn nostr_profile_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel
        .config()
        .savfox_home
        .join("gateway")
        .join("nostr-profile.json")
}

fn default_nostr_profile() -> Value {
    json!({
        "name": "",
        "about": "",
        "picture": "",
        "nip05": "",
        "public_key": "",
        "private_key": "",
        "relays": ["wss://relay.damus.io", "wss://nos.lol"],
    })
}

async fn load_nostr_profile(channel: &GatewayChannel) -> Value {
    let path = nostr_profile_path(channel);
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(default_nostr_profile)
}

async fn save_nostr_profile(channel: &GatewayChannel, profile: &Value) -> Result<(), String> {
    let path = nostr_profile_path(channel);
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

async fn handle_channels_nostr_profile_get(channel: &Arc<GatewayChannel>) -> RpcResult {
    let profile = load_nostr_profile(channel).await;
    Ok(json!({ "profile": profile }))
}

async fn handle_channels_nostr_profile_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let mut profile = load_nostr_profile(channel).await;
    if let Some(incoming) = params.get("profile").and_then(|v| v.as_object()) {
        for (key, value) in incoming {
            profile[key] = value.clone();
        }
    } else {
        for key in [
            "name",
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
    save_nostr_profile(channel, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({ "status": "saved", "profile": profile }))
}

async fn handle_channels_nostr_profile_import(
    params: &Value,
    channel: &Arc<GatewayChannel>,
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
    let mut profile = load_nostr_profile(channel).await;
    profile["private_key"] = json!(private_key);
    if let Some(public_key) = params.get("public_key").and_then(|v| v.as_str()) {
        profile["public_key"] = json!(public_key);
    }
    save_nostr_profile(channel, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({
        "status": "imported",
        "profile": profile,
    }))
}

async fn handle_channels_nostr_profile_export(channel: &Arc<GatewayChannel>) -> RpcResult {
    let profile = load_nostr_profile(channel).await;
    Ok(json!({
        "status": "exported",
        "profile": profile,
    }))
}

async fn handle_channels_nostr_relays_get(channel: &Arc<GatewayChannel>) -> RpcResult {
    let profile = load_nostr_profile(channel).await;
    let relays = profile.get("relays").cloned().unwrap_or_else(|| json!([]));
    Ok(json!({ "relays": relays }))
}

async fn handle_channels_nostr_relays_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let relays = params.get("relays").cloned().unwrap_or_else(|| json!([]));
    if !relays.is_array() {
        return Err((INVALID_REQUEST, "'relays' must be an array".to_string()));
    }
    let mut profile = load_nostr_profile(channel).await;
    profile["relays"] = relays;
    save_nostr_profile(channel, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({
        "status": "saved",
        "relays": profile.get("relays").cloned().unwrap_or_else(|| json!([])),
    }))
}

// ── Channel Config Management ─────────────────────────────────────────

async fn handle_channels_config_list(channel: &Arc<GatewayChannel>) -> RpcResult {
    use savfox_core::config::channel_store;
    let configs = channel_store::list_channel_configs(&channel.config().savfox_home)
        .await
        .map_err(|e| {
            (
                INTERNAL_ERROR,
                format!("failed to list channel configs: {e}"),
            )
        })?;
    Ok(channel_store::channel_configs_to_json(&configs))
}

async fn handle_channels_config_get(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    use savfox_core::config::channel_store;
    let channel_id = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    if channel_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'channel' parameter".to_string()));
    }
    match channel_store::get_channel_config(&channel.config().savfox_home, &channel_id).await {
        Ok(Some(config)) => Ok(json!({ "config": channel_store::channel_config_to_json(&config) })),
        Ok(None) => Ok(json!({ "config": serde_json::Value::Null })),
        Err(e) => Err((INTERNAL_ERROR, format!("failed to get channel config: {e}"))),
    }
}

async fn handle_channels_config_save(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    use savfox_core::config::channel_store;
    let channel_kind = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let fallback_name = channel_kind.to_string();
    let channel_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&fallback_name);
    let config_value = params.get("config").cloned().unwrap_or_else(|| json!({}));

    if channel_kind.is_empty() {
        return Err((INVALID_REQUEST, "missing 'channel' parameter".to_string()));
    }

    let mut patch = if config_value.is_object() {
        config_value
    } else {
        json!({})
    };
    if let Some(id) = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        patch["id"] = json!(id);
    }

    match channel_store::merge_channel_config(
        &channel.config().savfox_home,
        &channel_kind,
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

async fn handle_channels_config_delete(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    use savfox_core::config::channel_store;
    let channel_id = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    if channel_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'channel' parameter".to_string()));
    }

    match channel_store::delete_channel_config(&channel.config().savfox_home, &channel_id).await {
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
    model_slug: &str,
    provider_base_url: Option<&str>,
) -> Value {
    json!({
        "id": format!("{provider_id}/{model_slug}"),
        "code": model_slug,
        "name": humanize_hyphenated_id(model_slug),
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
            if let Some((provider_id, model_slug)) =
                savfox_core::parse_provider_prefixed_model(model_id.as_str())
            {
                *model_value = normalized_model_object(provider_id, model_slug, None);
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
                .map(|(provider_id, model_slug)| (provider_id.to_string(), model_slug.to_string()));
            let provider_base_url = model.get("provider").and_then(extract_provider_base_url);

            let provider_id = model
                .get("provider")
                .and_then(extract_provider_id)
                .or_else(|| {
                    parsed_from_id
                        .as_ref()
                        .map(|(provider, _)| provider.clone())
                });
            let model_slug = model
                .get("code")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed)
                .or_else(|| {
                    model
                        .get("model_slug")
                        .and_then(Value::as_str)
                        .and_then(non_empty_trimmed)
                })
                .or_else(|| parsed_from_id.as_ref().map(|(_, code)| code.clone()));

            if let (Some(provider_id), Some(model_slug)) = (provider_id, model_slug) {
                *model_value = normalized_model_object(
                    &provider_id,
                    &model_slug,
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

enum DetachedBridgeConfig {
    Upsert(Value),
    Delete,
}

fn take_detached_matrix_channel_config(config: &mut Value) -> Option<DetachedBridgeConfig> {
    let root = config.as_object_mut()?;
    let (matrix_value, remove_gateway) = {
        let gateway = root.get_mut("gateway")?.as_object_mut()?;
        let (matrix, remove_channels) = {
            let channels = gateway.get_mut("channels")?.as_object_mut()?;
            let matrix = channels.remove("matrix")?;
            (matrix, channels.is_empty())
        };
        if remove_channels {
            gateway.remove("channels");
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

async fn persist_detached_matrix_channel_config(
    channel: &Arc<GatewayChannel>,
    detached: DetachedBridgeConfig,
) -> Result<(), (i64, String)> {
    use savfox_core::config::channel_store;

    match detached {
        DetachedBridgeConfig::Delete => {
            // Legacy config patch only represented one matrix channel; map delete to that canonical
            // ID.
            let _ =
                channel_store::delete_channel_config(&channel.config().savfox_home, "matrix-matrix")
                    .await
                    .map_err(|e| {
                        (
                            INTERNAL_ERROR,
                            format!("failed to delete matrix channel config: {e}"),
                        )
                    })?;
            let _ = channel_store::delete_channel_config(&channel.config().savfox_home, "matrix")
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
                &channel.config().savfox_home,
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
                "gateway.channels.matrix must be an object or null".to_string(),
            ));
        }
    }

    Ok(())
}

async fn sanitize_config_before_write(
    config: &mut Value,
    channel: &Arc<GatewayChannel>,
) -> Result<(), (i64, String)> {
    normalize_model_reasoning_key(config);
    if let Some(detached_matrix) = take_detached_matrix_channel_config(config) {
        persist_detached_matrix_channel_config(channel, detached_matrix).await?;
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

async fn handle_config_get(channel: &Arc<GatewayChannel>) -> RpcResult {
    let session_count = channel.websocket_manager().session_count().await;

    let mut config_value = load_config_value_or_empty(channel).await;
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
    channel: &GatewayChannel,
) -> Result<crate::security_audit::ConfigFile, String> {
    crate::security_audit::load_config_document(&channel.config().savfox_home).await
}

fn primary_config_json_path(channel: &GatewayChannel) -> PathBuf {
    channel.config().savfox_home.join("config.json")
}

async fn load_config_value_or_empty(channel: &GatewayChannel) -> Value {
    let mut config = load_config_intermediate(channel)
        .await
        .map(|doc| doc.value)
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
    if !config.is_object() {
        config = Value::Object(serde_json::Map::new());
    }
    config
}

async fn write_config_json(channel: &GatewayChannel, config: &Value) -> Result<(), String> {
    let path = primary_config_json_path(channel);
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("JSON serialization failed: {e}"))?;
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| format!("failed to write config: {e}"))
}

async fn handle_config_export(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let mut doc = load_config_intermediate(channel)
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

async fn handle_config_set(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let config = params.get("config");
    let Some(config_value) = config else {
        return Err((INVALID_REQUEST, "missing 'config' parameter".to_string()));
    };

    let mut sanitized = config_value.clone();
    sanitize_config_before_write(&mut sanitized, channel).await?;
    write_config_json(channel, &sanitized)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "ok" }))
}

async fn handle_config_apply(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let config = params.get("config");
    let Some(config_value) = config else {
        return Err((INVALID_REQUEST, "missing 'config' parameter".to_string()));
    };

    let mut sanitized = config_value.clone();
    sanitize_config_before_write(&mut sanitized, channel).await?;
    let config_path = primary_config_json_path(channel);

    // Auto-snapshot before applying (#33)
    let _ = handle_config_snapshot(channel).await;

    // Create a backup before applying.
    if config_path.exists() {
        let backup = channel.config().savfox_home.join("config.json.bak");
        let _ = tokio::fs::copy(&config_path, &backup).await;
    }

    write_config_json(channel, &sanitized)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({
        "status": "applied",
        "note": "restart required for changes to take effect",
    }))
}

async fn handle_config_patch(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let patch = params.get("patch");
    let Some(patch_value) = patch else {
        return Err((INVALID_REQUEST, "missing 'patch' parameter".to_string()));
    };

    let mut config = load_config_value_or_empty(channel).await;

    // Merge patch fields (deep merge, null deletes keys).
    if patch_value.is_object() {
        deep_merge_patch(&mut config, patch_value);
    }

    sanitize_config_before_write(&mut config, channel).await?;
    write_config_json(channel, &config)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "patched" }))
}

// ── Config+core handlers (split from ws_rpc.rs) ───────────────────────────
include!("ws_rpc/config_and_core_handlers.rs");

// ── Browser and related handlers (split from ws_rpc.rs) ────────────────────
include!("ws_rpc/browser_and_related_handlers.rs");

// ── Hooks Event Bus (#31) ───────────────────────────────────────────────────

fn hooks_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("hooks-config.json")
}

async fn handle_hooks_list(channel: &GatewayChannel) -> RpcResult {
    let path = hooks_config_path(channel);
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

async fn handle_hooks_enable(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let hook_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if hook_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let path = hooks_config_path(channel);
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

async fn handle_hooks_disable(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let hook_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if hook_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let path = hooks_config_path(channel);
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

async fn handle_reactions_add(params: &Value, channel: &GatewayChannel) -> RpcResult {
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

async fn handle_reactions_remove(params: &Value, channel: &GatewayChannel) -> RpcResult {
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

fn streaming_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("streaming-config.json")
}

async fn handle_streaming_config_get(channel: &GatewayChannel) -> RpcResult {
    let path = streaming_config_path(channel);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let config: Value = serde_json::from_str(&content).unwrap_or(json!({}));
    Ok(json!({
        "config": config,
        "modes": ["token", "sentence", "paragraph", "complete"],
    }))
}

async fn handle_streaming_config_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
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

    let path = streaming_config_path(channel);
    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;
    Ok(json!({ "status": "ok" }))
}

// ── YAML Config Support (#59) ───────────────────────────────────────────────

/// Detect which config format is currently in use (.json, .yaml, .toml).
async fn handle_config_format(channel: &GatewayChannel) -> RpcResult {
    let home = &channel.config().savfox_home;
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
async fn handle_config_convert(params: &Value, channel: &GatewayChannel) -> RpcResult {
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
        let home = &channel.config().savfox_home;
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
async fn handle_device_pair_qr(params: &Value, channel: &GatewayChannel) -> RpcResult {
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
        &channel.config().savfox_home,
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
async fn handle_agent_avatar_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let avatar = params.get("avatar").and_then(|v| v.as_str()).unwrap_or("");
    if avatar.is_empty() {
        return Err((INVALID_PARAMS, "missing 'avatar' parameter".to_string()));
    }

    let dir = agents_dir(channel);
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
async fn handle_agent_avatar_get(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let config_path = agents_dir(channel).join(format!("{agent_id}.json"));
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
fn log_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("log-rotation-config.json")
}

/// Trigger log rotation: clear in-memory log buffer and archive to a timestamped file.
async fn handle_logs_rotate(channel: &GatewayChannel) -> RpcResult {
    // Drain current logs from in-memory store.
    let entries = log_store::list_logs(usize::MAX).await;
    let count = entries.len();

    if count > 0 {
        // Archive to a timestamped JSONL file.
        let logs_dir = channel.config().savfox_home.join("logs");
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
        let _ = prune_log_archives(channel).await;
    }

    log_store::append_log("info", "logs.rotate", format!("rotated {count} entries")).await;

    Ok(json!({
        "status": "ok",
        "rotated_entries": count,
    }))
}

/// Prune old log archives beyond the configured max_files limit.
async fn prune_log_archives(channel: &GatewayChannel) -> Result<usize, String> {
    let config = read_log_rotation_config(channel).await;
    let max_files = config
        .get("max_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    let logs_dir = channel.config().savfox_home.join("logs");
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
async fn read_log_rotation_config(channel: &GatewayChannel) -> Value {
    let path = log_config_path(channel);
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
async fn handle_logs_config(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("get");

    let path = log_config_path(channel);

    match action {
        "set" => {
            let mut config = read_log_rotation_config(channel).await;

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
            let config = read_log_rotation_config(channel).await;
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

async fn handle_security_audit(_params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let report = crate::security_audit::run_audit(&channel.config().savfox_home).await;
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

async fn handle_security_rotate(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
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
        let mut doc = load_config_intermediate(channel)
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
        let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
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

async fn handle_tools_policy_get(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&channel.config().savfox_home);
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

async fn handle_tools_policy_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
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

    let store = crate::tool_policy::ToolPolicyStore::new(&channel.config().savfox_home);
    store
        .set(agent_id, policy.clone())
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to save policy: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id }))
}

async fn handle_tools_policy_reset(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&channel.config().savfox_home);
    store
        .reset(agent_id)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to reset policy: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id }))
}

async fn handle_tools_policy_allow(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let tool_name = params
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "tool is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&channel.config().savfox_home);
    store
        .allow_tool(agent_id, tool_name)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to allow tool: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id, "tool": tool_name, "allowed": true }))
}

async fn handle_tools_policy_deny(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let tool_name = params
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "tool is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&channel.config().savfox_home);
    store
        .deny_tool(agent_id, tool_name)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("Failed to deny tool: {}", e)))?;

    Ok(json!({ "success": true, "agentId": agent_id, "tool": tool_name, "allowed": false }))
}

async fn handle_tools_list(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "agentId is required".to_string()))?;

    let store = crate::tool_policy::ToolPolicyStore::new(&channel.config().savfox_home);
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
