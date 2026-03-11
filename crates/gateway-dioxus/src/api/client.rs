use serde::de::DeserializeOwned;
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use super::ws::{WsRpc, get_token};

fn extract_stream_text_delta(parsed: &Value) -> Option<&str> {
    parsed["choices"][0]["delta"]["content"]
        .as_str()
        .or_else(|| parsed["choices"][0]["message"]["content"].as_str())
        .or_else(|| parsed["delta"]["content"].as_str())
}

fn extract_stream_reasoning_delta(parsed: &Value) -> Option<&str> {
    parsed["choices"][0]["delta"]["reasoning_content"]
        .as_str()
        .or_else(|| parsed["choices"][0]["delta"]["reasoning"].as_str())
        .or_else(|| parsed["delta"]["reasoning_content"].as_str())
}

pub async fn fetch_json<T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<T, String> {
    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    headers.set("Content-Type", "application/json").ok();
    if let Some(token) = get_token() {
        headers
            .set("Authorization", &format!("Bearer {token}"))
            .ok();
    }

    let opts = web_sys::RequestInit::new();
    opts.set_method(method);
    opts.set_headers(&headers.into());
    if let Some(body) = body {
        let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        opts.set_body(&wasm_bindgen::JsValue::from_str(&json));
    }

    let request =
        web_sys::Request::new_with_str_and_init(path, &opts).map_err(|e| format!("{e:?}"))?;

    let window = web_sys::window().ok_or("No window")?;
    let resp = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("{e:?}"))?;

    let response: web_sys::Response = resp.dyn_into().map_err(|_| "Invalid response")?;

    if response.status() == 401 {
        clear_token();
        if let Some(w) = web_sys::window() {
            w.location().reload().ok();
        }
        return Err("Unauthorized".into());
    }

    if !response.ok() {
        let text = JsFuture::from(response.text().map_err(|e| format!("{e:?}"))?)
            .await
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        return Err(format!("{}: {text}", response.status()));
    }

    let text = JsFuture::from(response.text().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("{e:?}"))?
        .as_string()
        .ok_or("Response not a string")?;

    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub async fn validate_token(token: &str) -> bool {
    let headers = match web_sys::Headers::new() {
        Ok(h) => h,
        Err(_) => return false,
    };
    headers.set("Content-Type", "application/json").ok();
    headers
        .set("Authorization", &format!("Bearer {token}"))
        .ok();

    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_headers(&headers.into());

    let request = match web_sys::Request::new_with_str_and_init("/api/token/validate", &opts) {
        Ok(r) => r,
        Err(_) => return false,
    };

    let window = match web_sys::window() {
        Some(w) => w,
        None => return false,
    };

    let resp = match JsFuture::from(window.fetch_with_request(&request)).await {
        Ok(r) => r,
        Err(_) => return false,
    };

    let response: web_sys::Response = match resp.dyn_into() {
        Ok(r) => r,
        Err(_) => return false,
    };

    if !response.ok() {
        return false;
    }

    let text = match response.text() {
        Ok(p) => match JsFuture::from(p).await {
            Ok(v) => v.as_string().unwrap_or_default(),
            Err(_) => return false,
        },
        Err(_) => return false,
    };

    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["valid"].as_bool())
        .unwrap_or(false)
}

pub async fn stream_chat(
    message: Value,
    model: String,
    session_id: Option<String>,
    mut on_chunk: impl FnMut(&str),
    mut on_reasoning: Option<&mut dyn FnMut(&str)>,
    signal: Option<web_sys::AbortSignal>,
) -> Result<(), String> {
    let body_str = serde_json::to_string(&serde_json::json!({
        "model": model,
        "message": message,
        "session_id": session_id,
        "stream": true,
    }))
    .map_err(|e| e.to_string())?;

    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    headers.set("Content-Type", "application/json").ok();
    headers.set("Accept", "text/event-stream").ok();
    headers
        .set("Cache-Control", "no-cache, no-store, must-revalidate")
        .ok();
    headers.set("Pragma", "no-cache").ok();
    if let Some(token) = get_token() {
        headers
            .set("Authorization", &format!("Bearer {token}"))
            .ok();
    }

    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_headers(&headers.into());
    opts.set_body(&wasm_bindgen::JsValue::from_str(&body_str));
    if let Some(ref sig) = signal {
        opts.set_signal(Some(sig));
    }

    let stream_url = format!("/api/chat/completions?ts={}", js_sys::Date::now() as u64);
    let request = web_sys::Request::new_with_str_and_init(&stream_url, &opts)
        .map_err(|e| format!("{e:?}"))?;

    let window = web_sys::window().ok_or("No window")?;
    let resp = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("{e:?}"))?;

    let response: web_sys::Response = resp.dyn_into().map_err(|_| "Invalid response")?;

    if !response.ok() {
        return Err(format!("Chat error: {}", response.status()));
    }

    let body = response.body().ok_or("No response body")?;
    let reader: web_sys::ReadableStreamDefaultReader = body.get_reader().unchecked_into();

    let mut buffer = String::new();

    loop {
        let result = JsFuture::from(reader.read())
            .await
            .map_err(|e| format!("{e:?}"))?;

        let done = js_sys::Reflect::get(&result, &"done".into())
            .unwrap_or(wasm_bindgen::JsValue::TRUE)
            .as_bool()
            .unwrap_or(true);

        if done {
            break;
        }

        let value = js_sys::Reflect::get(&result, &"value".into()).map_err(|_| "No value")?;
        let array = js_sys::Uint8Array::new(&value);
        let bytes = array.to_vec();
        let text = String::from_utf8_lossy(&bytes);

        buffer.push_str(&text);

        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].to_string();
            buffer = buffer[pos + 1..].to_string();

            let line = line.trim();
            if !line.starts_with("data:") {
                continue;
            }
            let data = line[5..].trim_start();

            if data.trim() == "[DONE]" {
                return Ok(());
            }
            if data.is_empty() {
                continue;
            }

            if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                if let Some(delta) = extract_stream_text_delta(&parsed) {
                    on_chunk(delta);
                }
                // Detect reasoning_content for thinking display
                if let Some(reasoning) = extract_stream_reasoning_delta(&parsed) {
                    if let Some(ref mut cb) = on_reasoning {
                        cb(reasoning);
                    }
                }
            }
        }
    }

    Ok(())
}

fn clear_token() {
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
    {
        let _ = storage.remove_item("savfox_token");
    }
}

/// Stream chat via WebSocket for real-time delta delivery.
/// This avoids HTTP buffering issues and provides immediate streaming.
/// Returns the request_id that can be used to match stream events.
pub fn stream_chat_ws(
    ws: &WsRpc,
    message: &str,
    model: &str,
    session_id: Option<&str>,
) -> Result<String, String> {
    if !ws.connected() {
        return Err("WebSocket not connected".into());
    }

    let request_id = format!("req_{}", js_sys::Date::now() as u64);

    // Send the chat.send request via WebSocket
    let params = serde_json::json!({
        "message": message,
        "agent": model,
        "request_id": &request_id,
        "session_id": session_id,
    });

    ws.notify("chat.send", Some(params))?;

    Ok(request_id)
}
