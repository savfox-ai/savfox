use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::VoiceSettings;
use crate::api::ws::WsRpc;

#[component]
pub fn Voice() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);

    let ws_talk = ws.clone();
    let talk_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_talk.clone();
        async move { ws.call::<serde_json::Value>("talk.mode", None).await.ok() }
    });

    let ws_wake = ws.clone();
    let wake_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_wake.clone();
        async move { ws.call::<VoiceSettings>("voicewake.get", None).await.ok() }
    });

    let talk_read = talk_data.read();
    let talk_mode = talk_read
        .as_ref()
        .and_then(|t| t.as_ref())
        .and_then(|t| t.get("mode"))
        .and_then(|m| m.as_str())
        .unwrap_or("off")
        .to_string();

    let wake_read = wake_data.read();
    let wake = wake_read.as_ref().and_then(|w| w.as_ref());

    let wake_enabled = wake.and_then(|w| w.enabled).unwrap_or(false);
    let wake_word = wake
        .and_then(|w| w.wake_word.as_deref())
        .unwrap_or("-")
        .to_string();
    let sensitivity = wake
        .and_then(|w| w.sensitivity)
        .map(|s| format!("{s:.1}"))
        .unwrap_or_else(|| "-".into());

    let modes = ["off", "push-to-talk", "voice-activity", "continuous"];

    rsx! {
        div { class: "page-content",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;",
                h2 { style: "font-size:20px;font-weight:600;", "Voice & Talk" }
                button {
                    onclick: move |_| refresh_tick += 1,
                    style: "padding:6px 14px;background:var(--bg-tertiary);color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                    "Refresh"
                }
            }

            // Talk Mode
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Talk Mode" }
                div { style: "display:flex;gap:8px;flex-wrap:wrap;",
                    for mode in modes.iter() {
                        {
                            let is_active = talk_mode == *mode;
                            let bg = if is_active { "var(--accent)" } else { "var(--bg-tertiary)" };
                            let color = if is_active { "#fff" } else { "var(--text-secondary)" };
                            let m = mode.to_string();
                            let ws_set = ws.clone();
                            rsx! {
                                button {
                                    key: "{mode}",
                                    onclick: move |_| {
                                        let ws = ws_set.clone();
                                        let mode = m.clone();
                                        spawn(async move {
                                            let _ = ws.call::<serde_json::Value>("talk.mode", Some(json!({"mode": mode}))).await;
                                            refresh_tick += 1;
                                        });
                                    },
                                    style: "padding:8px 16px;background:{bg};color:{color};border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                                    "{mode}"
                                }
                            }
                        }
                    }
                }
                p { style: "font-size:12px;color:var(--text-muted);margin-top:8px;",
                    "Current: "
                    code { "{talk_mode}" }
                }
            }

            // Voice Wake
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Voice Wake" }
                div { style: "display:flex;justify-content:space-between;align-items:center;padding:6px 0;",
                    span { style: "color:var(--text-secondary);font-size:14px;", "Enabled" }
                    {
                        let ws_toggle = ws.clone();
                        let btn_bg = if wake_enabled { "var(--success)" } else { "var(--bg-tertiary)" };
                        let btn_color = if wake_enabled { "#fff" } else { "var(--text-secondary)" };
                        let label = if wake_enabled { "On" } else { "Off" };
                        rsx! {
                            button {
                                onclick: move |_| {
                                    let ws = ws_toggle.clone();
                                    let new_val = !wake_enabled;
                                    spawn(async move {
                                        let _ = ws.call::<serde_json::Value>("voicewake.set", Some(json!({"enabled": new_val}))).await;
                                        refresh_tick += 1;
                                    });
                                },
                                style: "padding:3px 14px;background:{btn_bg};color:{btn_color};border:1px solid var(--border);border-radius:var(--radius);font-size:12px;font-family:monospace;",
                                "{label}"
                            }
                        }
                    }
                }
                div { style: "display:flex;justify-content:space-between;padding:6px 0;",
                    span { style: "color:var(--text-secondary);font-size:14px;", "Wake Word" }
                    span { style: "font-family:monospace;font-size:13px;", "{wake_word}" }
                }
                div { style: "display:flex;justify-content:space-between;padding:6px 0;",
                    span { style: "color:var(--text-secondary);font-size:14px;", "Sensitivity" }
                    span { style: "font-family:monospace;font-size:13px;", "{sensitivity}" }
                }
            }

            // Wake Word Settings
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Configure Wake Word" }
                div { style: "display:flex;flex-direction:column;gap:12px;",
                    div {
                        label { style: "{LABEL}", "Wake Word" }
                        input {
                            value: "{wake_word}",
                            oninput: move |_| {},
                            placeholder: "hey savfox",
                            style: "{INPUT}",
                        }
                    }
                    div {
                        label { style: "{LABEL}", "Sensitivity (0.0 - 1.0)" }
                        input {
                            r#type: "number",
                            value: "{sensitivity}",
                            oninput: move |_| {},
                            step: "0.1",
                            min: "0",
                            max: "1",
                            style: "{INPUT}",
                        }
                    }
                    button {
                        onclick: {
                            let ws = ws.clone();
                            move |_| {
                                let ws = ws.clone();
                                let word = wake_word.clone();
                                let sens = sensitivity.clone();
                                spawn(async move {
                                    let s: f64 = sens.parse().unwrap_or(0.5);
                                    let _ = ws.call::<serde_json::Value>("voicewake.set", Some(json!({
                                        "wake_word": word, "sensitivity": s
                                    }))).await;
                                    refresh_tick += 1;
                                });
                            }
                        },
                        style: "align-self:flex-start;padding:8px 20px;background:var(--accent);color:#fff;border:none;border-radius:var(--radius);font-size:13px;",
                        "Save"
                    }
                }
            }
        }
    }
}

const CARD: &str = "background:var(--bg-secondary);border:1px solid var(--border);border-radius:var(--radius);padding:20px;margin-bottom:16px;";
const CARD_TITLE: &str =
    "font-size:14px;font-weight:600;margin-bottom:12px;color:var(--text-secondary);";
const INPUT: &str = "width:100%;padding:8px 12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);outline:none;font-size:14px;";
const LABEL: &str =
    "display:block;font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:4px;";
