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

use crate::ui::AppContext;
pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
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
            "Page not found",
            "The requested page does not exist.",
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
                html.contains("gateway-page"),
                "route {path} did not render page container"
            );
        }
    }
}
