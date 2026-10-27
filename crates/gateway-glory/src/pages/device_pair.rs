use glory::prelude::*;
use serde_json::json;

use crate::ui::{self, AppContext};

pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let code = ctx.scope.cage(serde_json::Value::Null);
    let error = ctx.scope.cage(String::new());
    let busy = ctx.scope.cage(false);
    let context = ctx.clone();
    ui::localized_page(
        ctx,
        "nav.device_pair",
        "Generate a code to authorize a new device.",
        vec![
            ui::error_text(ctx, error),
            Box::new(
                text()
                    .class("pairing-code")
                    .content(code.map(|value| value["code"].as_str().unwrap_or("").to_owned())),
            ),
            Box::new(
                sapid::button()
                    .unstyled()
                    .class("btn btn-primary")
                    .label(ui::label_for(locale, "Generate pairing code"))
                    .loading(busy)
                    .on_press(move || {
                        if busy.get() {
                            return;
                        }
                        let context_async = context.clone();
                        ui::set(busy, true);
                        ui::set(error, String::new());
                        context.spawn(async move {
                            match context_async
                                .rpc
                                .call::<serde_json::Value>("pairing.generate", Some(json!({})))
                                .await
                            {
                                Ok(value) => ui::set(code, value),
                                Err(message) => ui::set(error, message),
                            }
                            ui::set(busy, false);
                        });
                    }),
            ),
        ],
    )
}
