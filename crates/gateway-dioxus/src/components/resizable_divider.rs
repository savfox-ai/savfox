use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// A draggable divider for split-pane layouts.
/// Emits the new left-pane width fraction (0.0–1.0) on drag.
#[component]
pub fn ResizableDivider(on_resize: EventHandler<f64>) -> Element {
    let mut dragging = use_signal(|| false);

    // Set up global mousemove/mouseup listeners when dragging
    use_effect(move || {
        if !dragging() {
            return;
        }

        let move_cb = Closure::wrap(Box::new(move |e: web_sys::MouseEvent| {
            if !dragging() {
                return;
            }
            if let Some(container) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.query_selector(".chat-split-pane").ok())
                .flatten()
            {
                let rect = container.get_bounding_client_rect();
                let x = e.client_x() as f64 - rect.left();
                let fraction = (x / rect.width()).clamp(0.25, 0.85);
                on_resize.call(fraction);
            }
        }) as Box<dyn FnMut(web_sys::MouseEvent)>);

        let up_cb = Closure::wrap(Box::new(move |_: web_sys::MouseEvent| {
            dragging.set(false);
        }) as Box<dyn FnMut(web_sys::MouseEvent)>);

        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ =
                doc.add_event_listener_with_callback("mousemove", move_cb.as_ref().unchecked_ref());
            let _ = doc.add_event_listener_with_callback("mouseup", up_cb.as_ref().unchecked_ref());
        }

        move_cb.forget();
        up_cb.forget();
    });

    rsx! {
        div {
            class: "resizable-divider",
            onmousedown: move |e| {
                e.prevent_default();
                dragging.set(true);
            },
        }
    }
}
