use dioxus::prelude::*;
use lucide_dioxus::{ArrowLeft, Check, Network, X};
use serde_json::json;

use crate::api::types::{
    DevicePairEntry, ExecApprovalPolicy, NodeDetail, NodeEntry, NodeToken, NodesResponse,
};
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::empty_state::EmptyState;
use crate::components::skeleton::*;
use crate::utils::deep_link::replace_url;

#[component]
pub fn Nodes() -> Element {
    nodes_inner(Option::None)
}

#[component]
pub fn NodesDetail(node_id: String) -> Element {
    nodes_inner(Some(node_id))
}

fn nodes_inner(initial_node: Option<String>) -> Element {
    let is_routed = initial_node.is_some();
    let initial_show_detail = initial_node.is_some();
    let nav = use_navigator();

    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut selected_node = use_signal(move || initial_node);
    let mut show_detail = use_signal(move || initial_show_detail);

    // Sync URL with selected node for deep linking
    use_effect(move || {
        let selected = selected_node();
        if let Some(ref id) = selected {
            replace_url(&format!("/nodes/{id}"));
        } else if is_routed {
            nav.replace(crate::route::Route::Nodes {});
        } else {
            replace_url("/nodes");
        }
    });

    let ws_list = ws.clone();
    let nodes_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_list.clone();
        async move {
            ws.call::<NodesResponse>("node.list", None)
                .await
                .map(|r| r.nodes)
                .unwrap_or_default()
        }
    });

    let ws_detail = ws.clone();
    let sel = selected_node();
    let detail_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_detail.clone();
        let node_id = sel.clone();
        async move {
            if let Some(id) = node_id {
                ws.call::<NodeDetail>("node.describe", Some(json!({ "node_id": id })))
                    .await
                    .ok()
            } else {
                None
            }
        }
    });

    let ws_devices = ws.clone();
    let devices_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_devices.clone();
        async move {
            ws.call::<serde_json::Value>("device.pair.list", None)
                .await
                .ok()
                .and_then(|v| {
                    serde_json::from_value::<Vec<DevicePairEntry>>(
                        v.get("devices")
                            .cloned()
                            .unwrap_or(serde_json::Value::Array(vec![])),
                    )
                    .ok()
                })
                .unwrap_or_default()
        }
    });

    // Exec approval policy
    let ws_policy = ws.clone();
    let policy_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_policy.clone();
        async move {
            ws.call::<ExecApprovalPolicy>("approvals.policy", None)
                .await
                .ok()
        }
    });

    let nodes: Vec<NodeEntry> = nodes_data.read().as_ref().cloned().unwrap_or_default();
    let devices: Vec<DevicePairEntry> = devices_data.read().as_ref().cloned().unwrap_or_default();
    let is_loading = nodes_data.read().is_none();

    let selected_detail: Option<NodeDetail> = detail_data
        .read()
        .as_ref()
        .and_then(|d| d.as_ref())
        .cloned();

    let selected_entry: Option<NodeEntry> =
        selected_node().and_then(|id| nodes.iter().find(|n| n.id == id).cloned());

    let policy = policy_data
        .read()
        .as_ref()
        .and_then(|p| p.as_ref())
        .cloned();

    // Build exec policy matrix for the detail view
    let policy_matrix = build_exec_policy_matrix(&policy);

    let detail_class = if show_detail() {
        "split-view--detail-active"
    } else {
        ""
    };

    rsx! {
        div { class: "split-view {detail_class}",
            // Left: node list
            div { class: "split-view__list",
                div { class: "panel-header",
                    h2 { style: "font-size:16px;font-weight:600;", "Nodes" }
                    button {
                        onclick: move |_| refresh_tick += 1,
                        class: "{TOOL_BTN}",
                        "Refresh"
                    }
                }

                div { style: "flex:1;overflow:auto;",
                    if is_loading {
                        div { style: "padding:16px;",
                            SkeletonLines { count: 3 }
                        }
                    } else if nodes.is_empty() {
                        EmptyState {
                            icon: rsx! { Network { size: 20 } },
                            message: "No nodes connected".to_string(),
                        }
                    } else {
                        for node in nodes.iter() {
                            {
                                let is_sel = selected_node() == Some(node.id.clone());
                                let bg = if is_sel { "var(--bg-hover)" } else { "transparent" };
                                let status = node.status.as_deref().unwrap_or("unknown");
                                let n = node.clone();
                                let status_variant = match status {
                                    "online" | "connected" | "active" => ChipVariant::Success,
                                    "offline" | "disconnected" => ChipVariant::Danger,
                                    "pairing" | "pending" => ChipVariant::Warning,
                                    _ => ChipVariant::Muted,
                                };
                                rsx! {
                                    div {
                                        key: "{node.id}",
                                        role: "button",
                                        tabindex: "0",
                                        onclick: move |_| {
                                            selected_node.set(Some(n.id.clone()));
                                            show_detail.set(true);
                                        },
                                        style: "padding:10px 16px;border-bottom:1px solid var(--border);cursor:pointer;background:{bg};transition:background 0.1s;",
                                        div { style: "display:flex;justify-content:space-between;align-items:center;",
                                            span { style: "font-weight:500;font-size:14px;",
                                                "{node.name.as_deref().unwrap_or(&node.id)}"
                                            }
                                            Chip { label: status.to_string(), variant: status_variant }
                                        }
                                        div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;",
                                            if let Some(ref p) = node.platform {
                                                "{p} | "
                                            }
                                            if let Some(ref ls) = node.last_seen {
                                                "{ls}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Devices section
                    if !devices.is_empty() {
                        div { style: "padding:12px 16px;border-top:2px solid var(--border);",
                            h3 { style: "font-size:13px;font-weight:600;color:var(--text-secondary);margin-bottom:8px;", "Pairing Requests" }

                            // Pairing guidance steps
                            div { style: "margin-bottom:12px;display:flex;flex-direction:column;gap:6px;",
                                div { style: "display:flex;align-items:center;gap:8px;",
                                    span { class: "{STEP_CIRCLE}", "1" }
                                    span { style: "font-size:12px;color:var(--text-muted);", "Run the pairing command on your node device" }
                                }
                                div { style: "display:flex;align-items:center;gap:8px;",
                                    span { class: "{STEP_CIRCLE}", "2" }
                                    span { style: "font-size:12px;color:var(--text-muted);", "Wait for the pairing request to appear below" }
                                }
                                div { style: "display:flex;align-items:center;gap:8px;",
                                    span { class: "{STEP_CIRCLE}", "3" }
                                    span { style: "font-size:12px;color:var(--text-muted);", "Accept the request to complete pairing" }
                                }
                            }

                            for dev in devices.iter() {
                                { render_device_entry(dev, ws.clone(), refresh_tick) }
                            }
                        }
                    }

                    // Exec Approval Policy section (bottom of sidebar)
                    if let Some(ref pol) = policy {
                        div { style: "padding:12px 16px;border-top:2px solid var(--border);",
                            h3 { style: "font-size:13px;font-weight:600;color:var(--text-secondary);margin-bottom:8px;", "Exec Policy" }
                            div { style: "font-size:12px;",
                                div { style: "margin-bottom:4px;display:flex;gap:6px;align-items:center;",
                                    span { style: "color:var(--text-muted);", "Mode:" }
                                    {
                                        let mode = pol.mode.as_deref().unwrap_or("unknown");
                                        let variant = match mode {
                                            "deny" => ChipVariant::Danger,
                                            "allowlist" => ChipVariant::Warning,
                                            "full" => ChipVariant::Success,
                                            _ => ChipVariant::Muted,
                                        };
                                        rsx! { Chip { label: mode.to_string(), variant: variant } }
                                    }
                                }
                                if let Some(ref rules) = pol.rules {
                                    if !rules.is_empty() {
                                        div { style: "margin-top:6px;",
                                            span { style: "color:var(--text-muted);", "Rules:" }
                                            div { style: "display:flex;flex-direction:column;gap:2px;margin-top:4px;",
                                                for rule in rules.iter() {
                                                    code {
                                                        style: "font-size:11px;padding:2px 6px;background:var(--bg-tertiary);border-radius:4px;display:block;",
                                                        "{rule}"
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

            // Right: node detail
            div { class: "split-view__detail",
                button {
                    class: "split-view__back",
                    onclick: move |_| show_detail.set(false),
                    ArrowLeft { size: 14 }
                    " Back"
                }
                if let Some(ref entry) = selected_entry {
                    { render_node_detail(ws.clone(), refresh_tick, entry, &selected_detail, &policy_matrix) }
                } else {
                    div { style: "display:flex;align-items:center;justify-content:center;height:100%;color:var(--text-muted);font-size:14px;",
                        "Select a node to view details"
                    }
                }
            }
        }
    }
}

fn render_device_entry(dev: &DevicePairEntry, ws: WsRpc, mut refresh_tick: Signal<u32>) -> Element {
    let dev_status = dev.status.as_deref().unwrap_or("pending");
    let id_accept = dev.id.clone();
    let id_reject = dev.id.clone();
    let ws_accept = ws.clone();
    let ws_reject = ws;

    let status_variant = match dev_status {
        "accepted" | "active" => ChipVariant::Success,
        "pending" => ChipVariant::Warning,
        "rejected" => ChipVariant::Danger,
        _ => ChipVariant::Muted,
    };

    rsx! {
        div {
            key: "{dev.id}",
            style: "padding:8px;background:var(--bg-tertiary);border-radius:var(--radius);margin-bottom:4px;",
            div { style: "display:flex;justify-content:space-between;align-items:center;",
                div {
                    span { style: "font-size:13px;font-weight:500;", "{dev.name.as_deref().unwrap_or(&dev.id)}" }
                    Chip { label: dev_status.to_string(), variant: status_variant }
                }
                if dev_status == "pending" {
                    div { style: "display:flex;gap:4px;",
                        button {
                            onclick: move |_| {
                                let id = id_accept.clone();
                                let ws = ws_accept.clone();
                                spawn(async move {
                                    let _ = ws.call::<serde_json::Value>(
                                        "device.pair.accept",
                                        Some(json!({ "device_id": id })),
                                    ).await;
                                    refresh_tick += 1;
                                });
                            },
                            class: "{TOOL_BTN} tool-btn--accept",
                            "Accept"
                        }
                        button {
                            onclick: move |_| {
                                let id = id_reject.clone();
                                let ws = ws_reject.clone();
                                spawn(async move {
                                    let _ = ws.call::<serde_json::Value>(
                                        "device.pair.reject",
                                        Some(json!({ "device_id": id })),
                                    ).await;
                                    refresh_tick += 1;
                                });
                            },
                            class: "{TOOL_BTN} tool-btn--reject",
                            "Reject"
                        }
                    }
                }
            }
            if let Some(ref ts) = dev.requested_at {
                div { style: "font-size:11px;color:var(--text-muted);margin-top:2px;", "Requested: {ts}" }
            }
        }
    }
}

fn render_node_detail(
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    node: &NodeEntry,
    detail: &Option<NodeDetail>,
    policy_matrix: &[(String, bool)],
) -> Element {
    let node_id = node.id.clone();
    let status = node.status.as_deref().unwrap_or("unknown");
    let mut rename_val = use_signal(|| node.name.clone().unwrap_or_default());
    let id_rename = node.id.clone();
    let ws_rename = ws.clone();
    let id_rotate = node.id.clone();
    let ws_rotate = ws.clone();

    let status_variant = match status {
        "online" | "connected" | "active" => ChipVariant::Success,
        "offline" | "disconnected" => ChipVariant::Danger,
        "pairing" | "pending" => ChipVariant::Warning,
        _ => ChipVariant::Muted,
    };

    rsx! {
        div { style: "display:flex;flex-direction:column;height:100%;",
            // Header
            div { class: "panel-header",
                div { style: "display:flex;align-items:center;gap:8px;",
                    span { style: "font-weight:600;font-size:16px;", "{node.name.as_deref().unwrap_or(&node_id)}" }
                    Chip { label: status.to_string(), variant: status_variant }
                }
                div { style: "display:flex;gap:6px;",
                    button {
                        onclick: move |_| {
                            let id = id_rotate.clone();
                            let ws = ws_rotate.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>(
                                    "device.token.rotate",
                                    Some(json!({ "node_id": id })),
                                ).await;
                                refresh_tick += 1;
                            });
                        },
                        class: "{TOOL_BTN}",
                        "Rotate Token"
                    }
                }
            }

            // Rename
            div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;gap:8px;align-items:center;",
                input {
                    value: "{rename_val}",
                    oninput: move |e| rename_val.set(e.value()),
                    placeholder: "Node name",
                    class: "node-rename-input",
                }
                button {
                    onclick: move |_| {
                        let id = id_rename.clone();
                        let name = rename_val();
                        let ws = ws_rename.clone();
                        spawn(async move {
                            let _ = ws.call::<serde_json::Value>(
                                "node.rename",
                                Some(json!({ "node_id": id, "name": name })),
                            ).await;
                            refresh_tick += 1;
                        });
                    },
                    class: "{TOOL_BTN} tool-btn--rename",
                    "Rename"
                }
            }

            div { style: "padding:16px;overflow:auto;flex:1;",
                // Info
                div { style: "margin-bottom:16px;",
                    div { style: "margin-bottom:6px;",
                        span { style: "font-size:12px;color:var(--text-muted);", "ID: " }
                        code { style: "font-size:12px;", "{node.id}" }
                    }
                    if let Some(ref p) = node.platform {
                        div { style: "margin-bottom:6px;",
                            span { style: "font-size:12px;color:var(--text-muted);", "Platform: " }
                            span { style: "font-size:13px;", "{p}" }
                        }
                    }
                    if let Some(ref ls) = node.last_seen {
                        div { style: "margin-bottom:6px;",
                            span { style: "font-size:12px;color:var(--text-muted);", "Last seen: " }
                            span { style: "font-size:13px;", "{ls}" }
                        }
                    }
                }

                // Capabilities
                if let Some(det) = detail {
                    if let Some(ref caps) = det.capabilities {
                        if !caps.is_empty() {
                            div { style: "margin-bottom:16px;",
                                h4 { class: "{SECTION_TITLE}", "Capabilities" }
                                div { style: "display:flex;gap:6px;flex-wrap:wrap;",
                                    for cap in caps.iter() {
                                        Chip { label: cap.clone(), variant: ChipVariant::Info }
                                    }
                                }
                            }
                        }
                    }

                    // Commands
                    if let Some(ref cmds) = det.commands {
                        if !cmds.is_empty() {
                            div { style: "margin-bottom:16px;",
                                h4 { class: "{SECTION_TITLE}", "Commands" }
                                div { style: "display:flex;gap:6px;flex-wrap:wrap;",
                                    for cmd in cmds.iter() {
                                        Chip { label: cmd.clone(), variant: ChipVariant::Muted }
                                    }
                                }
                            }
                        }
                    }

                    // Tokens
                    if let Some(ref tokens) = det.tokens {
                        if !tokens.is_empty() {
                            div { style: "margin-bottom:16px;",
                                h4 { class: "{SECTION_TITLE}", "Tokens" }
                                div { style: "border:1px solid var(--border);border-radius:var(--radius);overflow:hidden;",
                                    table { style: "width:100%;border-collapse:collapse;",
                                        thead {
                                            tr { style: "background:var(--bg-tertiary);",
                                                th { class: "{TH}", "Role" }
                                                th { class: "{TH}", "Token" }
                                                th { class: "{TH}", "Scopes" }
                                                th { class: "{TH}", "Status" }
                                                th { class: "{TH}", "Actions" }
                                            }
                                        }
                                        tbody {
                                            for (i, token) in tokens.iter().enumerate() {
                                                { render_token_row(i, token, ws.clone(), &node.id, refresh_tick) }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Exec Policy Permission Matrix
                if !policy_matrix.is_empty() {
                    div { style: "margin-bottom:16px;",
                        h4 { class: "{SECTION_TITLE}", "Exec Policy" }
                        div { class: "policy-matrix",
                            table { class: "policy-matrix__table",
                                thead {
                                    tr {
                                        th { class: "policy-matrix__th", "Category" }
                                        th { class: "policy-matrix__th policy-matrix__th--allow", "Allow" }
                                        th { class: "policy-matrix__th policy-matrix__th--deny", "Deny" }
                                    }
                                }
                                tbody {
                                    for (category, allowed) in policy_matrix.iter() {
                                        {
                                            let cat = category.clone();
                                            let is_allowed = *allowed;
                                            let allow_class = if is_allowed { "policy-matrix__cell policy-matrix__cell--active" } else { "policy-matrix__cell" };
                                            let deny_class = if is_allowed { "policy-matrix__cell" } else { "policy-matrix__cell policy-matrix__cell--active" };
                                            rsx! {
                                                tr { key: "{cat}", class: "policy-matrix__row",
                                                    td { class: "policy-matrix__category", "{cat}" }
                                                    td { class: "{allow_class}",
                                                        span { class: "policy-matrix__check",
                                                            if is_allowed {
                                                                Check { size: 14 }
                                                            }
                                                        }
                                                    }
                                                    td { class: "{deny_class}",
                                                        span { class: "policy-matrix__cross",
                                                            if !is_allowed {
                                                                X { size: 14 }
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

                // Raw JSON details (collapsible)
                if let Some(det) = detail {
                    {
                        let json_str = serde_json::to_string_pretty(det).unwrap_or_default();
                        rsx! {
                            details { style: "margin-top:8px;",
                                summary { style: "font-size:14px;font-weight:600;color:var(--text-secondary);cursor:pointer;margin-bottom:8px;", "Raw JSON" }
                                pre {
                                    style: "background:var(--bg-tertiary);padding:12px;border-radius:var(--radius);font-size:12px;overflow:auto;max-height:300px;color:var(--text-secondary);",
                                    "{json_str}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn mask_token(value: &str) -> String {
    if value.len() <= 8 {
        "*".repeat(value.len())
    } else {
        let start = &value[..4];
        let end = &value[value.len() - 4..];
        format!("{start}...{end}")
    }
}

fn render_token_row(
    i: usize,
    token: &NodeToken,
    ws: WsRpc,
    node_id: &str,
    mut refresh_tick: Signal<u32>,
) -> Element {
    let role = token.role.as_deref().unwrap_or("-");
    let token_value = token.token.clone().unwrap_or_default();
    let scopes_str = token
        .scopes
        .as_ref()
        .map(|s| s.join(", "))
        .unwrap_or_else(|| "-".to_string());
    let status = token.status.as_deref().unwrap_or("active");
    let status_variant = match status {
        "active" => ChipVariant::Success,
        "revoked" => ChipVariant::Danger,
        "expired" => ChipVariant::Warning,
        _ => ChipVariant::Muted,
    };
    let node_id = node_id.to_string();
    let role_str = role.to_string();

    let mut show_token = use_signal(|| false);
    let masked = mask_token(&token_value);

    rsx! {
        tr { key: "{i}", style: "border-top:1px solid var(--border);",
            td { class: "{TD}", code { style: "font-size:12px;", "{role}" } }
            td { class: "{TD}",
                if token_value.is_empty() {
                    span { style: "font-size:12px;color:var(--text-muted);", "-" }
                } else {
                    code { style: "font-size:12px;font-family:var(--font-mono);",
                        if show_token() { "{token_value}" } else { "{masked}" }
                    }
                    button {
                        onclick: move |_| show_token.set(!show_token()),
                        class: "{TOOL_BTN} tool-btn--xs",
                        if show_token() { "Hide" } else { "Show" }
                    }
                }
            }
            td { class: "{TD}", style: "font-size:12px;", "{scopes_str}" }
            td { class: "{TD}", Chip { label: status.to_string(), variant: status_variant } }
            td { class: "{TD}",
                if status != "revoked" {
                    button {
                        onclick: move |_| {
                            let ws = ws.clone();
                            let nid = node_id.clone();
                            let r = role_str.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>(
                                    "device.token.revoke",
                                    Some(json!({ "node_id": nid, "role": r })),
                                ).await;
                                refresh_tick += 1;
                            });
                        },
                        class: "{TOOL_BTN} tool-btn--reject",
                        "Revoke"
                    }
                }
            }
        }
    }
}

/// Command categories used for the exec policy matrix.
const EXEC_CATEGORIES: &[&str] = &[
    "shell",
    "file_read",
    "file_write",
    "network",
    "process",
    "system",
    "browser",
    "code_exec",
];

/// Build a list of (category, allowed) pairs from the exec approval policy.
/// When mode is "full", everything is allowed. When "deny", everything is denied.
/// When "allowlist", only categories matching a rule are allowed.
fn build_exec_policy_matrix(policy: &Option<ExecApprovalPolicy>) -> Vec<(String, bool)> {
    let Some(pol) = policy else {
        return Vec::new();
    };
    let mode = pol.mode.as_deref().unwrap_or("deny");
    let rules: Vec<String> = pol
        .rules
        .as_ref()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| r.to_lowercase())
        .collect();

    EXEC_CATEGORIES
        .iter()
        .map(|cat| {
            let allowed = match mode {
                "full" => true,
                "deny" => false,
                "allowlist" => rules
                    .iter()
                    .any(|r| r == *cat || r == "*" || cat.contains(r.as_str()) || r.contains(*cat)),
                _ => false,
            };
            (cat.to_string(), allowed)
        })
        .collect()
}

const TOOL_BTN: &str = "tool-btn";
const SECTION_TITLE: &str = "section-title--nodes";
const TH: &str = "th-cell--nodes";
const TD: &str = "td-cell--nodes";
const STEP_CIRCLE: &str = "step-circle";
