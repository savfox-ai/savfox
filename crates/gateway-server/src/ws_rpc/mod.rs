use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use savfox_core::auth::{CLIENT_ID, login_with_api_key};
use savfox_core::{AuthManager, SavfoxAuth};
use savfox_login_oauth::{
    ServerOptions, ShutdownHandle, complete_device_code_login, request_device_code,
    run_login_server,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::{GatewayAuth, TokenInfo, has_scope, required_scope};
use crate::channel::GatewayChannel;
use crate::cron_service::CronService;
use crate::home_paths::heartbeat_config_path as gateway_heartbeat_config_path;
use crate::session::{GatewaySessionManager, SessionStore};

mod handlers;
mod types;
mod utils;
use self::handlers::channel_management::{
    handle_channels_config_delete, handle_channels_config_get, handle_channels_config_list,
    handle_channels_config_save, handle_channels_nostr_profile_export,
    handle_channels_nostr_profile_get, handle_channels_nostr_profile_import,
    handle_channels_nostr_profile_set, handle_channels_nostr_relays_get,
    handle_channels_nostr_relays_set, handle_web_login_start, handle_web_login_wait,
};
use self::types::{
    INTERNAL_ERROR, INVALID_REQUEST, JsonRpcRequest, METHOD_NOT_FOUND, PARSE_ERROR,
    PERMISSION_DENIED, RpcResult,
};
use self::utils::{rpc_error, rpc_success};

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

    // M17: destructure rather than clone. The previous `request.id.clone()`
    // and `request.params.clone()` walked the entire `serde_json::Value`
    // tree twice on every dispatch even though `request` was about to be
    // dropped at the end of the function.
    let JsonRpcRequest {
        jsonrpc: _,
        id,
        method,
        params,
    } = request;
    let params = params.unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    // ── Scope check ──────────────────────────────────────────────────
    let scope = required_scope(&method);
    if !has_scope(token_info, &scope) {
        return rpc_error(
            id,
            PERMISSION_DENIED,
            format!("permission denied: method \"{method}\" requires scope \"{scope}\""),
        );
    }

    let result = match method.as_str() {
        // ── Core ────────────────────────────────────────────────────────
        "connect" => handle_connect(&params).await,
        "health" => handle_health().await,
        "status" => handle_status(session_mgr, channel).await,
        "account/login/start" => handle_account_login_start(&params, channel).await,
        "account/login/cancel" => handle_account_login_cancel(&params).await,
        "account/read" => handle_account_read(&params, channel).await,

        // ── Agent (single-agent operations) ─────────────────────────────
        "agent" => handle_agent(&params, channel).await,
        "agent.identity" => handle_agent_identity().await,
        "agent.wait" => handle_agent_wait(&params, channel).await,
        "agent.capabilities" => handle_agent_capabilities(&params, channel).await,
        "agent.terminal.profile.list" => handle_agent_terminal_profile_list().await,
        "agent.terminal.health" => handle_agent_terminal_health(&params, channel).await,
        "agent.terminal.launch" => handle_agent_terminal_launch(&params, channel).await,
        "agent.terminal.cleanup" => handle_agent_terminal_cleanup(&params, channel).await,
        "agent.terminal.metrics" => handle_agent_terminal_metrics().await,
        "agent.terminal.pty.start" => handle_agent_terminal_pty_start(&params, channel).await,
        "agent.terminal.pty.write" => handle_agent_terminal_pty_write(&params).await,
        "agent.terminal.pty.read" => handle_agent_terminal_pty_read(&params).await,
        "agent.terminal.pty.resize" => handle_agent_terminal_pty_resize(&params).await,
        "agent.terminal.pty.close" => handle_agent_terminal_pty_close(&params).await,
        "agent.terminal.pty.list" => handle_agent_terminal_pty_list().await,
        "agent.terminal.pty.close_idle" => handle_agent_terminal_pty_close_idle().await,
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
        "agents.reset" => handle_agents_reset(&params, channel).await,
        "agents.files.list" => handle_agents_files_list(&params, channel).await,
        "agents.files.get" => handle_agents_files_get(&params, channel).await,
        "agents.files.set" => handle_agents_files_set(&params, channel).await,
        "agents.files.delete" => handle_agents_files_delete(&params, channel).await,
        "agents.skills.get" => handle_agents_skills_get(&params, channel).await,
        "agents.skills.set" => handle_agents_skills_set(&params, channel).await,

        // ── Chat ────────────────────────────────────────────────────────
        "chat.send" => handle_chat_send(&params, channel, session_mgr, session_store).await,
        "chat.history" => handle_chat_history(&params, session_store, channel).await,
        "chat.abort" => handle_chat_abort(&params, channel, session_store).await,
        "chat.inject" => handle_chat_inject(&params, session_store).await,

        // ── Sessions ────────────────────────────────────────────────────
        "sessions.list" => handle_sessions_list(session_mgr, session_store, channel).await,
        "sessions.ambient.get" => {
            handle_sessions_ambient_get(&params, session_store, channel).await
        }
        "sessions.idle_reply.get" => {
            handle_sessions_idle_reply_get(&params, session_store, channel).await
        }
        "sessions.preview" => handle_sessions_preview(&params, session_store, channel).await,
        "sessions.patch" => handle_sessions_patch(&params, session_store).await,
        "sessions.reset" => {
            handle_sessions_reset(&params, session_mgr, session_store, channel).await
        }
        "sessions.delete" => {
            handle_sessions_delete(&params, session_mgr, session_store, channel).await
        }
        "sessions.compact" => handle_sessions_compact(&params, session_store, channel).await,
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
        "channels.login" => handle_channels_login(&params, channel, session_store).await,
        "channels.logout" => handle_channels_logout(&params, channel).await,
        "channels.test" => handle_channels_test(&params, channel).await,
        "channels.matrix.invites" => handle_channels_matrix_invites(&params, channel).await,
        "channels.matrix.invite.accept" => {
            handle_channels_matrix_invite_accept(&params, channel).await
        }
        "channels.matrix.invite.reject" => {
            handle_channels_matrix_invite_reject(&params, channel).await
        }
        "channels.matrix.invite.dismiss" => {
            handle_channels_matrix_invite_dismiss(&params, channel).await
        }
        "channels.account.update" => handle_channels_account_update(&params, channel).await,
        "web.login.start" => handle_web_login_start(&params, channel, session_store).await,
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
        "tts.providers" => handle_tts_providers(channel).await,
        "tts.voices" => handle_tts_voices(&params).await,
        "tts.enable" => handle_tts_enable(&params, channel).await,
        "tts.disable" => handle_tts_disable(channel).await,
        "tts.convert" => handle_tts_convert(&params, channel).await,
        "tts.setProvider" => handle_tts_set_provider(&params, channel).await,
        "tts.setVoice" => handle_tts_set_voice(&params, channel).await,
        "tts.settings" => handle_tts_settings(&params, channel).await,

        // ── Log level ──────────────────────────────────────────────────
        "log.get_level" => handle_log_get_level().await,
        "log.set_level" => handle_log_set_level(&params).await,

        // ── Skills ──────────────────────────────────────────────────────
        "skills.status" => handle_skills_status(channel).await,
        "skills.bins" => handle_skills_bins(&params, channel).await,
        "skills.update" => handle_skills_update(&params, channel).await,
        "skills.setEnv" => handle_skills_set_env(&params, channel).await,
        "skills.install_url" => handle_skills_install_url(&params, channel).await,
        "skills.install_zip" => handle_skills_install_zip(&params, channel).await,

        // ── Exec approvals ──────────────────────────────────────────────
        "exec.approvals.get" => handle_exec_approvals_get(channel).await,
        "exec.approvals.set" => handle_exec_approvals_set(&params, channel).await,
        "exec.approvals.node.get" => handle_exec_approvals_node_get(&params, channel).await,
        "exec.approvals.node.set" => handle_exec_approvals_node_set(&params, channel).await,
        "exec.approval.request" => {
            handle_exec_approval_request(&params, channel, session_mgr).await
        }
        "exec.approval.resolve" => {
            handle_exec_approval_resolve(&params, channel, session_mgr).await
        }

        // ── Usage ───────────────────────────────────────────────────────
        "usage.status" => handle_usage_status(session_store).await,
        "usage.cost" => handle_usage_cost(&params, session_store).await,

        // ── Logs ────────────────────────────────────────────────────────
        "logs.tail" => handle_logs_tail(&params).await,

        // ── System ──────────────────────────────────────────────────────
        // The dash-style names below pre-date the `domain.action` convention
        // used elsewhere; they're kept as deprecated aliases so existing
        // Dioxus + scope-guard call sites keep working while clients migrate.
        "system.heartbeat" | "last-heartbeat" => handle_last_heartbeat(&params).await,
        "system.heartbeats.set" | "set-heartbeats" => handle_set_heartbeats(&params, channel).await,
        "system.presence" | "system-presence" => handle_system_presence(&params, session_mgr).await,
        "system.event" | "system-event" => {
            handle_system_event(&params, channel, session_mgr, cron_service).await
        }
        "system.disconnect" | "system-disconnect" => {
            handle_system_disconnect(&params, session_mgr).await
        }
        "system.kick" | "system-kick" => handle_system_kick(&params, session_mgr).await,
        "approvals.policy" => handle_approvals_policy(&params, channel).await,

        // ── Models ──────────────────────────────────────────────────────
        "models.list" => handle_models_list(&params, channel).await,
        "models.test" => handle_models_test(&params, channel).await,
        "models.add" => handle_models_add(&params, channel).await,
        "models.update" => handle_models_update(&params, channel).await,
        "models.delete" => handle_models_delete(&params, channel).await,
        "models.deleteAccount" => handle_models_account_delete(&params, channel).await,
        "models.setdefault" => handle_models_setdefault(&params, channel).await,
        "models.import" => handle_models_import(&params, channel).await,

        // ── Tools ───────────────────────────────────────────────────────
        "tools.invoke" => handle_tools_invoke(&params, channel).await,

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

        // ── STT (speech-to-text) ────────────────────────────────────────
        "stt.transcribe" => handle_stt_transcribe(&params, channel).await,
        "stt.providers" => handle_stt_providers().await,

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

        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    };

    match result {
        Ok(value) => rpc_success(id, value),
        Err((code, message)) => rpc_error(id, code, message),
    }
}

// ─── Method handlers ─────────────────────────────────────────────────────────

pub(crate) fn heartbeat_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    gateway_heartbeat_config_path(&channel.config().savfox_home)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct NodeInvokeRecord {
    pub(crate) request_id: String,
    pub(crate) node_id: String,
    pub(crate) method: String,
    pub(crate) status: String,
    pub(crate) result: Value,
    pub(crate) updated_at_ms: u64,
}

pub(crate) fn node_invoke_store() -> &'static Mutex<HashMap<String, NodeInvokeRecord>> {
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

pub(crate) fn gateway_auth_manager(channel: &Arc<GatewayChannel>) -> Arc<AuthManager> {
    AuthManager::shared(
        channel.config().savfox_home.clone(),
        false,
        channel.config().cli_auth_credentials_store_mode,
    )
}

fn chatgpt_server_options(channel: &Arc<GatewayChannel>) -> ServerOptions {
    ServerOptions::new(
        channel.config().savfox_home.clone(),
        CLIENT_ID.to_owned(),
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
                .to_owned();
            if api_key.is_empty() {
                return Err((INVALID_REQUEST, "missing 'apiKey' parameter".to_owned()));
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
            let mut opts = chatgpt_server_options(channel);
            if let Some(open_browser) = params.get("openBrowser").and_then(|v| v.as_bool()) {
                opts.open_browser = open_browser;
            }
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
                .to_owned(),
        )),
    }
}

async fn handle_account_login_cancel(params: &Value) -> RpcResult {
    let login_id = params
        .get("loginId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_owned();
    if login_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'loginId' parameter".to_owned()));
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

    let account = match auth_manager.auth_cached() {
        Some(SavfoxAuth::ApiKey(_)) => json!({ "type": "apiKey" }),
        Some(auth @ (SavfoxAuth::Chatgpt(_) | SavfoxAuth::ChatgptAuthTokens(_))) => {
            let mut payload = serde_json::Map::new();
            payload.insert("type".to_owned(), json!("chatgpt"));
            if let Some(email) = auth.get_account_email() {
                payload.insert("email".to_owned(), json!(email));
            }
            if let Some(plan_type) = auth.account_plan_type() {
                payload.insert("planType".to_owned(), json!(plan_type));
            }
            Value::Object(payload)
        }
        None => Value::Null,
    };

    Ok(json!({
        "account": account,
        "requiresOpenaiAuth": requires_openai_auth,
    }))
}

pub(crate) async fn save_node_invoke_result(record: NodeInvokeRecord) {
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

pub(crate) async fn get_node_invoke_result(request_id: &str) -> Option<NodeInvokeRecord> {
    let lock = node_invoke_store().lock().await;
    lock.get(request_id).cloned()
}

// ── Log level handlers ────────────────────────────────────────────────────

async fn handle_log_get_level() -> RpcResult {
    crate::log_level::get_level().map_err(|e| (INTERNAL_ERROR, e))
}

async fn handle_log_set_level(params: &Value) -> RpcResult {
    let filter = params
        .get("level")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'level' parameter".to_owned()))?;
    crate::log_level::set_level(filter).map_err(|e| (INVALID_REQUEST, e))
}

// ── Sub-module handler groups ──────────────────────────────────────────────
use self::handlers::agent::*;
use self::handlers::browser::*;
use self::handlers::channel::*;
use self::handlers::config::*;
use self::handlers::config_core::*;
use self::handlers::cron::*;
// Re-export for crate-level access (used by lib.rs)
pub(crate) use self::handlers::model::inject_all_provider_auth;
use self::handlers::model::*;
use self::handlers::node::*;
use self::handlers::session::*;
use self::handlers::skill::*;
use self::handlers::system::*;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::{
        canonical_channel_platform, cron_job_summary_value, cron_param_job_id,
        cron_run_summary_value, cron_status_summary_value, default_agent_config_from_source,
        enrich_model_reasoning_metadata, normalize_agent_config, normalize_config_model_fields,
        normalized_agent_name_key, saved_channel_config_ready, saved_channel_state,
        started_saved_channel_count,
    };

    fn named_channel_config(
        id: &str,
        kind: &str,
        enabled: bool,
        config: serde_json::Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        savfox_core::config::channel_store::ChannelConfig {
            id: id.to_owned(),
            kind: kind.to_owned(),
            slug: String::new(),
            name: format!("{kind}-test"),
            enabled,
            config,
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn channel_config(
        kind: &str,
        enabled: bool,
        config: serde_json::Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        named_channel_config(&format!("{kind}-test"), kind, enabled, config)
    }

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

    #[test]
    fn canonicalizes_lark_platform_alias() {
        assert_eq!(canonical_channel_platform("lark"), "feishu");
        assert_eq!(canonical_channel_platform("Feishu"), "feishu");
    }

    #[test]
    fn lark_saved_config_counts_as_feishu_state() {
        let saved = vec![channel_config(
            "lark",
            true,
            json!({
                "app_id": "cli_lark",
                "app_secret": "secret_lark"
            }),
        )];

        let state = saved_channel_state(&saved, "feishu");
        assert!(state.exists);
        assert!(state.enabled);
        assert!(state.ready);
        assert_eq!(state.channel_name.as_deref(), Some("lark-test"));
    }

    #[test]
    fn feishu_saved_config_requires_complete_auth() {
        let incomplete = channel_config(
            "feishu",
            true,
            json!({
                "app_id": "cli_incomplete"
            }),
        );
        let complete = channel_config(
            "feishu",
            true,
            json!({
                "app_id": "cli_complete",
                "app_secret": "secret_complete"
            }),
        );

        assert!(!saved_channel_config_ready(&incomplete));
        assert!(saved_channel_config_ready(&complete));
    }

    #[test]
    fn matrix_saved_config_requires_user_id_when_using_password() {
        let missing_user_id = channel_config(
            "matrix",
            true,
            json!({
                "password": "secret-password"
            }),
        );
        let complete = channel_config(
            "matrix",
            true,
            json!({
                "userId": "@savfox:matrix.org",
                "password": "secret-password"
            }),
        );

        assert!(!saved_channel_config_ready(&missing_user_id));
        assert!(saved_channel_config_ready(&complete));
    }

    #[test]
    fn started_saved_channel_count_only_includes_ready_started_entries() {
        let saved = vec![
            named_channel_config(
                "matrix-running",
                "matrix",
                true,
                json!({ "accessToken": "token-1" }),
            ),
            named_channel_config(
                "matrix-disabled",
                "matrix",
                false,
                json!({ "accessToken": "token-2" }),
            ),
            named_channel_config("matrix-incomplete", "matrix", true, json!({})),
            named_channel_config(
                "telegram-running",
                "telegram",
                true,
                json!({ "bot_token": "telegram-token" }),
            ),
        ];
        let started = HashSet::from([
            "matrix-running".to_owned(),
            "matrix-disabled".to_owned(),
            "matrix-incomplete".to_owned(),
        ]);

        assert_eq!(started_saved_channel_count("matrix", &saved, &started), 1);
    }

    #[test]
    fn normalized_agent_name_key_is_case_insensitive() {
        assert_eq!(
            normalized_agent_name_key("  Savvy fox  "),
            Some("savvy fox".into())
        );
    }

    #[test]
    fn normalize_agent_config_populates_default_agent_fields() {
        let mut config = json!({
            "kind": "native",
            "native": {
                "provider": "volcengine",
                "model": "volcengine/doubao-seed-2.0-code",
                "fallback_models": ["openai/gpt-5-mini"]
            }
        });

        normalize_agent_config(&mut config, "default", true);

        assert_eq!(config["id"], json!("default"));
        assert_eq!(config["name"], json!("Savvy fox"));
        assert_eq!(config["builtin"], json!(true));
        assert_eq!(config["status"], json!("active"));
        assert_eq!(config["kind"], json!("native"));
        assert_eq!(
            config["native"]["model"],
            json!("volcengine/doubao-seed-2.0-code")
        );
        assert_eq!(
            config["native"]["fallback_models"],
            json!(["openai/gpt-5-mini"])
        );
    }

    #[test]
    fn default_agent_config_from_source_preserves_agent_settings() {
        let source = json!({
            "id": "planner",
            "name": "Planner",
            "system_prompt": "Plan the work before execution.",
            "kind": "native",
            "native": {
                "provider": "openai",
                "model": "openai/gpt-5",
                "fallback_models": ["openai/gpt-5-mini"],
                "thinking": "medium"
            },
            "status": "idle",
            "is_default": false
        });

        let default_config = default_agent_config_from_source(&source);

        assert_eq!(default_config["id"], json!("default"));
        assert_eq!(default_config["name"], json!("Planner"));
        assert_eq!(
            default_config["system_prompt"],
            json!("Plan the work before execution.")
        );
        assert_eq!(default_config["builtin"], json!(true));
        assert_eq!(default_config["status"], json!("active"));
        assert_eq!(default_config["is_default"], json!(true));
        assert_eq!(default_config["kind"], json!("native"));
        assert_eq!(default_config["native"]["model"], json!("openai/gpt-5"));
        assert_eq!(
            default_config["native"]["fallback_models"],
            json!(["openai/gpt-5-mini"])
        );
        assert_eq!(default_config["native"]["thinking"], json!("medium"));
    }

    #[test]
    fn enrich_model_reasoning_metadata_normalizes_reasoning_aliases() {
        let mut model = json!({
            "id": "openai/gpt-5.2-savfox",
            "provider": "openai",
            "model_slug": "gpt-5.2-savfox",
            "default_reasoning_effort": "medium",
            "supported_reasoning_efforts": [
                {
                    "reasoningEffort": "low",
                    "description": "low"
                },
                {
                    "reasoning_effort": "medium",
                    "description": "medium"
                }
            ]
        });

        enrich_model_reasoning_metadata(&mut model);

        assert_eq!(model["default_reasoning_level"], json!("medium"));
        assert_eq!(
            model["supported_reasoning_levels"][0]["effort"],
            json!("low")
        );
        assert_eq!(
            model["supported_reasoning_levels"][1]["effort"],
            json!("medium")
        );
    }

    #[test]
    fn enrich_model_reasoning_metadata_synthesizes_binary_reasoning_levels() {
        let mut model = json!({
            "id": "volcengine/doubao-seed-2.0-code",
            "provider": "volcengine",
            "model_slug": "doubao-seed-2.0-code",
            "options": {
                "thinking": {
                    "type": "enabled"
                }
            }
        });

        enrich_model_reasoning_metadata(&mut model);

        assert_eq!(model["default_reasoning_level"], json!("medium"));
        assert_eq!(
            model["supported_reasoning_levels"],
            json!([
                {
                    "effort": "none",
                    "description": "Disable additional reasoning for faster responses",
                },
                {
                    "effort": "medium",
                    "description": "Enable provider-managed reasoning",
                }
            ])
        );
    }

    #[test]
    fn cron_param_job_id_accepts_legacy_alias() {
        assert_eq!(cron_param_job_id(&json!({ "id": "job-1" })), Some("job-1"));
        assert_eq!(
            cron_param_job_id(&json!({ "job_id": "job-2" })),
            Some("job-2")
        );
        assert_eq!(cron_param_job_id(&json!({ "job_id": "   " })), None);
    }

    #[test]
    fn cron_job_summary_value_matches_wire_shape() {
        let job = savfox_core::cron::CronJob {
            id: "job-1".to_owned(),
            name: "Daily Summary".to_owned(),
            agent_id: Some("writer".to_owned()),
            schedule: savfox_core::cron::CronSchedule::Every {
                interval_secs: 3_600,
                anchor_ms: 0,
            },
            payload: savfox_core::cron::CronPayload::SystemEvent {
                text: "summarize the latest notes".to_owned(),
            },
            delivery: savfox_core::cron::CronDelivery::default(),
            session_target: savfox_core::cron::CronSessionTarget::Main,
            state: savfox_core::cron::CronJobState {
                enabled: true,
                next_run_at_ms: Some(1_700_000_000_000),
                last_run_at_ms: Some(1_699_999_900_000),
                last_status: Some("ok".to_owned()),
                consecutive_errors: 0,
                main_session_id: Some(uuid::Uuid::now_v7().to_string()),
            },
            created_at_ms: 1_699_999_800_000,
        };

        let value = cron_job_summary_value(&job);

        assert_eq!(value["id"], json!("job-1"));
        assert_eq!(value["name"], json!("Daily Summary"));
        assert_eq!(value["schedule"], json!("every 1h"));
        assert_eq!(value["enabled"], json!(true));
        assert_eq!(value["agent_id"], json!("writer"));
        assert_eq!(value["session_target"], json!("main"));
        assert_eq!(value["payload"]["type"], json!("system_event"));
        assert!(value["next_run"].as_str().is_some());
        assert!(value["last_run"].as_str().is_some());
    }

    #[test]
    fn cron_summary_helpers_expose_counts_and_duration() {
        let status = savfox_core::cron::CronServiceStatus {
            enabled: true,
            total_jobs: 3,
            enabled_jobs: 2,
            running_jobs: 1,
        };
        let status_value = cron_status_summary_value(&status);
        assert_eq!(status_value["running"], json!(true));
        assert_eq!(status_value["job_count"], json!(3));

        let run = savfox_core::cron::CronRunEntry {
            job_id: "job-1".to_owned(),
            job_name: "Daily Summary".to_owned(),
            started_at_ms: 1_700_000_000_000,
            finished_at_ms: 1_700_000_015_250,
            status: "ok".to_owned(),
            error: None,
            result_preview: Some("done".to_owned()),
        };
        let run_value = cron_run_summary_value(&run);
        assert_eq!(run_value["job_id"], json!("job-1"));
        assert_eq!(run_value["status"], json!("ok"));
        assert_eq!(run_value["duration_ms"], json!(15_250));
        assert_eq!(run_value["result_preview"], json!("done"));
    }
}
