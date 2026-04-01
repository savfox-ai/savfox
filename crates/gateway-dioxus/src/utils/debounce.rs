use dioxus::prelude::*;

/// A debounced string signal.
///
/// `raw` is updated immediately on each keystroke.
/// `value` is updated after `delay_ms` of inactivity.
///
/// Usage:
/// ```rust
/// let search = use_debounced_signal(300);
/// // In input: oninput: move |e| search.set_raw(e.value())
/// // For filtering: search.value()
/// ```
#[derive(Clone, Copy)]
pub struct DebouncedSignal {
    raw: Signal<String>,
    value: Signal<String>,
    delay_ms: u32,
}

impl DebouncedSignal {
    /// Get the debounced (settled) value.
    pub fn value(&self) -> String {
        (self.value)()
    }

    /// Get the raw (immediate) value — useful for displaying in the input.
    pub fn raw(&self) -> String {
        (self.raw)()
    }

    /// Update the raw value and schedule a debounced update.
    pub fn set_raw(&mut self, v: String) {
        self.raw.set(v);
    }
}

/// Create a debounced string signal with the given delay in milliseconds.
pub fn use_debounced_signal(delay_ms: u32) -> DebouncedSignal {
    let raw = use_signal(String::new);
    let mut value = use_signal(String::new);
    let mut pending = use_signal(|| 0u64);
    let mut generation = use_signal(|| 0u64);

    // When raw changes, schedule a delayed update
    use_effect(move || {
        let current = raw();
        generation += 1;
        let my_generation = generation();
        pending.set(my_generation);

        spawn(async move {
            async_sleep(delay_ms).await;
            // Only apply if no newer update has been scheduled
            if pending() == my_generation {
                value.set(current);
            }
        });
    });

    DebouncedSignal {
        raw,
        value,
        delay_ms,
    }
}

/// Async sleep using setTimeout for WASM.
async fn async_sleep(ms: u32) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    let cb = wasm_bindgen::prelude::Closure::once(move || {
        let _ = tx.send(());
    });
    if let Some(w) = web_sys::window() {
        use wasm_bindgen::JsCast;
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            ms as i32,
        );
    }
    let _ = rx.await;
}
