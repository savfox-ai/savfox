//! Shared building blocks for the per-channel status pages.
//!
//! Most channel pages render the same structure: title + icon, load status
//! over `channels.status`, render Configured/Running/(Connected) stats, plus
//! Start/Stop/Refresh buttons. The differences are limited to the channel id,
//! display strings, status type, and a small set of extra stat rows. To
//! collapse that boilerplate we expose:
//!
//! - [`ChannelStatusView`] — trait every status type implements to project the bits the shared
//!   renderer needs.
//! - [`channel_status_page!`] — macro that emits a `#[component] pub fn <Name>Channel() -> Element`
//!   for a given status type.
//! - [`UnimplementedChannel`] — placeholder component for channels whose page is still a stub.

use dioxus::prelude::*;

use crate::components::skeleton::*;

/// Projection a per-channel `*Status` type provides for the shared renderer.
pub trait ChannelStatusView {
    fn configured(&self) -> Option<bool>;
    fn running(&self) -> Option<bool>;
    fn connected(&self) -> Option<bool> {
        None
    }
    fn last_error(&self) -> Option<String>;
    /// Optional name shown on the card header; falls back to the channel title.
    fn display_name(&self) -> Option<String> {
        None
    }
    /// Extra `(label, value)` rows shown after the standard three.
    fn extra_stats(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// Placeholder component for channels whose configuration page is still a stub.
#[component]
pub fn UnimplementedChannel(title: String, icon_html: String) -> Element {
    rsx! {
        div { class: "channels-page",
            div { class: "channels-toolbar",
                div { class: "channels-toolbar__left",
                    h2 { class: "channels-toolbar__title",
                        span { class: "channel-icon", dangerous_inner_html: icon_html }
                        "{title}"
                    }
                    p { class: "channels-toolbar__subtitle",
                        "This channel can be configured from the channel list. A dedicated UI is coming soon."
                    }
                }
            }
        }
    }
}

/// Generate a `#[component] pub fn <Name>Channel() -> Element` for a status type.
///
/// The status type must implement [`ChannelStatusView`].
#[macro_export]
#[rustfmt::skip]
macro_rules! channel_status_page {
    (
        component: $component_name:ident,
        status: $status_ty:ty,
        channel_id: $channel_id:expr,
        title: $title:expr,
        icon: $icon:expr,
        subtitle: $subtitle:expr,
    ) => {
        #[::dioxus::prelude::component]
        pub fn $component_name() -> ::dioxus::prelude::Element {
            $crate::pages::channels::common::render_channel_status_page::<$status_ty>(
                $channel_id,
                $title,
                $icon,
                $subtitle,
            )
        }
    };
}

/// Shared rendering core. Per-channel components delegate here.
#[allow(clippy::too_many_arguments)]
pub fn render_channel_status_page<S>(
    channel_id: &'static str,
    title: &'static str,
    icon_html: &'static str,
    subtitle: &'static str,
) -> Element
where
    S: ChannelStatusView + serde::de::DeserializeOwned + Clone + PartialEq + 'static,
{
    use crate::api::ws::WsRpc;
    use crate::components::chip::{Chip, ChipVariant};

    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<S>);
    let mut loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut action_loading = use_signal(|| false);

    let load_status = move || {
        let ws = ws;
        async move {
            loading.set(true);
            error.set(None);
            let result = ws
                .read()
                .call::<S>(
                    "channels.status",
                    Some(serde_json::json!({"channel": channel_id})),
                )
                .await;
            match result {
                Ok(s) => status.set(Some(s)),
                Err(e) => error.set(Some(e)),
            }
            loading.set(false);
        }
    };

    use_effect(move || {
        spawn(async move {
            load_status().await;
        });
    });

    let snapshot = status.read().clone();

    rsx! {
        div { class: "channels-page",
            div { class: "channels-toolbar",
                div { class: "channels-toolbar__left",
                    h2 { class: "channels-toolbar__title",
                        span { class: "channel-icon", dangerous_inner_html: icon_html }
                        "{title}"
                    }
                    p { class: "channels-toolbar__subtitle", "{subtitle}" }
                }
            }

            if *loading.read() {
                SkeletonCard {}
            }

            if let Some(err) = error.read().as_ref() {
                div { class: "callout danger", "{err}" }
            }

            if let Some(s) = snapshot.as_ref() {
                {
                    let configured = s.configured().unwrap_or(false);
                    let running = s.running().unwrap_or(false);
                    let connected_opt = s.connected();
                    let connected = connected_opt.unwrap_or(false);
                    let last_error = s.last_error();
                    let name = s.display_name().unwrap_or_else(|| title.to_string());
                    let extra = s.extra_stats();

                    let (variant, label) = if connected_opt.is_some() {
                        if running && connected {
                            (ChipVariant::Success, "Connected")
                        } else if running {
                            (ChipVariant::Warning, "Running")
                        } else if configured {
                            (ChipVariant::Warning, "Configured")
                        } else {
                            (ChipVariant::Muted, "Not configured")
                        }
                    } else if running {
                        (ChipVariant::Success, "Running")
                    } else if configured {
                        (ChipVariant::Warning, "Configured")
                    } else {
                        (ChipVariant::Muted, "Not configured")
                    };

                    rsx! {
                        div { class: "channels-grid",
                            div { class: "channels-card",
                                div { class: "channels-card__header",
                                    div { class: "channels-card__identity",
                                        div { class: "channels-card__name", "{name}" }
                                    }
                                    div { class: "channels-card__status",
                                        Chip { label: label.to_string(), variant: variant }
                                    }
                                }

                                div { class: "channels-card__stats",
                                    div { class: "channels-card__stat",
                                        span { class: "channels-card__stat-label", "Configured" }
                                        span { class: "channels-card__stat-value",
                                            if configured { "Yes" } else { "No" }
                                        }
                                    }
                                    div { class: "channels-card__stat",
                                        span { class: "channels-card__stat-label", "Running" }
                                        span { class: "channels-card__stat-value",
                                            if running { "Yes" } else { "No" }
                                        }
                                    }
                                    if connected_opt.is_some() {
                                        div { class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "Connected" }
                                            span { class: "channels-card__stat-value",
                                                if connected { "Yes" } else { "No" }
                                            }
                                        }
                                    }
                                    for (k, v) in extra.into_iter() {
                                        div { class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "{k}" }
                                            span { class: "channels-card__stat-value", "{v}" }
                                        }
                                    }
                                }

                                if let Some(err) = last_error.as_ref() {
                                    div { class: "callout danger", style: "margin-top: 12px;", "{err}" }
                                }

                                div { class: "channels-card__actions", style: "margin-top: 16px;",
                                    if running {
                                        button {
                                            class: "btn btn-danger",
                                            disabled: *action_loading.read(),
                                            onclick: move |_| {
                                                let ws = ws;
                                                async move {
                                                    action_loading.set(true);
                                                    let _ = ws.read()
                                                        .call::<serde_json::Value>(
                                                            "channels.logout",
                                                            Some(serde_json::json!({"channel": channel_id})),
                                                        )
                                                        .await;
                                                    action_loading.set(false);
                                                    load_status().await;
                                                }
                                            },
                                            if *action_loading.read() { "Stopping..." } else { "Stop Channel" }
                                        }
                                    } else {
                                        button {
                                            class: "btn btn-success",
                                            disabled: *action_loading.read(),
                                            onclick: move |_| {
                                                let ws = ws;
                                                async move {
                                                    action_loading.set(true);
                                                    let _ = ws.read()
                                                        .call::<serde_json::Value>(
                                                            "channels.login",
                                                            Some(serde_json::json!({"channel": channel_id})),
                                                        )
                                                        .await;
                                                    action_loading.set(false);
                                                    load_status().await;
                                                }
                                            },
                                            if *action_loading.read() { "Starting..." } else { "Start Channel" }
                                        }
                                    }
                                    button {
                                        class: "btn channels-btn",
                                        onclick: move |_| {
                                            spawn(async move {
                                                load_status().await;
                                            });
                                        },
                                        span { dangerous_inner_html: crate::utils::icons::ICON_REFRESH_CW }
                                        " Refresh"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
