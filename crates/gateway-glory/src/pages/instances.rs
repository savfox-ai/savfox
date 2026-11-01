use glory::prelude::*;
use serde_json::json;

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let state = query(ctx, "system.connections", None);
    let row_state = state.clone();
    ui::localized_page(
        ctx,
        "nav.instances",
        "Clients connected to the gateway",
        vec![
            status(ctx, &state),
            refresh(ctx, &state),
            records(
                ctx,
                &state,
                &["entries", "instances", "presence", "clients"],
                move |mut instance, context| {
                    let is_current = instance["is_current"].as_bool().unwrap_or(false);
                    if let Some(timestamp) = instance["connected_at"]
                        .as_str()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    {
                        instance["connected_at"] =
                            json!(timestamp.format("%Y-%m-%d %H:%M:%S %:z").to_string());
                    }
                    let mut actions = vec![];
                    if is_current {
                        let current = context.clone();
                        actions.push(ui::action(ui::label(&context, "Sign out"), move || {
                            crate::api::ws::clear_token();
                            current.rpc.disconnect();
                            ui::set(current.authenticated, false);
                        }));
                    } else {
                        let context = context.clone();
                        let state = row_state.clone();
                        let session_id = string(&instance, &["session_id"]);
                        actions.push(ui::action(ui::label(&context, "Disconnect"), move || {
                            if confirmed(&ui::translate(
                                context.locale.get(),
                                "Disconnect this client?",
                            )) {
                                mutate(
                                    &context,
                                    &state,
                                    "system.disconnect",
                                    json!({"session_id":session_id}),
                                );
                            }
                        }));
                    }
                    card(
                        string(&instance, &["name", "session_id"]),
                        vec![
                            Box::new(text().class("form-help").content(if is_current {
                                ui::label(&context, "This browser")
                            } else {
                                ui::label(&context, "Remote client")
                            })),
                            metadata_for(
                                context.locale,
                                &instance,
                                &[
                                    ("session_id", "Connection ID"),
                                    ("connected_at", "Connected at"),
                                ],
                            ),
                            Box::new(row().class("actions").fill(actions)),
                        ],
                    )
                },
            ),
        ],
    )
}
