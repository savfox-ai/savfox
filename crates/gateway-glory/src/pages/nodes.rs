use glory::prelude::*;
use serde_json::json;

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "node.list", None);
    let devices = query(ctx, "device.pair.list", None);
    let detail = path
        .strip_prefix("/nodes/")
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let row_state = state.clone();
    let has_detail = detail.is_some();
    let list_context = ctx.clone();
    let node_rows = state.value.map(|value| array(value, &["nodes"]));
    let list = col().class("split-view__list agent-list-pane").fill((
        row().class("panel-header").fill((
            text()
                .class("page-title panel-header__title")
                .kind(TextKind::Heading(1))
                .content(ui::label_for(locale, "Nodes")),
            refresh(ctx, &state),
        )),
        status(ctx, &state),
        col().class("agent-list-scroll").fill((
            when(
                state.value.map(|value| array(value, &["nodes"]).is_empty()),
                move || {
                    col().class("nodes-empty-list").fill((
                        text().class("shell-icon shell-icon--network").content(""),
                        text().content(ui::label_for(locale, "No nodes connected")),
                    ))
                },
            ),
            each(node_rows).key(record_key).children(move |each_row| {
                let item = each_row.value;
                let node = item.get_untracked();
                let id = string(&node, &["id", "node_id"]);
                let context = list_context.clone();
                col().class("agent-list-item").fill((
                    sapid::button()
                        .unstyled()
                        .class("btn-plain agent-select")
                        .label(string(&node, &["name", "id"]))
                        .on_press(move || context.navigate(&format!("/nodes/{id}"))),
                    text()
                        .class("list-item__subtitle")
                        .content(string(&node, &["status", "platform"])),
                ))
            }),
        )),
        col()
            .class("nodes-pair-action")
            .fill(nav(ctx, "Pair device", "/device-pair")),
    ));
    let selected_node = detail.clone();
    let mut children = Vec::new();
    if let Some(id) = detail.clone() {
        let info = query(ctx, "node.describe", Some(json!({"node_id":id})));
        children.push(card(
            ui::label_for(locale, "Node details"),
            vec![status(ctx, &info), ui::json_text(ctx, info.value)],
        ));
    }
    children.push(records(ctx, &state, &["nodes"], move |node, context| {
        let id = string(&node, &["id", "node_id"]);
        if selected_node.as_deref() != Some(id.as_str()) {
            return Box::new(group()) as Box<dyn Widget>;
        }
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
                metadata_for(
                    locale,
                    &node,
                    &[
                        ("status", "Status"),
                        ("platform", "Platform"),
                        ("version", "Version"),
                        ("last_seen", "Last seen"),
                    ],
                ),
                ui::field(ui::label_for(locale, "Node name"), name, false),
                ui::action(ui::label_for(locale, "Rename"), move || {
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
        ui::label_for(locale, "Device pairing requests"),
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
                            metadata_for(
                                locale,
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
    let detail: Box<dyn Widget> = if has_detail {
        Box::new(col().class("nodes-detail-content").fill(children))
    } else {
        Box::new(
            col().class("agent-empty-detail").fill((
                text()
                    .class("empty-state__message")
                    .content(ui::label_for(locale, "Select a node to view details")),
                ui::details(
                    "node-pairing-requests",
                    ui::label_for(locale, "Device pairing requests"),
                    Box::new(col().fill(children)),
                ),
            )),
        )
    };
    Box::new(row().class(if has_detail {"gateway-page management-page agent-workspace nodes-workspace split-view split-view--detail"}else{"gateway-page management-page agent-workspace nodes-workspace split-view"}).fill((list,col().class("split-view__detail").fill(detail))))
}
