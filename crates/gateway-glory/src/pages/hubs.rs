//! Related runtime tools share one navigation entry without duplicating forms.
use glory::prelude::*;

use crate::ui::AppContext;

pub fn speech(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    hub(
        ctx,
        "speech-hub",
        &[
            ("Speech synthesis", "/speech"),
            ("Talk and wake word", "/speech/wake"),
        ],
        if path == "/speech/wake" {
            super::voice::render(ctx)
        } else {
            super::tts::render(ctx)
        },
    )
}

pub fn devices(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let content = match path {
        "/devices/connections" => super::instances::render(ctx),
        "/devices/pair" => super::device_pair::render(ctx),
        _ => super::nodes::render(ctx, path),
    };
    hub(
        ctx,
        "devices-hub",
        &[
            ("Nodes", "/devices"),
            ("Connected clients", "/devices/connections"),
            ("Pair device", "/devices/pair"),
        ],
        content,
    )
}

fn hub(
    ctx: &AppContext,
    class: &str,
    tabs: &[(&str, &str)],
    content: Box<dyn Widget>,
) -> Box<dyn Widget> {
    Box::new(
        col()
            .class(format!("gateway-page workspace-hub {class}"))
            .fill((
                row().class("hub-tabs actions").fill(
                    tabs.iter()
                        .map(|(label, path)| tab(ctx, label, path))
                        .collect::<Vec<_>>(),
                ),
                col().class("hub-content").fill(content),
            )),
    )
}

fn tab(ctx: &AppContext, label: &str, target: &str) -> Box<dyn Widget> {
    let target = target.to_owned();
    let selected = target.clone();
    let current = ctx.path;
    let context = ctx.clone();
    let label = crate::ui::label(ctx, label.to_owned());
    Box::new(
        sapid::anchor()
            .unstyled()
            .href(target.clone())
            .name(label.clone())
            .class(current.map(move |path| {
                let path = path.split('?').next().unwrap_or(path);
                let active = path == selected
                    || selected == "/devices" && path.starts_with("/devices/nodes/");
                if active {
                    "btn hub-tab active".into()
                } else {
                    "btn hub-tab".into()
                }
            }))
            .fill(text().content(label))
            .on_navigate(move |_| context.navigate(&target)),
    )
}
