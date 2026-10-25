use glory::prelude::*;

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let state = query(ctx, "logs.tail", None);
    let filter = ctx.scope.cage(String::new());
    let level = ctx.scope.cage(String::new());
    let value = state.value;
    ui::localized_page(
        ctx,
        "nav.logs",
        "Gateway log output",
        vec![
            status(ctx, &state),
            refresh(ctx, &state),
            ui::field("Search logs", filter, false),
            choice(
                "Level",
                level,
                &[
                    ("", "All"),
                    ("error", "Error"),
                    ("warn", "Warning"),
                    ("info", "Info"),
                    ("debug", "Debug"),
                    ("trace", "Trace"),
                ],
            ),
            ui::action("Export filtered logs", move || {
                let entries = array(&value.get(), &["entries", "logs", "lines"])
                    .into_iter()
                    .filter(|entry| {
                        (level.get().is_empty()
                            || entry["level"]
                                .as_str()
                                .is_some_and(|v| v.eq_ignore_ascii_case(&level.get())))
                            && (filter.get().is_empty()
                                || entry
                                    .to_string()
                                    .to_lowercase()
                                    .contains(&filter.get().to_lowercase()))
                    })
                    .collect::<Vec<_>>();
                crate::utils::download::trigger_download(
                    "savfox-logs.json",
                    &serde_json::to_string_pretty(&entries).unwrap_or_default(),
                    "application/json",
                );
            }),
            Box::new(text().class("log-output").content(ctx.scope.bond(move || {
                array(&value.get(), &["entries", "logs", "lines"])
                    .iter()
                    .filter(|entry| {
                        (level.get().is_empty()
                            || entry["level"]
                                .as_str()
                                .is_some_and(|v| v.eq_ignore_ascii_case(&level.get())))
                            && (filter.get().is_empty()
                                || entry
                                    .to_string()
                                    .to_lowercase()
                                    .contains(&filter.get().to_lowercase()))
                    })
                    .map(|entry| {
                        if let Some(line) = entry.as_str() {
                            line.to_owned()
                        } else {
                            format!(
                                "{} {:5} {}",
                                string(entry, &["timestamp", "time"]),
                                string(entry, &["level"]),
                                string(entry, &["message", "text"])
                            )
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }))),
        ],
    )
}
