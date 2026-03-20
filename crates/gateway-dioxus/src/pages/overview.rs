use dioxus::prelude::*;
use serde_json::json;

use crate::api::client::fetch_json;
use crate::api::types::{
    AgentsResponse, LogEntry, LogsResponse, ModelInfo, ModelsResponse, SessionEntry,
    SessionsResponse, StatusResponse, UsageStatus,
};
use crate::api::ws::WsRpc;
use crate::components::copy_button::CopyButton;
use crate::components::onboarding::{OnboardingStep, OnboardingStepper};

#[derive(Clone, Debug, serde::Deserialize)]
#[allow(dead_code)]
struct HealthResponse {
    status: String,
    version: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[allow(dead_code)]
struct SnapshotData {
    uptime_ms: Option<u64>,
    tick_interval_ms: Option<u64>,
    connected_clients: Option<u32>,
    sessions_count: Option<u32>,
    cron_enabled: Option<bool>,
    cron_next_run: Option<u64>,
    security_audit: Option<SecurityAuditSummary>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct SecurityAuditSummary {
    critical: Option<u32>,
    warn: Option<u32>,
    info: Option<u32>,
}

/// Channel health info extracted from channels.status RPC.
#[derive(Clone, Debug)]
struct ChannelHealthInfo {
    platform: String,
    icon: String,
    connected: usize,
    total: usize,
    has_error: bool,
    channel_name: Option<String>,
    channel_id: Option<String>,
}

fn platform_icon(platform: &str) -> &'static str {
    match platform.to_lowercase().as_str() {
        "discord" => "DC",
        "telegram" => "TG",
        "slack" => "SL",
        "matrix" => "MX",
        "mattermost" => "MM",
        "googlechat" => "GC",
        "irc" => "IR",
        "webhook" => "WH",
        "line" => "LN",
        "feishu" => "FS",
        "whatsapp" => "WA",
        "signal" => "SG",
        _ => "??",
    }
}

fn platform_brand_colors(platform: &str) -> (&'static str, &'static str) {
    match platform.to_lowercase().as_str() {
        "discord" => ("#5865F2", "#ffffff"),
        "telegram" => ("#26A5E4", "#ffffff"),
        "slack" => ("#4A154B", "#ffffff"),
        "whatsapp" => ("#25D366", "#ffffff"),
        "signal" => ("#3A76F0", "#ffffff"),
        "matrix" => ("#000000", "#ffffff"),
        _ => ("var(--surface-2)", "var(--text-muted)"),
    }
}

fn extract_channel_health(raw: &serde_json::Value) -> Vec<ChannelHealthInfo> {
    let Some(channels_obj) = raw.get("channels").and_then(|c| c.as_object()) else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for (platform, info) in channels_obj {
        let is_configured = info
            .get("configured")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_running = info
            .get("running")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_connected = info
            .get("connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_error = info.get("lastError").and_then(|v| v.as_str()).is_some();

        // Count accounts if present, otherwise treat as single
        let accounts = info
            .get("accounts")
            .and_then(|a| a.as_array())
            .map(|a| a.len())
            .unwrap_or(if is_configured { 1 } else { 0 });

        let connected_count = if is_running && is_connected {
            accounts
        } else if is_running {
            // Running but not fully connected
            0
        } else {
            0
        };

        if is_configured || is_running {
            let channel_name = info
                .get("channel_name")
                .and_then(|v| v.as_str())
                .or_else(|| info.get("slug").and_then(|v| v.as_str()))
                .or_else(|| info.get("bot_username").and_then(|v| v.as_str()))
                .or_else(|| info.get("user_id").and_then(|v| v.as_str()))
                .map(|s| s.to_string());
            let channel_id = info
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            result.push(ChannelHealthInfo {
                platform: platform.clone(),
                icon: platform_icon(platform).to_string(),
                connected: connected_count,
                total: accounts.max(1),
                has_error,
                channel_name,
                channel_id,
            });
        }
    }

    result.sort_by(|a, b| a.platform.cmp(&b.platform));
    result
}

#[component]
pub fn Overview() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);

    // Live uptime counter (seconds added client-side)
    let mut uptime_offset_secs = use_signal(|| 0u64);

    // Collapsible state for recent errors
    let mut errors_expanded = use_signal(|| Option::<usize>::None);

    // Security audit detail expansion
    let mut security_expanded = use_signal(|| false);
    let mut security_report = use_signal(|| None::<serde_json::Value>);
    let mut security_loading = use_signal(|| false);

    // --- Data resources (all re-fetch on refresh_tick) ---

    let ws_config = ws.clone();
    let config = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_config.clone();
        async move { ws.call::<serde_json::Value>("config.get", None).await.ok() }
    });

    let status = use_resource(move || {
        let _t = refresh_tick();
        async move {
            fetch_json::<StatusResponse>("GET", "/api/status", None)
                .await
                .ok()
        }
    });

    let health = use_resource(move || {
        let _t = refresh_tick();
        async move {
            fetch_json::<HealthResponse>("GET", "/health", None)
                .await
                .ok()
        }
    });

    let ws_snapshot = ws.clone();
    let snapshot = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_snapshot.clone();
        async move { ws.call::<SnapshotData>("status", None).await.ok() }
    });

    let ws_models = ws.clone();
    let models = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_models.clone();
        async move { ws.call::<ModelsResponse>("models.list", None).await.ok() }
    });

    let ws_usage = ws.clone();
    let usage = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_usage.clone();
        async move { ws.call::<UsageStatus>("usage.status", None).await.ok() }
    });

    let ws_sessions = ws.clone();
    let sessions = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_sessions.clone();
        async move {
            ws.call::<SessionsResponse>("sessions.list", None)
                .await
                .ok()
        }
    });

    let ws_channels = ws.clone();
    let channels_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_channels.clone();
        async move {
            ws.call::<serde_json::Value>("channels.status", Some(json!({ "probe": false })))
                .await
                .ok()
        }
    });

    let ws_agents = ws.clone();
    let agents = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_agents.clone();
        async move { ws.call::<AgentsResponse>("agents.list", None).await.ok() }
    });

    let ws_logs = ws.clone();
    let logs_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_logs.clone();
        async move { ws.call::<LogsResponse>("logs.tail", None).await.ok() }
    });

    // --- Uptime auto-increment timer (1s interval) ---
    // We use a simple spawn-loop instead of setInterval to avoid leaked
    // closures that survive signal disposal and cause WASM panics.
    {
        use_effect(move || {
            let _snap = snapshot.read(); // re-run when snapshot changes
            uptime_offset_secs.set(0);

            spawn(async move {
                loop {
                    // sleep 1 second
                    let promise = js_sys::Promise::new(&mut |resolve, _| {
                        if let Some(win) = web_sys::window() {
                            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                                &resolve, 1000,
                            );
                        }
                    });
                    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                    uptime_offset_secs += 1;
                }
            });
        });
    }

    // --- Read all resources ---
    let config_read = config.read();
    let status_read = status.read();
    let models_read = models.read();
    let usage_read = usage.read();
    let snapshot_read = snapshot.read();
    let health_read = health.read();
    let sessions_read = sessions.read();
    let channels_read = channels_data.read();
    let logs_read = logs_data.read();
    let agents_read = agents.read();

    // --- First-use / onboarding detection ---
    // We consider it "first use" when data has loaded but both models and
    // agents lists are empty.  While data is still loading (None) we skip
    // the onboarding to avoid a flash before the real dashboard appears.
    let models_loaded = models_read.as_ref().and_then(|m| m.as_ref());
    let agents_loaded = agents_read.as_ref().and_then(|a| a.as_ref());
    let data_loaded = models_loaded.is_some() && agents_loaded.is_some();
    let no_models = models_loaded.map(|m| m.models.is_empty()).unwrap_or(true);
    let no_agents = agents_loaded.map(|a| a.agents.is_empty()).unwrap_or(true);
    let is_first_use = data_loaded && no_models && no_agents;

    let mut onboarding_dismissed = use_signal(|| false);

    // --- Derived values ---

    let version = health_read
        .as_ref()
        .and_then(|h| h.as_ref())
        .map(|h| h.version.clone())
        .or_else(|| {
            config_read.as_ref().and_then(|c| c.as_ref()).and_then(|c| {
                c.get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        })
        .unwrap_or_else(|| "-".into());

    let clients = snapshot_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .and_then(|s| s.connected_clients)
        .or_else(|| {
            status_read
                .as_ref()
                .and_then(|s| s.as_ref().map(|s| s.connected_clients))
        })
        .unwrap_or(0);

    // Active sessions from sessions.list
    let session_entries: Vec<SessionEntry> = sessions_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .map(|s| s.entries.clone())
        .unwrap_or_default();
    let active_sessions = session_entries.len();

    // Models list (sorted for stable display order)
    let mut model_list: Vec<ModelInfo> = models_read
        .as_ref()
        .and_then(|m| m.as_ref())
        .map(|m| m.models.clone())
        .unwrap_or_default();
    model_list.sort_by(|a, b| {
        let provider_a = a.provider.as_deref().unwrap_or("");
        let provider_b = b.provider.as_deref().unwrap_or("");
        match provider_a.cmp(provider_b) {
            std::cmp::Ordering::Equal => a.id.cmp(&b.id),
            other => other,
        }
    });
    let model_count = model_list.len();

    let total_tokens = usage_read
        .as_ref()
        .and_then(|u| u.as_ref())
        .and_then(|u| u.total_tokens)
        .map(|t| format_number(t))
        .unwrap_or_else(|| "-".into());
    let total_cost = usage_read
        .as_ref()
        .and_then(|u| u.as_ref())
        .and_then(|u| u.total_cost)
        .map(|c| format!("${c:.4}"))
        .unwrap_or_else(|| "-".into());

    // Uptime with live counter
    let base_uptime_ms = snapshot_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .and_then(|s| s.uptime_ms)
        .unwrap_or(0);
    let live_uptime_ms = base_uptime_ms + (uptime_offset_secs() * 1000);
    let uptime_str = if base_uptime_ms > 0 {
        format_duration_precise(live_uptime_ms)
    } else {
        "n/a".into()
    };

    let tick_interval = snapshot_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .and_then(|s| s.tick_interval_ms)
        .map(|ms| format!("{}ms", ms))
        .unwrap_or_else(|| "n/a".into());

    let cron_enabled = snapshot_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .and_then(|s| s.cron_enabled);

    let cron_status = match cron_enabled {
        Some(true) => "Enabled",
        Some(false) => "Disabled",
        None => "n/a",
    };
    let cron_color = match cron_enabled {
        Some(true) => "var(--success)",
        Some(false) => "var(--text-muted)",
        None => "var(--text-secondary)",
    };

    let cron_next = snapshot_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .and_then(|s| s.cron_next_run)
        .map(|ts| format_next_run(ts))
        .unwrap_or_else(|| "n/a".into());

    // Security audit
    let security = snapshot_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .and_then(|s| s.security_audit.as_ref());

    let critical = security.and_then(|s| s.critical).unwrap_or(0);
    let warn = security.and_then(|s| s.warn).unwrap_or(0);
    let info_count = security.and_then(|s| s.info).unwrap_or(0);

    let (security_label, security_color) = if critical > 0 {
        (format!("{} critical", critical), "var(--danger)")
    } else if warn > 0 {
        (format!("{} warnings", warn), "var(--warning)")
    } else {
        ("No critical issues".into(), "var(--success)")
    };

    let connected = ws_connected();
    let conn_color = if connected {
        "var(--success)"
    } else {
        "var(--danger)"
    };
    let conn_label = if connected {
        "Connected"
    } else {
        "Disconnected"
    };

    // Channel health summary
    let channel_health: Vec<ChannelHealthInfo> = channels_read
        .as_ref()
        .and_then(|c| c.as_ref())
        .map(|c| extract_channel_health(c))
        .unwrap_or_default();
    let healthy_channels = channel_health
        .iter()
        .filter(|ch| !ch.has_error && ch.connected == ch.total)
        .count();
    let channel_chip_label = if channel_health.is_empty() {
        "No channels wired".to_string()
    } else {
        format!(
            "{healthy_channels}/{} channels glowing",
            channel_health.len()
        )
    };

    // Recent errors (last 5 error-level entries)
    let all_logs: Vec<LogEntry> = logs_read
        .as_ref()
        .and_then(|l| l.as_ref())
        .map(|l| l.entries.clone())
        .unwrap_or_default();
    let recent_errors: Vec<&LogEntry> = all_logs
        .iter()
        .filter(|e| {
            e.level
                .as_deref()
                .unwrap_or("")
                .eq_ignore_ascii_case("error")
        })
        .rev()
        .take(5)
        .collect();

    // Quick action: clear cache handler
    let ws_clear = ws.clone();

    rsx! {
        div { class: "page-content overview-page",
            section { class: "ov-hero",
                div { class: "ov-hero__copy",
                    span { class: "ov-hero__eyebrow", "Foxfire Control Room" }
                    h2 { class: "page-title ov-hero__title", "Overview" }
                    p { class: "ov-hero__desc",
                        "A sharper command deck for your agents, channels, and sessions. Savfox keeps the backstage hot, watchful, and ready to work."
                    }
                    div { class: "ov-hero__chips",
                        span {
                            class: if connected {
                                "ov-hero__chip ov-hero__chip--live"
                            } else {
                                "ov-hero__chip ov-hero__chip--quiet"
                            },
                            "{conn_label}"
                        }
                        span { class: "ov-hero__chip", "{model_count} models staged" }
                        span { class: "ov-hero__chip", "{active_sessions} active sessions" }
                        span { class: "ov-hero__chip", "{channel_chip_label}" }
                    }
                }
                div { class: "ov-hero__aside",
                    div { class: "ov-hero__stat",
                        span { class: "ov-hero__stat-label", "Build" }
                        strong { class: "ov-hero__stat-value", "{version}" }
                    }
                    div { class: "ov-hero__stat",
                        span { class: "ov-hero__stat-label", "Security" }
                        strong { class: "ov-hero__stat-value", style: "color:{security_color};", "{security_label}" }
                    }
                    div { class: "ov-hero__stat",
                        span { class: "ov-hero__stat-label", "Next Cron" }
                        strong { class: "ov-hero__stat-value", "{cron_next}" }
                    }
                }
            }

            // ── Onboarding stepper (first-use) ────────────────────
            if is_first_use && !onboarding_dismissed() {
                OnboardingStepper {
                    steps: vec![
                        OnboardingStep {
                            title: "Connect a Model Provider".to_string(),
                            description: "Configure an LLM provider such as OpenAI, Anthropic, or a local model so the gateway can generate responses.".to_string(),
                            completed: false,
                            action_label: "Go to Models".to_string(),
                            route: crate::route::Route::Models {},
                        },
                        OnboardingStep {
                            title: "Create Your First Agent".to_string(),
                            description: "Agents define personality, tools, and behaviour. Create one to start interacting with users.".to_string(),
                            completed: false,
                            action_label: "Go to Agents".to_string(),
                            route: crate::route::Route::Agents {},
                        },
                        OnboardingStep {
                            title: "Start a Conversation".to_string(),
                            description: "Open a session to chat with your agent and verify everything works end-to-end.".to_string(),
                            completed: false,
                            action_label: "Go to Sessions".to_string(),
                            route: crate::route::Route::Sessions {},
                        },
                    ],
                    on_dismiss: move |_| {
                        onboarding_dismissed.set(true);
                    },
                }
            } else {
                { rsx! {} }
            }

            // ── Quick action buttons ──────────────────────────────
            div { class: "ov-actions",
                button {
                    class: "ov-action-btn ov-action-btn--primary",
                    onclick: move |_| refresh_tick += 1,
                    "Refresh All"
                }
                button {
                    class: "ov-action-btn",
                    onclick: {
                        let ws = ws_clear.clone();
                        move |_| {
                            let ws = ws.clone();
                            spawn(async move {
                                let _ = ws
                                    .call::<serde_json::Value>("config.apply", None)
                                    .await;
                            });
                        }
                    },
                    "Clear Cache"
                }
                a {
                    href: "/logs",
                    class: "ov-action-btn ov-action-btn--link",
                    "View Logs"
                }
            }

            // ── Security Audit critical banner ─────────────────────
            if critical > 0 {
                div {
                    class: "ws-banner ws-banner--disconnected",
                    style: "background:var(--danger-bg);color:var(--danger);border-bottom:1px solid var(--danger);",
                    span { class: "ov-pulse-dot", style: "background:var(--danger);" }
                    {
                        let plural = if critical > 1 { "s" } else { "" };
                        format!("{critical} critical security issue{plural} detected — run security audit for details")
                    }
                }
            }

            // ── Connection status ────────────────────────────────
            div { class: "card",
                div { style: "display:flex;justify-content:space-between;align-items:center;",
                    h3 { class: "card-title", style: "margin-bottom:0;", "Connection" }
                    div { style: "display:flex;align-items:center;gap:6px;",
                        span { class: "ov-status-dot", style: "background:{conn_color};", aria_hidden: "true" }
                        span { style: "font-size:13px;color:{conn_color};font-weight:500;", "{conn_label}" }
                    }
                }
                if let Some(_sec) = security {
                    div {
                        style: "margin-top:12px;padding:10px 14px;background:rgba({security_color_bg(security_color)});border:1px solid {security_color};border-radius:var(--radius);cursor:pointer;",
                        onclick: move |_| {
                            let expanded = !security_expanded();
                            security_expanded.set(expanded);
                            if expanded && security_report.read().is_none() {
                                let ws = ws.clone();
                                spawn(async move {
                                    security_loading.set(true);
                                    if let Ok(report) = ws.call::<serde_json::Value>("security.audit", None).await {
                                        security_report.set(Some(report));
                                    }
                                    security_loading.set(false);
                                });
                            }
                        },
                        div { style: "display:flex;justify-content:space-between;align-items:center;",
                            div {
                                div { style: "font-size:13px;color:{security_color};font-weight:500;",
                                    "Security audit: {security_label}"
                                }
                                if info_count > 0 && !security_expanded() {
                                    div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;",
                                        "{info_count} info items. Click to view details."
                                    }
                                }
                            }
                            span { style: "font-size:12px;color:var(--text-muted);",
                                if security_expanded() { "Hide" } else { "Details" }
                            }
                        }
                        if security_expanded() {
                            if *security_loading.read() {
                                div { style: "margin-top:10px;font-size:12px;color:var(--text-muted);", "Loading audit report..." }
                            }
                            if let Some(ref report) = *security_report.read() {
                                div { style: "margin-top:10px;display:flex;flex-direction:column;gap:8px;",
                                    {
                                        let checks = report.get("checks")
                                            .and_then(|v| v.as_array())
                                            .cloned()
                                            .unwrap_or_default();
                                        rsx! {
                                            for (i, check) in checks.iter().enumerate() {
                                                {
                                                    let name = check.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                                    let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                                                    let details = check.get("details").and_then(|v| v.as_str()).unwrap_or("");
                                                    let suggestion = check.get("suggestion").and_then(|v| v.as_str());
                                                    let (dot_color, status_label) = match status {
                                                        "fail" => ("var(--danger)", "Critical"),
                                                        "warn" => ("var(--warning)", "Warning"),
                                                        _ => ("var(--success)", "Pass"),
                                                    };
                                                    rsx! {
                                                        div {
                                                            key: "{i}",
                                                            style: "padding:8px 10px;background:var(--bg-secondary);border-radius:var(--radius);",
                                                            div { style: "display:flex;align-items:center;gap:6px;margin-bottom:4px;",
                                                                span { style: "width:8px;height:8px;border-radius:50%;background:{dot_color};flex-shrink:0;" }
                                                                span { style: "font-size:12px;font-weight:600;", "{name}" }
                                                                span { style: "font-size:11px;color:{dot_color};", "{status_label}" }
                                                            }
                                                            div { style: "font-size:12px;color:var(--text-secondary);margin-left:14px;", "{details}" }
                                                            if let Some(suggestion) = suggestion {
                                                                div { style: "font-size:11px;color:var(--accent);margin-left:14px;margin-top:4px;", "Fix: {suggestion}" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Primary stats ────────────────────────────────────
            div { class: "stats-grid",
                { stat_card("Version", &version) }
                { stat_card_colored("Clients", &clients.to_string(), if clients > 0 { "var(--success)" } else { "var(--text-muted)" }) }
                { stat_card_colored("Active Sessions", &active_sessions.to_string(), if active_sessions > 0 { "var(--accent)" } else { "var(--text-muted)" }) }
                { stat_card("Models", &model_count.to_string()) }
                { stat_card_with_trend("Tokens Used", &total_tokens, "vs. yesterday") }
                { stat_card_with_trend("Total Cost", &total_cost, "vs. yesterday") }
            }

            // ── Uptime (live counter) + Tick + Cron ──────────────
            div { class: "ov-trio-grid",
                div { class: "stat-card ov-uptime-card",
                    div { class: "stat-label", "Uptime" }
                    div { class: "stat-value ov-uptime-value", "{uptime_str}" }
                }
                { stat_card("Tick Interval", &tick_interval) }
                div { class: "stat-card",
                    div { class: "stat-label", "Cron Jobs" }
                    div { class: "stat-value", style: "color:{cron_color};", "{cron_status}" }
                    div { style: "font-size:11px;color:var(--text-muted);margin-top:2px;", "Next: {cron_next}" }
                }
            }

            // ── Channel Health Summary ───────────────────────────
            if !channel_health.is_empty() {
                div { class: "card",
                    h3 { class: "card-title", "Channel Health" }
                    div { class: "ov-channel-grid",
                        for ch in channel_health.iter() {
                            {
                                let status_color = if ch.has_error {
                                    "var(--danger)"
                                } else if ch.connected == ch.total {
                                    "var(--success)"
                                } else if ch.connected > 0 {
                                    "var(--warning)"
                                } else {
                                    "var(--text-muted)"
                                };
                                let status_text = if ch.has_error {
                                    "Error".to_string()
                                } else if ch.connected == ch.total {
                                    "Connected".to_string()
                                } else {
                                    format!("{}/{}", ch.connected, ch.total)
                                };
                                let card_style = if ch.has_error {
                                    "cursor:pointer;border-left:3px solid var(--danger);"
                                } else {
                                    "cursor:pointer;"
                                };
                                let (icon_bg, icon_fg) = platform_brand_colors(&ch.platform);
                                let icon_style = format!("background:{icon_bg};color:{icon_fg};border-color:{icon_bg};");
                                rsx! {
                                    a { href: "/channels", style: "text-decoration:none;color:inherit;",
                                        div { class: "ov-channel-card", style: "{card_style}",
                                            div { class: "ov-channel-icon", style: "{icon_style}", "{ch.icon}" }
                                            div { class: "ov-channel-info",
                                                div { class: "ov-channel-name",
                                                    "{ch.platform}"
                                                    if let Some(ref name) = ch.channel_name {
                                                        span { style: "font-weight:400;color:var(--text-muted);margin-left:6px;font-size:11px;", "{name}" }
                                                    }
                                                }
                                                if let Some(ref id) = ch.channel_id {
                                                    div { style: "font-size:10px;color:var(--text-muted);opacity:0.7;", "{id}" }
                                                }
                                                div { class: "ov-channel-status", style: "color:{status_color};",
                                                    span { class: "ov-status-dot ov-status-dot--sm", style: "background:{status_color};", aria_hidden: "true" }
                                                    "{status_text}"
                                                }
                                            }
                                            div { class: "ov-channel-count",
                                                "{ch.connected}/{ch.total}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Model Availability ───────────────────────────────
            if !model_list.is_empty() {
                div { class: "card",
                    h3 { class: "card-title", "Model Availability" }
                    div { class: "ov-model-list",
                        for model in model_list.iter() {
                            {
                                let provider = model.provider.as_deref().unwrap_or("unknown");
                                let name = model.name.as_deref().unwrap_or(&model.id);
                                rsx! {
                                    div { class: "ov-model-row",
                                        div { class: "ov-model-name",
                                            span { class: "ov-model-id", "{name}" }
                                            span { class: "ov-model-provider-badge", "{provider}" }
                                        }
                                        div { class: "ov-model-status",
                                            span { class: "ov-status-dot ov-status-dot--sm", style: "background:var(--success);", aria_hidden: "true" }
                                            "Available"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Recent Errors ────────────────────────────────────
            div { class: "card",
                h3 { class: "card-title", "Recent Errors" }
                if recent_errors.is_empty() {
                    div { class: "ov-empty-state",
                        div { class: "ov-empty-icon", "OK" }
                        div { class: "ov-empty-text", "No recent errors" }
                    }
                } else {
                    div { class: "ov-error-list",
                        for (i, entry) in recent_errors.iter().enumerate() {
                            {
                                let ts = entry.timestamp.as_deref().unwrap_or("--");
                                let source = entry.source.as_deref().unwrap_or("");
                                let msg = &entry.message;
                                let is_expanded = errors_expanded() == Some(i);
                                // Truncate message for collapsed view
                                let short_msg = if msg.chars().count() > 80 {
                                    let truncated: String = msg.chars().take(80).collect();
                                    format!("{truncated}...")
                                } else {
                                    msg.clone()
                                };
                                let needs_expand = msg.chars().count() > 80;
                                rsx! {
                                    div {
                                        key: "{i}",
                                        class: "ov-error-item",
                                        role: "button",
                                        tabindex: "0",
                                        onclick: move |_| {
                                            if needs_expand {
                                                if is_expanded {
                                                    errors_expanded.set(None);
                                                } else {
                                                    errors_expanded.set(Some(i));
                                                }
                                            }
                                        },
                                        div { class: "ov-error-header",
                                            span { class: "ov-error-ts", "{ts}" }
                                            if !source.is_empty() {
                                                span { class: "ov-error-source", "[{source}]" }
                                            }
                                            if needs_expand {
                                                span { class: "ov-error-toggle",
                                                    if is_expanded { "[-]" } else { "[+]" }
                                                }
                                            }
                                        }
                                        div { class: "ov-error-msg",
                                            if is_expanded { "{msg}" } else { "{short_msg}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── API Endpoints ────────────────────────────────────
            div { class: "card",
                h3 { class: "card-title", "API Endpoints" }
                InfoRow { label: "Health".to_string(), value: "GET /health".to_string() }
                InfoRow { label: "Status".to_string(), value: "GET /api/status".to_string() }
                InfoRow {
                    label: "Chat (OpenAI)".to_string(),
                    value: "POST /api/chat/completions".to_string(),
                }
                InfoRow { label: "Responses".to_string(), value: "POST /api/responses".to_string() }
                InfoRow { label: "WebSocket".to_string(), value: "WS /ws".to_string() }
                InfoRow {
                    label: "Token Validate".to_string(),
                    value: "POST /api/token/validate".to_string(),
                }
            }

            // ── Quick Actions ────────────────────────────────────
            div { class: "card",
                h3 { class: "card-title", "Quick Actions" }
                div { class: "ov-quick-grid",
                    { quick_action_card("New Session", "/sessions", "+") }
                    { quick_action_card("Create Agent", "/agents", "*") }
                    { quick_action_card("Add Channel", "/channels", "#") }
                    { quick_action_card("View Logs", "/logs", ">") }
                }
            }
        }
        style { {STYLE} }
    }
}

/// Format a duration with seconds precision for the live counter.
fn format_duration_precise(ms: u64) -> String {
    let total_secs = ms / 1000;
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, minutes, secs)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, secs)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, secs)
    } else {
        format!("{}s", secs)
    }
}

fn format_next_run(ts: u64) -> String {
    // Use js_sys::Date for WASM (std::time::SystemTime panics on wasm32)
    let now = js_sys::Date::now() as u64;
    let diff = ts.saturating_sub(now);
    if diff == 0 {
        "now".into()
    } else if diff < 60000 {
        format!("{}s", diff / 1000)
    } else if diff < 3600000 {
        format!("{}m", diff / 60000)
    } else if diff < 86400000 {
        format!("{}h {}m", diff / 3600000, (diff % 3600000) / 60000)
    } else {
        format!("{}d", diff / 86400000)
    }
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn security_color_bg(color: &str) -> String {
    match color {
        "var(--danger)" => "239,68,68,0.1",
        "var(--warning)" => "245,158,11,0.1",
        _ => "34,197,94,0.1",
    }
    .into()
}

fn stat_card(label: &str, value: &str) -> Element {
    rsx! {
        div { class: "stat-card",
            div { class: "stat-label", "{label}" }
            div { class: "stat-value", "{value}" }
        }
    }
}

fn stat_card_colored(label: &str, value: &str, color: &str) -> Element {
    rsx! {
        div { class: "stat-card",
            div { class: "stat-label", "{label}" }
            div { class: "stat-value", style: "color:{color};", "{value}" }
        }
    }
}

fn stat_card_with_trend(label: &str, value: &str, trend_label: &str) -> Element {
    rsx! {
        div { class: "stat-card",
            div { class: "stat-label", "{label}" }
            div { class: "stat-value", "{value}" }
            div { class: "stat-trend", "\u{2014} {trend_label}" }
        }
    }
}

fn quick_action_card(label: &str, href: &str, icon: &str) -> Element {
    rsx! {
        a { href: "{href}", class: "ov-quick-card",
            div { class: "ov-quick-icon", "{icon}" }
            div { class: "ov-quick-label", "{label}" }
        }
    }
}

#[component]
fn InfoRow(label: String, value: String, #[props(default)] copy_value: Option<String>) -> Element {
    let copy_text = copy_value.unwrap_or_else(|| value.clone());
    rsx! {
        div { class: "info-row",
            span { class: "info-label", "{label}" }
            div { style: "display:flex;align-items:center;gap:8px;",
                span { class: "info-value", "{value}" }
                CopyButton { text: copy_text }
            }
        }
    }
}

const STYLE: &str = r#"
    .overview-page {
        /* Layout handled by .page-content */
    }

    .page-title {
        font-size: 20px;
        font-weight: 600;
        margin-bottom: 16px;
    }

    /* ── Quick action buttons ── */
    .ov-actions {
        display: flex;
        gap: 8px;
        margin-bottom: 20px;
        flex-wrap: wrap;
    }

    .ov-action-btn {
        padding: 8px 18px;
        font-size: 13px;
        font-weight: 500;
        border-radius: var(--radius);
        cursor: pointer;
        transition: background 0.15s, color 0.15s, transform 0.15s;
        border: 1px solid var(--border);
        background: var(--bg-tertiary);
        color: var(--text-secondary);
        text-decoration: none;
        display: inline-flex;
        align-items: center;
    }

    .ov-action-btn:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
        transform: translateY(-1px);
    }

    .ov-action-btn--primary {
        background: var(--accent);
        color: #fff;
        border-color: var(--accent);
    }

    .ov-action-btn--primary:hover {
        opacity: 0.9;
        background: var(--accent);
        color: #fff;
    }

    .ov-action-btn--link {
        text-decoration: none;
    }

    .ov-action-btn--link:hover {
        text-decoration: none;
    }

    /* ── Status dot ── */
    .ov-status-dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        display: inline-block;
        flex-shrink: 0;
    }

    .ov-status-dot--sm {
        width: 6px;
        height: 6px;
    }

    /* ── Stats grid ── */
    .stats-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
        gap: 12px;
        margin-bottom: 16px;
    }

    .stat-card {
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        padding: 16px;
        transition: transform 0.15s ease, box-shadow 0.15s ease;
    }

    .stat-card:hover {
        transform: translateY(-2px);
        box-shadow: 0 4px 12px rgba(0, 0, 0, 0.2);
    }

    .stat-label {
        font-size: 12px;
        color: var(--text-muted);
        margin-bottom: 4px;
    }

    .stat-value {
        font-size: 22px;
        font-weight: 600;
        font-family: var(--font-mono);
    }

    /* ── Trio grid (uptime + tick + cron) ── */
    .ov-trio-grid {
        display: grid;
        grid-template-columns: repeat(3, 1fr);
        gap: 12px;
        margin-bottom: 16px;
    }

    .ov-uptime-card {
        position: relative;
        overflow: hidden;
    }

    .ov-uptime-card::after {
        content: "";
        position: absolute;
        top: 0;
        left: 0;
        width: 3px;
        height: 100%;
        background: var(--accent);
        border-radius: 3px 0 0 3px;
    }

    .ov-uptime-value {
        font-size: 18px;
        font-variant-numeric: tabular-nums;
    }

    /* ── Channel health grid ── */
    .ov-channel-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
        gap: 10px;
    }

    .ov-channel-card {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 12px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        transition: background 0.15s;
    }

    .ov-channel-card:hover {
        background: var(--bg-hover);
    }

    .ov-channel-icon {
        width: 32px;
        height: 32px;
        border-radius: var(--radius);
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 12px;
        font-weight: 700;
        color: var(--text-muted);
        flex-shrink: 0;
    }

    .ov-channel-info {
        flex: 1;
        min-width: 0;
    }

    .ov-channel-name {
        font-size: 13px;
        font-weight: 600;
        text-transform: capitalize;
    }

    .ov-channel-status {
        font-size: 11px;
        display: flex;
        align-items: center;
        gap: 4px;
        margin-top: 2px;
    }

    .ov-channel-count {
        font-size: 13px;
        font-family: var(--font-mono);
        color: var(--text-muted);
        font-weight: 600;
    }

    /* ── Model availability ── */
    .ov-model-list {
        display: flex;
        flex-direction: column;
        gap: 0;
    }

    .ov-model-row {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 8px 0;
        border-bottom: 1px solid var(--border);
    }

    .ov-model-row:last-child {
        border-bottom: none;
    }

    .ov-model-name {
        display: flex;
        align-items: center;
        gap: 8px;
        min-width: 0;
    }

    .ov-model-id {
        font-size: 13px;
        font-weight: 500;
        font-family: var(--font-mono);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }

    .ov-model-provider-badge {
        font-size: 10px;
        padding: 1px 6px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: 8px;
        color: var(--text-muted);
        text-transform: lowercase;
        white-space: nowrap;
    }

    .ov-model-status {
        font-size: 12px;
        color: var(--success);
        display: flex;
        align-items: center;
        gap: 4px;
        flex-shrink: 0;
    }

    /* ── Recent errors ── */
    .ov-error-list {
        display: flex;
        flex-direction: column;
        gap: 0;
    }

    .ov-error-item {
        padding: 10px 0;
        border-bottom: 1px solid var(--border);
        cursor: pointer;
        transition: background 0.1s;
    }

    .ov-error-item:last-child {
        border-bottom: none;
    }

    .ov-error-item:hover {
        background: rgba(239, 68, 68, 0.03);
    }

    .ov-error-header {
        display: flex;
        align-items: center;
        gap: 8px;
        margin-bottom: 4px;
    }

    .ov-error-ts {
        font-size: 11px;
        font-family: var(--font-mono);
        color: var(--text-muted);
        white-space: nowrap;
    }

    .ov-error-source {
        font-size: 11px;
        color: var(--text-muted);
        font-family: var(--font-mono);
    }

    .ov-error-toggle {
        font-size: 11px;
        color: var(--accent);
        font-family: var(--font-mono);
        margin-left: auto;
    }

    .ov-error-msg {
        font-size: 13px;
        color: var(--danger);
        font-family: var(--font-mono);
        line-height: 1.5;
        word-break: break-word;
        white-space: pre-wrap;
    }

    /* ── Empty state ── */
    .ov-empty-state {
        display: flex;
        flex-direction: column;
        align-items: center;
        padding: 24px 16px;
        gap: 8px;
    }

    .ov-empty-icon {
        width: 40px;
        height: 40px;
        border-radius: 50%;
        background: rgba(34, 197, 94, 0.1);
        border: 1px solid var(--success);
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 12px;
        font-weight: 700;
        color: var(--success);
    }

    .ov-empty-text {
        font-size: 13px;
        color: var(--text-muted);
    }

    /* ── Card ── */
    .card {
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        padding: 20px;
        margin-bottom: 16px;
    }

    .card-title {
        font-size: 14px;
        font-weight: 600;
        margin-bottom: 12px;
        color: var(--text-secondary);
    }

    .info-row {
        display: flex;
        justify-content: space-between;
        padding: 6px 0;
        border-bottom: 1px solid var(--border);
    }

    .info-row:last-child {
        border-bottom: none;
    }

    .info-label {
        color: var(--text-secondary);
        font-size: 14px;
    }

    .info-value {
        font-family: var(--font-mono);
        font-size: 13px;
        word-break: break-all;
    }

    .quick-links {
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
    }

    .quick-link {
        padding: 6px 14px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        color: var(--text-secondary);
        text-decoration: none;
        font-size: 13px;
        transition: background 0.15s ease, color 0.15s ease, transform 0.15s ease;
    }

    .quick-link:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
        text-decoration: none;
        transform: translateY(-1px);
    }

    .quick-action {
        padding: 10px 20px;
        font-weight: 500;
    }

    /* ── Stat trend label ── */
    .stat-trend {
        font-size: 11px;
        color: var(--text-muted);
        margin-top: 4px;
        font-weight: 400;
    }

    /* ── Quick Actions 2x2 grid ── */
    .ov-quick-grid {
        display: grid;
        grid-template-columns: repeat(2, 1fr);
        gap: 10px;
    }

    .ov-quick-card {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: 8px;
        padding: 20px 12px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        text-decoration: none;
        color: var(--text-secondary);
        cursor: pointer;
        transition: background 0.15s, color 0.15s, transform 0.15s, box-shadow 0.15s;
    }

    .ov-quick-card:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
        transform: translateY(-2px);
        box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
        text-decoration: none;
    }

    .ov-quick-icon {
        width: 36px;
        height: 36px;
        border-radius: 50%;
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 16px;
        font-weight: 700;
        font-family: var(--font-mono);
        color: var(--accent);
    }

    .ov-quick-label {
        font-size: 13px;
        font-weight: 500;
    }

    /* ── Pulse dot for critical banner ── */
    .ov-pulse-dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        display: inline-block;
        flex-shrink: 0;
        margin-right: 8px;
        animation: ov-pulse 1.5s ease-in-out infinite;
    }

    @keyframes ov-pulse {
        0%, 100% { opacity: 1; }
        50% { opacity: 0.3; }
    }

    /* ── Foxfire refresh ── */
    .ov-hero {
        position: relative;
        display: grid;
        grid-template-columns: minmax(0, 1.6fr) minmax(240px, 0.8fr);
        gap: 18px;
        padding: 30px;
        margin-bottom: 22px;
        border-radius: calc(var(--radius-xl) + 4px);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 58%, var(--ornament) 42%);
        background:
            radial-gradient(circle at 16% 18%, color-mix(in srgb, var(--accent) 18%, transparent) 0%, transparent 34%),
            radial-gradient(circle at 84% 18%, color-mix(in srgb, var(--ornament) 22%, transparent) 0%, transparent 26%),
            linear-gradient(135deg, color-mix(in srgb, var(--accent) 6%, var(--surface-panel-strong) 94%) 0%, color-mix(in srgb, var(--ornament) 8%, var(--surface-panel) 92%) 100%);
        box-shadow: var(--surface-inner), var(--surface-shadow), var(--surface-glow);
        overflow: hidden;
        isolation: isolate;
        backdrop-filter: blur(var(--panel-blur)) saturate(138%);
        -webkit-backdrop-filter: blur(var(--panel-blur)) saturate(138%);
    }

    .ov-hero::before,
    .ov-hero::after {
        content: "";
        position: absolute;
        pointer-events: none;
    }

    .ov-hero::before {
        inset: auto -10% -38% 38%;
        height: 240px;
        background: radial-gradient(circle, color-mix(in srgb, var(--accent-ember) 18%, transparent) 0%, transparent 58%);
        filter: blur(16px);
        opacity: 0.8;
    }

    .ov-hero::after {
        inset: 0;
        background:
            linear-gradient(120deg, rgba(255, 255, 255, 0.06) 0%, transparent 24%, transparent 72%, rgba(255, 255, 255, 0.03) 100%),
            repeating-linear-gradient(135deg, transparent 0 28px, rgba(255, 255, 255, 0.018) 28px 29px);
        opacity: 0.35;
        z-index: 0;
    }

    .ov-hero__copy,
    .ov-hero__aside {
        position: relative;
        z-index: 1;
    }

    .ov-hero__eyebrow {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        padding: 6px 12px;
        border-radius: 999px;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 64%, transparent);
        background: color-mix(in srgb, var(--surface-flat-soft) 92%, transparent);
        color: color-mix(in srgb, var(--text-secondary) 82%, var(--ornament) 18%);
        font-size: 11px;
        font-weight: 700;
        letter-spacing: 0.16em;
        text-transform: uppercase;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08);
    }

    .ov-hero__title {
        margin: 14px 0 12px;
        font-size: clamp(32px, 3.2vw, 48px);
        line-height: 0.94;
        letter-spacing: 0.04em;
    }

    .ov-hero__desc {
        max-width: 56ch;
        font-size: 15px;
        line-height: 1.78;
        color: color-mix(in srgb, var(--text-secondary) 88%, var(--ornament) 12%);
    }

    .ov-hero__chips {
        display: flex;
        flex-wrap: wrap;
        gap: 10px;
        margin-top: 20px;
    }

    .ov-hero__chip {
        display: inline-flex;
        align-items: center;
        min-height: 34px;
        padding: 0 14px;
        border-radius: 999px;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 66%, transparent);
        background: color-mix(in srgb, var(--surface-flat-soft) 92%, transparent);
        color: var(--text-primary);
        font-size: 12px;
        font-weight: 600;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.07);
    }

    .ov-hero__chip--live {
        color: var(--success);
        border-color: color-mix(in srgb, var(--success) 36%, transparent);
        background: color-mix(in srgb, var(--success-bg) 78%, var(--surface-flat-soft) 22%);
    }

    .ov-hero__chip--quiet {
        color: var(--danger);
        border-color: color-mix(in srgb, var(--danger) 36%, transparent);
        background: color-mix(in srgb, var(--danger-bg) 78%, var(--surface-flat-soft) 22%);
    }

    .ov-hero__aside {
        display: grid;
        gap: 12px;
        align-content: center;
    }

    .ov-hero__stat {
        padding: 16px 18px;
        border-radius: 18px;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 64%, transparent);
        background: color-mix(in srgb, var(--surface-flat-soft) 94%, transparent);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), var(--surface-shadow-soft);
    }

    .ov-hero__stat-label {
        display: block;
        margin-bottom: 6px;
        font-size: 10px;
        font-weight: 700;
        letter-spacing: 0.16em;
        text-transform: uppercase;
        color: color-mix(in srgb, var(--text-muted) 84%, var(--ornament) 16%);
    }

    .ov-hero__stat-value {
        display: block;
        font-size: 16px;
        line-height: 1.4;
        color: var(--text-primary);
    }

    .ov-actions {
        gap: 10px;
        margin-bottom: 22px;
    }

    .ov-action-btn {
        min-height: 40px;
        padding: 0 18px;
        border-radius: 999px;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 66%, transparent);
        background: var(--surface-panel-soft);
        color: color-mix(in srgb, var(--text-secondary) 88%, var(--ornament) 12%);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), var(--surface-shadow-soft);
        backdrop-filter: blur(18px) saturate(132%);
        -webkit-backdrop-filter: blur(18px) saturate(132%);
    }

    .ov-action-btn:hover {
        background: color-mix(in srgb, var(--accent) 8%, var(--surface-hover) 92%);
        border-color: color-mix(in srgb, var(--surface-stroke) 52%, var(--accent) 48%);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.10), 0 14px 28px color-mix(in srgb, var(--accent) 10%, transparent);
    }

    .ov-action-btn--primary {
        border-color: transparent;
        background: var(--button-primary-surface);
        color: #fff;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-primary-shadow);
    }

    .ov-action-btn--primary:hover {
        background: var(--button-primary-surface-hover);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-primary-shadow-hover);
    }

    .stat-card,
    .card {
        position: relative;
        overflow: hidden;
        background: var(--surface-panel);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 66%, transparent);
        border-radius: var(--radius-lg);
        box-shadow: var(--surface-inner), var(--surface-shadow-soft);
        backdrop-filter: blur(22px) saturate(136%);
        -webkit-backdrop-filter: blur(22px) saturate(136%);
    }

    .stat-card::before,
    .card::before {
        content: "";
        position: absolute;
        inset: 0;
        background: linear-gradient(180deg, rgba(255, 255, 255, 0.03) 0%, transparent 42%);
        pointer-events: none;
    }

    .stat-card {
        padding: 18px;
    }

    .card {
        padding: 22px;
    }

    .card-title,
    .stat-label {
        position: relative;
        z-index: 1;
        font-size: 11px;
        font-weight: 700;
        letter-spacing: 0.14em;
        text-transform: uppercase;
        color: color-mix(in srgb, var(--text-muted) 84%, var(--ornament) 16%);
    }

    .card-title {
        margin-bottom: 16px;
    }

    .stat-value {
        position: relative;
        z-index: 1;
        font-size: 24px;
        letter-spacing: -0.04em;
    }

    .stat-trend {
        position: relative;
        z-index: 1;
        font-style: italic;
        color: color-mix(in srgb, var(--text-secondary) 88%, var(--ornament) 12%);
    }

    .ov-channel-card,
    .ov-quick-card {
        border-radius: 18px;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 64%, transparent);
        background: color-mix(in srgb, var(--surface-flat-soft) 94%, transparent);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), var(--surface-shadow-soft);
    }

    .ov-channel-card:hover,
    .ov-quick-card:hover {
        background: color-mix(in srgb, var(--accent) 8%, var(--surface-hover) 92%);
        border-color: color-mix(in srgb, var(--surface-stroke) 52%, var(--accent) 48%);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.10), 0 14px 30px color-mix(in srgb, var(--accent) 10%, transparent);
    }

    .ov-channel-icon,
    .ov-quick-icon {
        border-radius: 14px;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08);
    }

    .ov-model-row,
    .ov-error-item,
    .info-row {
        position: relative;
        border-bottom-color: color-mix(in srgb, var(--surface-stroke) 72%, transparent);
    }

    .ov-model-row:hover,
    .ov-error-item:hover {
        background: linear-gradient(180deg, color-mix(in srgb, var(--accent) 5%, transparent) 0%, color-mix(in srgb, var(--ornament) 5%, transparent) 100%);
    }

    .ov-model-provider-badge {
        border-radius: 999px;
        padding: 4px 9px;
        background: color-mix(in srgb, var(--accent) 10%, transparent);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 60%, var(--accent) 40%);
        color: color-mix(in srgb, var(--text-secondary) 82%, var(--ornament) 18%);
        text-transform: uppercase;
        letter-spacing: 0.08em;
    }

    .ov-error-item {
        padding: 14px 12px;
        border-radius: 14px;
    }

    .ov-error-msg {
        color: color-mix(in srgb, var(--danger) 88%, var(--accent) 12%);
    }

    .info-label {
        color: color-mix(in srgb, var(--text-secondary) 86%, var(--ornament) 14%);
    }

    @media screen and (max-width: 768px) {
        .ov-hero {
            grid-template-columns: 1fr;
            padding: 22px 18px;
        }

        .ov-hero__aside {
            grid-template-columns: 1fr;
        }

        .ov-hero__title {
            margin-top: 12px;
        }

        .ov-hero__desc {
            font-size: 14px;
        }

        .overview-page {
            /* responsive padding handled by .page-content */
        }

        .page-title {
            font-size: 18px;
            margin-bottom: 12px;
        }

        .ov-actions {
            margin-bottom: 16px;
        }

        .ov-action-btn {
            padding: 6px 14px;
            font-size: 12px;
        }

        .stats-grid {
            grid-template-columns: repeat(2, 1fr);
            gap: 10px;
        }

        .stat-card {
            padding: 12px;
        }

        .stat-value {
            font-size: 18px;
        }

        .ov-trio-grid {
            grid-template-columns: 1fr;
            gap: 10px;
        }

        .ov-channel-grid {
            grid-template-columns: 1fr;
        }

        .card {
            padding: 16px;
        }

        .info-row {
            flex-direction: column;
            gap: 2px;
        }

        .info-value {
            font-size: 12px;
        }

        .ov-model-row {
            flex-direction: column;
            align-items: flex-start;
            gap: 4px;
        }

        .ov-quick-grid {
            grid-template-columns: repeat(2, 1fr);
            gap: 8px;
        }

        .ov-quick-card {
            padding: 16px 10px;
        }
    }
"#;
