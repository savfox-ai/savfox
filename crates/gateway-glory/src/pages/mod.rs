//! Glory + Sapid gateway page dispatch.
pub mod agents;
pub mod approvals;
pub mod channels;
pub(crate) mod common;
pub mod config;
pub mod cron;
pub mod debug;
pub mod device_pair;
pub mod instances;
pub mod logs;
pub mod models;
pub mod nodes;
pub mod overview;
pub mod settings;
pub mod skills;
pub mod tts;
pub mod usage;
pub mod voice;

use glory::prelude::*;

use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let full_path = path;
    let path = path.split('?').next().unwrap_or("/").trim_end_matches('/');
    match path {
        "" => overview::render(ctx),
        p if p == "/sessions" || p.starts_with("/sessions/") => crate::chat::render(ctx, full_path),
        p if p == "/agents" || p.starts_with("/agents/") => agents::render(ctx, path),
        p if p == "/channels" || p.starts_with("/channels/") => channels::render(ctx, path),
        p if p == "/models" || p.starts_with("/models/") => models::render(ctx, path),
        p if p == "/config" || p.starts_with("/config/") => config::render(ctx, path),
        p if p == "/cron" || p.starts_with("/cron/") => cron::render(ctx, path),
        p if p == "/nodes" || p.starts_with("/nodes/") => nodes::render(ctx, path),
        "/approvals" => approvals::render(ctx),
        "/debug" => debug::render(ctx),
        "/device-pair" => device_pair::render(ctx),
        "/instances" => instances::render(ctx),
        "/logs" => logs::render(ctx),
        "/settings" => settings::render(ctx),
        "/skills" => skills::render(ctx),
        "/tts" => tts::render(ctx),
        "/usage" => usage::render(ctx),
        "/voice" => voice::render(ctx),
        _ => crate::ui::page(
            ui::label_for(locale, "Page not found"),
            ui::label_for(locale, "The requested page does not exist."),
            vec![common::nav(ctx, "Overview", "/")],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ws::WsRpc;
    use crate::i18n::Locale;
    #[test]
    fn all_management_routes_render_without_browser_side_effects() {
        let paths = [
            "/",
            "/sessions",
            "/sessions?session_id=example",
            "/agents",
            "/agents/new",
            "/agents/example",
            "/agents/example/files",
            "/agents/example/tools",
            "/agents/example/skills",
            "/models",
            "/models/connect-provider",
            "/config",
            "/config/gateway",
            "/cron",
            "/cron/new",
            "/cron/example",
            "/nodes",
            "/nodes/example",
            "/approvals",
            "/debug",
            "/device-pair",
            "/instances",
            "/logs",
            "/settings",
            "/skills",
            "/tts",
            "/usage",
            "/voice",
            "/not-found",
        ];
        for path in paths {
            let html = glory::ssr::render_with(|scope| {
                let context = AppContext {
                    scope: scope.clone(),
                    rpc: WsRpc::new(),
                    connected: scope.cage(false),
                    locale: scope.cage(Locale::En),
                    path: scope.cage(path.to_owned()),
                    authenticated: scope.cage(true),
                };
                render(&context, path)
            })
            .unwrap_or_else(|error| panic!("route {path} failed: {error}"));
            assert!(
                html.contains(if path.starts_with("/sessions") {
                    "session-page"
                } else {
                    "gateway-page"
                }),
                "route {path} did not render page container"
            );
        }
    }
    #[test]
    fn restored_management_pages_keep_reference_layout_regions() {
        for (path, expected) in [
            (
                "/",
                vec![
                    "ov-hero",
                    "ov-hero__aside",
                    "stats-grid",
                    "ov-system-grid",
                    "ov-trio-grid",
                ],
            ),
            (
                "/agents",
                vec![
                    "split-view__list",
                    "agent-list-scroll",
                    "split-view__detail",
                    "agent-empty-card",
                ],
            ),
            ("/models", vec!["models-toolbar", "models-current-default"]),
            (
                "/config",
                vec![
                    "config-sidebar",
                    "config-nav",
                    "config-actions",
                    "config-scroll",
                ],
            ),
        ] {
            let html = glory::ssr::render_with(|scope| {
                let mut context = AppContext::new(scope);
                context.locale = scope.cage(Locale::ZhCn);
                render(&context, path)
            })
            .unwrap_or_else(|error| panic!("reference layout {path}: {error}"));
            for class in expected {
                assert!(
                    html.contains(class),
                    "{path} is missing restored reference region {class}"
                );
            }
        }
    }
    #[test]
    fn localized_form_labels_do_not_reset_the_controlled_value() {
        let app = glory::App::memory();
        let locale = app.scope().cage(Locale::En);
        let value = app.scope().cage("unsaved setting".to_owned());
        app.mount(ui::field(ui::label_for(locale, "Agent name"), value, false))
            .expect("mount localized field");
        locale.revise(|current| *current = Locale::ZhCn);
        app.flush().expect("update translation");
        assert_eq!(value.get(), "unsaved setting");
    }
}
