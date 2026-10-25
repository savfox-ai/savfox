use glory::prelude::*;
use serde_json::json;

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let state = query(ctx, "node.list", None);
    let devices = query(ctx, "device.pair.list", None);
    let detail = path
        .strip_prefix("/nodes/")
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let row_state = state.clone();
    let mut children = vec![
        status(ctx, &state),
        refresh(ctx, &state),
        nav(ctx, "Pair device", "/device-pair"),
    ];
    if let Some(id) = detail {
        let info = query(ctx, "node.describe", Some(json!({"node_id":id})));
        children.push(card(
            "Node details",
            vec![status(ctx, &info), ui::json_text(ctx, info.value)],
        ));
    }
    children.push(records(ctx, &state, &["nodes"], move |node, context| {
        let id = string(&node, &["id", "node_id"]);
        let name = context.scope.cage(string(&node, &["name"]));
        let mut actions = vec![nav(&context, "Details", format!("/nodes/{id}"))];
        for (label, method) in [
            ("Rotate token", "device.token.rotate"),
            ("Revoke token", "device.token.revoke"),
        ] {
            let context = context.clone();
            let state = row_state.clone();
            let id = id.clone();
            actions.push(ui::action(label, move || {
                if confirmed("Change this device's access token?") {
                    mutate(&context, &state, method, json!({"node_id":id}));
                }
            }));
        }
        let context_rename = context.clone();
        let state_rename = row_state.clone();
        let id_rename = id.clone();
        card(
            string(&node, &["name", "id"]),
            vec![
                metadata(
                    &node,
                    &[
                        ("status", "Status"),
                        ("platform", "Platform"),
                        ("version", "Version"),
                        ("last_seen", "Last seen"),
                    ],
                ),
                ui::field("Node name", name, false),
                ui::action("Rename", move || {
                    mutate(
                        &context_rename,
                        &state_rename,
                        "node.rename",
                        json!({"node_id":id_rename,"name":name.get()}),
                    )
                }),
                Box::new(row().class("actions").fill(actions)),
            ],
        )
    }));
    let pair_state = devices.clone();
    children.push(card(
        "Device pairing requests",
        vec![
            status(ctx, &devices),
            records(
                ctx,
                &devices,
                &["devices", "requests", "pending"],
                move |device, context| {
                    let mut actions = vec![];
                    for (label, method) in [
                        ("Accept", "device.pair.accept"),
                        ("Reject", "device.pair.reject"),
                    ] {
                        let context = context.clone();
                        let state = pair_state.clone();
                        let id = string(&device, &["id", "device_id"]);
                        actions.push(ui::action(label, move || {
                            mutate(&context, &state, method, json!({"device_id":id}))
                        }));
                    }
                    card(
                        string(&device, &["name", "id"]),
                        vec![
                            metadata(
                                &device,
                                &[("status", "Status"), ("requested_at", "Requested")],
                            ),
                            Box::new(row().class("actions").fill(actions)),
                        ],
                    )
                },
            ),
        ],
    ));
    ui::localized_page(
        ctx,
        "nav.nodes",
        "Connected nodes and device authorization",
        children,
    )
}
