use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{
    DevicePairEntry, ExecApprovalPolicy, NodeDetail, NodeEntry, NodeToken, NodesResponse,
};
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};

#[component]
pub fn Nodes() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut selected_node = use_signal(|| Option::<String>::None);

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

    rsx! {
        div { style: "display:flex;height:100%;",
            // Left: node list
            div { style: "width:340px;min-width:340px;border-right:1px solid var(--border);display:flex;flex-direction:column;",
                div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;",
                    h2 { style: "font-size:16px;font-weight:600;", "Nodes" }
                    button {
                        onclick: move |_| refresh_tick += 1,
                        style: "{TOOL_BTN}",
                        "Refresh"
                    }
                }

                div { style: "flex:1;overflow:auto;",
                    if is_loading {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "Loading..." }
                    } else if nodes.is_empty() {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "No nodes connected" }
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
                                        onclick: move |_| selected_node.set(Some(n.id.clone())),
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
                            h3 { style: "font-size:13px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;margin-bottom:8px;", "Pairing Requests" }
                            for dev in devices.iter() {
                                { render_device_entry(dev, ws.clone(), refresh_tick) }
                            }
                        }
                    }

                    // Exec Approval Policy section (bottom of sidebar)
                    if let Some(ref pol) = policy {
                        div { style: "padding:12px 16px;border-top:2px solid var(--border);",
                            h3 { style: "font-size:13px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;margin-bottom:8px;", "Exec Policy" }
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
            div { style: "flex:1;display:flex;flex-direction:column;overflow:auto;",
                if let Some(ref entry) = selected_entry {
                    { render_node_detail(ws.clone(), refresh_tick, entry, &selected_detail) }
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
                            style: "{TOOL_BTN}font-size:11px;padding:2px 8px;background:var(--success);color:#fff;border:none;",
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
                            style: "{TOOL_BTN}font-size:11px;padding:2px 8px;color:var(--danger);border-color:var(--danger);",
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
            div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;",
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
                        style: "{TOOL_BTN}",
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
                    style: "flex:1;padding:6px 12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);outline:none;font-size:13px;",
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
                    style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;",
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
                                h4 { style: "{SECTION_TITLE}", "Capabilities" }
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
                                h4 { style: "{SECTION_TITLE}", "Commands" }
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
                                h4 { style: "{SECTION_TITLE}", "Tokens" }
                                div { style: "border:1px solid var(--border);border-radius:var(--radius);overflow:hidden;",
                                    table { style: "width:100%;border-collapse:collapse;",
                                        thead {
                                            tr { style: "background:var(--bg-tertiary);",
                                                th { style: "{TH}", "Role" }
                                                th { style: "{TH}", "Scopes" }
                                                th { style: "{TH}", "Status" }
                                                th { style: "{TH}", "Actions" }
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

                // Raw JSON details (collapsible)
                if let Some(det) = detail {
                    {
                        let json_str = serde_json::to_string_pretty(det).unwrap_or_default();
                        rsx! {
                            details { style: "margin-top:8px;",
                                summary { style: "font-size:14px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;cursor:pointer;margin-bottom:8px;", "Raw JSON" }
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

fn render_token_row(
    i: usize,
    token: &NodeToken,
    ws: WsRpc,
    node_id: &str,
    mut refresh_tick: Signal<u32>,
) -> Element {
    let role = token.role.as_deref().unwrap_or("-");
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

    rsx! {
        tr { key: "{i}", style: "border-top:1px solid var(--border);",
            td { style: "{TD}", code { style: "font-size:12px;", "{role}" } }
            td { style: "{TD}font-size:12px;", "{scopes_str}" }
            td { style: "{TD}", Chip { label: status.to_string(), variant: status_variant } }
            td { style: "{TD}",
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
                        style: "{TOOL_BTN}font-size:11px;padding:2px 8px;color:var(--danger);border-color:var(--danger);",
                        "Revoke"
                    }
                }
            }
        }
    }
}

const TOOL_BTN: &str = "padding:4px 12px;background:transparent;color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:12px;";
const SECTION_TITLE: &str = "font-size:14px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;margin-bottom:8px;";
const TH: &str = "text-align:left;padding:8px 12px;font-size:12px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;";
const TD: &str = "padding:8px 12px;font-size:13px;";
