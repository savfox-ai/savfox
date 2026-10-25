use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let state = query(ctx, "voicewake.get", None);
    let enabled = ctx.scope.cage(false);
    let word = ctx.scope.cage(String::new());
    let mode = ctx.scope.cage("text".to_owned());
    let saved_mode = ctx.scope.cage(String::new());
    let value = state.value;
    ctx.scope.effect(move || {
        let value = value.get();
        if !value.is_null() {
            ui::set(enabled, value["enabled"].as_bool().unwrap_or(false));
            ui::set(word, string(&value, &["keyword"]));
        }
    });
    let context = ctx.clone();
    let mode_state = state.clone();
    let apply = ui::action("Apply talk mode", move || {
        let context_async = context.clone();
        let mode = mode.get();
        let state = mode_state.clone();
        ui::set(state.busy, true);
        context.spawn(async move {
            match context_async
                .rpc
                .call::<Value>("talk.mode", Some(json!({"mode":mode})))
                .await
            {
                Ok(value) => ui::set(
                    saved_mode,
                    format!("Applied talk mode: {}", string(&value, &["mode"])),
                ),
                Err(message) => ui::set(state.error, message),
            }
            ui::set(state.busy, false);
        });
    });
    let context = ctx.clone();
    let save_state = state.clone();
    ui::localized_page(
        ctx,
        "nav.voice",
        "Talk mode and wake-word settings",
        vec![
            status(ctx, &state),
            refresh(ctx, &state),
            choice("Talk mode", mode, &[("text", "Text"), ("voice", "Voice")]),
            apply,
            Box::new(text().content(saved_mode)),
            toggle("Wake word enabled", enabled),
            ui::field("Wake word", word, false),
            ui::action("Save wake settings", move || {
                if word.get().trim().is_empty() {
                    ui::set(save_state.error, "Wake word is required".to_owned());
                    return;
                }
                mutate(
                    &context,
                    &save_state,
                    "voicewake.set",
                    json!({"enabled":enabled.get(),"keyword":word.get()}),
                );
            }),
        ],
    )
}
