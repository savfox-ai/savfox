use dioxus::prelude::*;
use serde_json::json;

use crate::api::ws::WsRpc;

/// Response from the pairing.generate RPC call.
#[derive(Clone, Debug, serde::Deserialize)]
struct PairingResponse {
    code: Option<String>,
}

/// Device pairing page that displays a pairing code and QR code.
#[component]
pub fn DevicePair() -> Element {
    let ws = use_context::<WsRpc>();
    let mut pairing_code = use_signal(|| None::<String>);
    let mut pairing_status = use_signal(|| "waiting".to_string());

    // Request a pairing code from the server
    let generate_code = move |_| {
        let ws = ws.clone();
        spawn(async move {
            let result: Result<PairingResponse, String> =
                ws.call("pairing.generate", Some(json!({}))).await;
            match result {
                Ok(resp) => {
                    if let Some(code) = resp.code {
                        pairing_code.set(Some(code));
                        pairing_status.set("pending".to_string());
                    }
                }
                Err(e) => {
                    pairing_status.set(format!("error: {e}"));
                }
            }
        });
    };

    rsx! {
        div {
            class: "page-container",
            style: "padding: 24px; max-width: 600px; margin: 0 auto; text-align: center;",

            h2 {
                style: "margin-bottom: 16px;",
                "Device Pairing"
            }
            p {
                style: "color: var(--text-secondary, #aaa); margin-bottom: 24px;",
                "Pair a new device to access this gateway remotely."
            }

            if let Some(code) = pairing_code() {
                div {
                    style: "background: var(--surface-alt, #1a1a2e); border-radius: 12px; padding: 32px; margin: 16px 0;",
                    p {
                        style: "font-size: 0.9em; color: var(--text-secondary, #aaa); margin-bottom: 12px;",
                        "Enter this code on the new device:"
                    }
                    div {
                        style: "font-size: 2.5em; font-family: monospace; font-weight: 700; letter-spacing: 0.2em; color: var(--accent, #4fc3f7);",
                        "{code}"
                    }
                    p {
                        style: "margin-top: 12px; font-size: 0.8em; color: var(--text-muted, #666);",
                        "Status: {pairing_status}"
                    }
                }
            } else {
                button {
                    style: "background: var(--accent, #4fc3f7); color: #000; border: none; border-radius: 8px; padding: 12px 24px; font-size: 1em; cursor: pointer; font-weight: 600;",
                    onclick: generate_code,
                    "Generate Pairing Code"
                }
            }
        }
    }
}
