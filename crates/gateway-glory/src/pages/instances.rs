use glory::prelude::*;
use serde_json::json;

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let state = query(ctx, "system-presence", None);
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
                move |instance, context| {
                    let mut actions = vec![];
                    for (label, method) in
                        [("Disconnect", "system-disconnect"), ("Kick", "system-kick")]
                    {
                        let context = context.clone();
                        let state = row_state.clone();
                        let host = string(&instance, &["host", "id"]);
                        actions.push(ui::action(label, move || {
                            if confirmed("Disconnect this instance?") {
                                mutate(&context, &state, method, json!({"host":host}));
                            }
                        }));
                    }
                    card(
                        string(&instance, &["name", "host", "id"]),
                        vec![
                            metadata(
                                &instance,
                                &[
                                    ("platform", "Platform"),
                                    ("version", "Version"),
                                    ("mode", "Mode"),
                                    ("last_seen", "Last seen"),
                                    ("connected_at", "Connected"),
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
