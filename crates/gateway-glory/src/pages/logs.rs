use glory::prelude::*;

use super::common::*;
use crate::ui::{self, AppContext};

pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "logs.tail", None);
    let filter = ctx.scope.cage(String::new());
    let level = ctx.scope.cage(String::new());
    let value = state.value;
    let filtered = ctx.scope.bond(move || {
        let filter = filter.get().to_lowercase();
        let level = level.get();
        array(&value.get(), &["entries", "logs", "lines"])
            .into_iter()
            .filter(|entry| {
                (level.is_empty()
                    || entry["level"]
                        .as_str()
                        .is_some_and(|v| v.eq_ignore_ascii_case(&level)))
                    && (filter.is_empty() || entry.to_string().to_lowercase().contains(&filter))
            })
            .collect::<Vec<_>>()
    });
    let export = ui::action(ui::label_for(locale, "Export filtered logs"), move || {
        crate::utils::download::trigger_download(
            "savfox-logs.json",
            &serde_json::to_string_pretty(&filtered.get()).unwrap_or_default(),
            "application/json",
        );
    });
    let levels = ["info", "warn", "error", "debug", "trace"].map(|name| {
        sapid::button()
            .unstyled()
            .class(level.map(move |current| {
                format!(
                    "logs-level-btn logs-level-btn--{name}{}",
                    if current.is_empty() || current == name {
                        " active"
                    } else {
                        ""
                    }
                )
            }))
            .label(name.to_uppercase())
            .on_press(move || {
                ui::set(
                    level,
                    if level.get() == name {
                        String::new()
                    } else {
                        name.to_owned()
                    },
                )
            })
    });
    Box::new(
        col().class("gateway-page management-page logs-page").fill((
            col().class("logs-toolbar").fill((
                row().class("logs-toolbar__top").fill((
                    row().class("logs-toolbar__title-row").fill((
                        text()
                            .class("page-title logs-title")
                            .kind(TextKind::Heading(1))
                            .content(ui::label_for(locale, "Logs")),
                        text()
                            .class("logs-count-badge")
                            .content(ctx.scope.bond(move || filtered.get().len().to_string())),
                    )),
                    row()
                        .class("logs-toolbar__actions")
                        .fill((export, refresh(ctx, &state))),
                )),
                row().class("logs-toolbar__filters").fill((
                    row()
                        .class("logs-level-toggles")
                        .fill(levels.into_iter().collect::<Vec<_>>()),
                    ui::field(ui::label_for(locale, "Search logs"), filter, false),
                )),
            )),
            status(ctx, &state),
            col()
                .class("logs-list")
                .fill(each(filtered, record_key, move |item, _| {
                    let entry = item.get_untracked();
                    let severity = string(&entry, &["level"]).to_lowercase();
                    let timestamp = string(&entry, &["timestamp", "time"]);
                    let time = timestamp
                        .split('T')
                        .nth(1)
                        .unwrap_or(&timestamp)
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    row()
                        .class(format!("logs-entry logs-entry--{severity}"))
                        .fill((
                            text().class("logs-entry__time").content(time),
                            text()
                                .class(format!("logs-entry__level logs-entry__level--{severity}"))
                                .content(severity.to_uppercase()),
                            text().class("logs-entry__message").content(
                                entry
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| string(&entry, &["message", "text"])),
                            ),
                        ))
                })),
        )),
    )
}
