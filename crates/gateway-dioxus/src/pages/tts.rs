use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::TtsStatus;
use crate::api::ws::WsRpc;

/// A single TTS provider entry extracted from the providers response.
#[derive(Clone)]
struct ProviderItem {
    id: String,
    name: String,
    requires_key: Option<String>,
}

#[component]
pub fn Tts() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut convert_text = use_signal(String::new);
    let mut convert_result = use_signal(|| Option::<String>::None);
    let mut converting = use_signal(|| false);
    let mut speed = use_signal(|| 1.0_f64);
    let mut pitch = use_signal(|| 1.0_f64);
    let mut settings_loaded = use_signal(|| false);
    let mut previewing = use_signal(|| false);

    let ws_status = ws.clone();
    let status_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_status.clone();
        async move { ws.call::<TtsStatus>("tts.status", None).await.ok() }
    });

    let ws_providers = ws.clone();
    let providers_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_providers.clone();
        async move {
            ws.call::<serde_json::Value>("tts.providers", None)
                .await
                .ok()
        }
    });

    let status_read = status_data.read();
    let status = status_read.as_ref().and_then(|s| s.as_ref());

    let enabled = status.and_then(|s| s.enabled).unwrap_or(false);
    let has_key = status
        .and_then(|s| s.active_provider_has_key)
        .unwrap_or(false);
    let provider = status
        .and_then(|s| s.provider.as_deref())
        .unwrap_or("-")
        .to_string();
    let voice = status
        .and_then(|s| s.voice.as_deref())
        .unwrap_or("-")
        .to_string();

    // Load speed/pitch from server on first successful status fetch
    if !settings_loaded() {
        if let Some(s) = status {
            if let Some(sp) = s.speed {
                speed.set(sp);
            }
            if let Some(pi) = s.pitch {
                pitch.set(pi);
            }
            settings_loaded.set(true);
        }
    }

    // Fetch voices for current provider
    let ws_voices = ws.clone();
    let current_provider = provider.clone();
    let voices_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_voices.clone();
        let prov = current_provider.clone();
        async move {
            ws.call::<serde_json::Value>("tts.voices", Some(json!({"provider": prov})))
                .await
                .ok()
        }
    });

    let voices_read = voices_data.read();
    let voices_list: Vec<String> = voices_read
        .as_ref()
        .and_then(|v| v.as_ref())
        .and_then(|v| v.get("voices"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Parse providers - backend returns array of objects with id/name/requires_key_env
    let providers_read = providers_data.read();
    let providers_list: Vec<ProviderItem> = providers_read
        .as_ref()
        .and_then(|v| v.as_ref())
        .and_then(|v| v.get("providers"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let id = v.get("id").and_then(|i| i.as_str())?.to_string();
                    let name = v
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or(&id)
                        .to_string();
                    let requires_key = v
                        .get("requires_key_env")
                        .and_then(|k| k.as_str())
                        .map(String::from);
                    Some(ProviderItem {
                        id,
                        name,
                        requires_key,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    rsx! {
        div { class: "page-content",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;",
                h2 { style: "font-size:20px;font-weight:600;", "Text-to-Speech" }
                button {
                    onclick: move |_| {
                        settings_loaded.set(false);
                        refresh_tick += 1;
                    },
                    style: "padding:6px 14px;background:var(--bg-tertiary);color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                    "Refresh"
                }
            }

            // Status card
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Status" }
                div { style: "display:flex;justify-content:space-between;align-items:center;padding:6px 0;",
                    span { style: "color:var(--text-secondary);font-size:14px;", "Enabled" }
                    {
                        let ws_toggle = ws.clone();
                        let btn_bg = if enabled { "var(--success)" } else { "var(--bg-tertiary)" };
                        let btn_color = if enabled { "#fff" } else { "var(--text-secondary)" };
                        let label = if enabled { "On" } else { "Off" };
                        rsx! {
                            button {
                                onclick: move |_| {
                                    let ws = ws_toggle.clone();
                                    let method = if enabled { "tts.disable" } else { "tts.enable" };
                                    spawn(async move {
                                        let _ = ws.call::<serde_json::Value>(method, None).await;
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
                    span { style: "color:var(--text-secondary);font-size:14px;", "Provider" }
                    span { style: "font-family:monospace;font-size:13px;", "{provider}" }
                }
                div { style: "display:flex;justify-content:space-between;padding:6px 0;",
                    span { style: "color:var(--text-secondary);font-size:14px;", "Voice" }
                    span { style: "font-family:monospace;font-size:13px;", "{voice}" }
                }
                if provider != "-" {
                    {
                        let key_color = if has_key { "var(--success)" } else { "var(--warning)" };
                        rsx! {
                            div { style: "display:flex;justify-content:space-between;padding:6px 0;",
                                span { style: "color:var(--text-secondary);font-size:14px;", "API Key" }
                                span {
                                    style: "font-family:monospace;font-size:12px;color:{key_color};",
                                    if has_key { "configured" } else { "not set" }
                                }
                            }
                        }
                    }
                }
            }

            // Provider selector
            if !providers_list.is_empty() {
                div { style: "{CARD}",
                    h3 { style: "{CARD_TITLE}", "Provider" }
                    div { style: "display:flex;gap:8px;flex-wrap:wrap;",
                        for p in providers_list.iter() {
                            {
                                let is_active = provider == p.id;
                                let bg = if is_active { "var(--accent)" } else { "var(--bg-tertiary)" };
                                let color = if is_active { "#fff" } else { "var(--text-secondary)" };
                                let pid = p.id.clone();
                                let pname = p.name.clone();
                                let env_hint = p.requires_key.clone();
                                let ws_set = ws.clone();
                                rsx! {
                                    div { style: "display:flex;flex-direction:column;align-items:center;gap:2px;",
                                        button {
                                            key: "{pid}",
                                            onclick: move |_| {
                                                let ws = ws_set.clone();
                                                let name = pid.clone();
                                                spawn(async move {
                                                    let _ = ws.call::<serde_json::Value>("tts.setProvider", Some(json!({"provider": name}))).await;
                                                    settings_loaded.set(false);
                                                    refresh_tick += 1;
                                                });
                                            },
                                            style: "padding:6px 16px;background:{bg};color:{color};border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                                            "{pname}"
                                        }
                                        if let Some(ref env) = env_hint {
                                            span { style: "font-size:10px;color:var(--text-muted);", "{env}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Voice selector
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Voice" }
                if voices_list.is_empty() {
                    p { style: "color:var(--text-muted);font-size:13px;",
                        "No voices available for this provider."
                    }
                } else {
                    div { style: "display:flex;gap:8px;flex-wrap:wrap;margin-bottom:12px;",
                        for v in voices_list.iter() {
                            {
                                let is_active = voice == *v;
                                let bg = if is_active { "var(--accent)" } else { "var(--bg-tertiary)" };
                                let color = if is_active { "#fff" } else { "var(--text-secondary)" };
                                let vname = v.clone();
                                let ws_set = ws.clone();
                                rsx! {
                                    button {
                                        key: "{v}",
                                        onclick: move |_| {
                                            let ws = ws_set.clone();
                                            let name = vname.clone();
                                            spawn(async move {
                                                let _ = ws.call::<serde_json::Value>("tts.setVoice", Some(json!({"voice": name}))).await;
                                                refresh_tick += 1;
                                            });
                                        },
                                        style: "padding:4px 12px;background:{bg};color:{color};border:1px solid var(--border);border-radius:var(--radius);font-size:12px;",
                                        "{v}"
                                    }
                                }
                            }
                        }
                    }
                }
                // Preview button
                div { style: "display:flex;gap:8px;align-items:center;",
                    button {
                        disabled: previewing() || voice == "-",
                        onclick: {
                            let ws = ws.clone();
                            move |_| {
                                let ws = ws.clone();
                                previewing.set(true);
                                spawn(async move {
                                    let _ = ws.call::<serde_json::Value>(
                                        "tts.convert",
                                        Some(json!({"text": "Hello, this is a voice preview.", "preview": true})),
                                    ).await;
                                    previewing.set(false);
                                });
                            }
                        },
                        style: "padding:6px 14px;background:var(--bg-tertiary);color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:12px;",
                        if previewing() { "Playing..." } else { "Preview Voice" }
                    }
                }
            }

            // Speed & Pitch controls
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Settings" }
                div { style: "display:flex;flex-direction:column;gap:16px;",
                    // Speed slider
                    div {
                        div { style: "display:flex;justify-content:space-between;margin-bottom:4px;",
                            label { style: "font-size:13px;color:var(--text-secondary);", "Speed" }
                            span { style: "font-size:12px;font-family:monospace;color:var(--text-muted);", "{speed:.1}x" }
                        }
                        input {
                            r#type: "range",
                            min: "0.5",
                            max: "2.0",
                            step: "0.1",
                            value: "{speed}",
                            style: "width:100%;accent-color:var(--accent);",
                            oninput: move |e| {
                                if let Ok(v) = e.value().parse::<f64>() {
                                    speed.set(v);
                                }
                            },
                        }
                        div { style: "display:flex;justify-content:space-between;font-size:10px;color:var(--text-muted);",
                            span { "0.5x" }
                            span { "1.0x" }
                            span { "2.0x" }
                        }
                    }
                    // Pitch slider
                    div {
                        div { style: "display:flex;justify-content:space-between;margin-bottom:4px;",
                            label { style: "font-size:13px;color:var(--text-secondary);", "Pitch" }
                            span { style: "font-size:12px;font-family:monospace;color:var(--text-muted);", "{pitch:.1}" }
                        }
                        input {
                            r#type: "range",
                            min: "0.5",
                            max: "2.0",
                            step: "0.1",
                            value: "{pitch}",
                            style: "width:100%;accent-color:var(--accent);",
                            oninput: move |e| {
                                if let Ok(v) = e.value().parse::<f64>() {
                                    pitch.set(v);
                                }
                            },
                        }
                        div { style: "display:flex;justify-content:space-between;font-size:10px;color:var(--text-muted);",
                            span { "0.5" }
                            span { "1.0" }
                            span { "2.0" }
                        }
                    }
                    // Apply settings button
                    button {
                        onclick: {
                            let ws = ws.clone();
                            move |_| {
                                let ws = ws.clone();
                                let s = speed();
                                let p = pitch();
                                spawn(async move {
                                    let _ = ws.call::<serde_json::Value>(
                                        "tts.settings",
                                        Some(json!({"speed": s, "pitch": p})),
                                    ).await;
                                    refresh_tick += 1;
                                });
                            }
                        },
                        style: "padding:6px 16px;background:var(--accent);color:#fff;border:none;border-radius:var(--radius);font-size:13px;align-self:flex-start;",
                        "Apply Settings"
                    }
                }
            }

            // Convert form
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Convert Text" }
                textarea {
                    value: "{convert_text}",
                    oninput: move |e| convert_text.set(e.value()),
                    placeholder: "Enter text to convert to speech...",
                    rows: 4,
                    style: "width:100%;padding:10px 12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);outline:none;font-size:14px;resize:vertical;margin-bottom:12px;",
                }
                div { style: "display:flex;gap:8px;align-items:center;",
                    button {
                        disabled: converting(),
                        onclick: {
                            let ws = ws.clone();
                            move |_| {
                                let text = convert_text();
                                if text.trim().is_empty() { return; }
                                let ws = ws.clone();
                                converting.set(true);
                                convert_result.set(None);
                                spawn(async move {
                                    let s = speed();
                                    let p = pitch();
                                    match ws.call::<serde_json::Value>("tts.convert", Some(json!({"text": text, "speed": s, "pitch": p}))).await {
                                        Ok(v) => {
                                            let path = v.get("audio_path").and_then(|p| p.as_str()).unwrap_or("(no path)");
                                            convert_result.set(Some(format!("Audio saved: {path}")));
                                        }
                                        Err(e) => {
                                            convert_result.set(Some(format!("Error: {e}")));
                                        }
                                    }
                                    converting.set(false);
                                });
                            }
                        },
                        style: "padding:8px 20px;background:var(--accent);color:#fff;border:none;border-radius:var(--radius);font-size:13px;",
                        if converting() { "Converting..." } else { "Convert" }
                    }
                    if let Some(ref result) = convert_result() {
                        span { style: "font-size:13px;color:var(--text-secondary);", "{result}" }
                    }
                }
            }
        }
    }
}

const CARD: &str = "background:var(--bg-secondary);border:1px solid var(--border);border-radius:var(--radius);padding:20px;margin-bottom:16px;";
const CARD_TITLE: &str =
    "font-size:14px;font-weight:600;margin-bottom:12px;color:var(--text-secondary);";
