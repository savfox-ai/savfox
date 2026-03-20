use dioxus::prelude::*;
use lucide_dioxus::{CircleCheck, CircleX, TriangleAlert, Info};

#[derive(Clone, Debug, PartialEq)]
pub enum ToastVariant {
    Success,
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub variant: ToastVariant,
    pub message: String,
    pub duration_ms: u32,
}

/// Global toast state — provide via `use_context_provider`.
#[derive(Clone, Copy)]
pub struct Toaster {
    items: Signal<Vec<Toast>>,
    next_id: Signal<u64>,
}

impl Toaster {
    pub fn new() -> Self {
        Self {
            items: Signal::new(Vec::new()),
            next_id: Signal::new(1),
        }
    }

    pub fn show(&mut self, variant: ToastVariant, message: impl Into<String>, duration_ms: u32) {
        let id = (self.next_id)();
        self.next_id.set(id + 1);
        let toast = Toast {
            id,
            variant,
            message: message.into(),
            duration_ms,
        };
        self.items.write().push(toast);

        // Schedule auto-dismiss
        let mut items = self.items;
        spawn(async move {
            async_sleep(duration_ms).await;
            items.write().retain(|t| t.id != id);
        });
    }

    pub fn success(&mut self, message: impl Into<String>) {
        self.show(ToastVariant::Success, message, 5000);
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.show(ToastVariant::Error, message, 8000);
    }

    pub fn warning(&mut self, message: impl Into<String>) {
        self.show(ToastVariant::Warning, message, 6000);
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.show(ToastVariant::Info, message, 5000);
    }

    pub fn dismiss(&mut self, id: u64) {
        self.items.write().retain(|t| t.id != id);
    }
}

/// Renders the global toast container. Place once at app root.
#[component]
pub fn ToastContainer() -> Element {
    let toaster = use_context::<Toaster>();
    let items = toaster.items.read();

    if items.is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "toast-container", role: "alert", aria_live: "assertive", aria_atomic: "true",
            for toast in items.iter() {
                {
                    let variant_class = match toast.variant {
                        ToastVariant::Success => "toast toast--success",
                        ToastVariant::Error => "toast toast--error",
                        ToastVariant::Warning => "toast toast--warning",
                        ToastVariant::Info => "toast toast--info",
                    };
                    let id = toast.id;
                    let mut toaster = toaster;
                    let variant = toast.variant.clone();
                    rsx! {
                        div {
                            key: "{id}",
                            class: "{variant_class}",
                            span { class: "toast__icon",
                                match variant {
                                    ToastVariant::Success => rsx! { CircleCheck { size: 16 } },
                                    ToastVariant::Error => rsx! { CircleX { size: 16 } },
                                    ToastVariant::Warning => rsx! { TriangleAlert { size: 16 } },
                                    ToastVariant::Info => rsx! { Info { size: 16 } },
                                }
                            }
                            span { class: "toast__message", "{toast.message}" }
                            button {
                                class: "toast__close",
                                onclick: move |_| toaster.dismiss(id),
                                "x"
                            }
                        }
                    }
                }
            }
        }
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
    cb.forget();
    let _ = rx.await;
}
