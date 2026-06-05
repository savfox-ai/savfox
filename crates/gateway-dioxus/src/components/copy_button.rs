use dioxus::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[component]
pub fn CopyButton(text: String) -> Element {
    let mut copied = use_signal(|| false);

    rsx! {
        button {
            class: "copy-btn",
            title: "Copy to clipboard",
            onclick: {
                let text = text.clone();
                move |_| {
                    let text = text.clone();
                    spawn(async move {
                        if let Some(window) = web_sys::window() {
                            if let Ok(nav) = js_sys::Reflect::get(
                                &window,
                                &wasm_bindgen::JsValue::from_str("navigator"),
                            ) {
                                if let Ok(clipboard) = js_sys::Reflect::get(
                                    &nav,
                                    &wasm_bindgen::JsValue::from_str("clipboard"),
                                ) {
                                    if let Ok(promise) = js_sys::Reflect::get(
                                        &clipboard,
                                        &wasm_bindgen::JsValue::from_str("writeText"),
                                    ) {
                                        let func: js_sys::Function = promise.into();
                                        if let Ok(p) = func.call1(
                                            &clipboard,
                                            &wasm_bindgen::JsValue::from_str(&text),
                                        ) {
                                            let _ = JsFuture::from(js_sys::Promise::from(p)).await;
                                        }
                                    }
                                }
                            }
                        }
                        copied.set(true);
                        // Reset after 2s
                        crate::utils::sleep_ms(2000).await;
                        copied.set(false);
                    });
                }
            },
            if copied() { "Copied!" } else { "Copy" }
        }
    }
}
