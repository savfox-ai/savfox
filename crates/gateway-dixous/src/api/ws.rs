use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use dioxus::prelude::*;
use futures::channel::oneshot;
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, WebSocket};

use super::types::{JsonRpcNotification, JsonRpcResponse};

// ── localStorage helpers ────────────────────────────────────────────

pub fn get_token() -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item("savfox_token")
        .ok()?
}

pub fn set_token(token: &str) {
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
    {
        let _ = storage.set_item("savfox_token", token);
    }
}

pub fn clear_token() {
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
    {
        let _ = storage.remove_item("savfox_token");
    }
}

// ── Notification handler type ───────────────────────────────────────

type NotificationHandler = Box<dyn Fn(Value)>;

// ── WebSocket JSON-RPC client ───────────────────────────────────────

#[derive(Clone)]
pub struct WsRpc {
    inner: Rc<WsRpcInner>,
}

impl PartialEq for WsRpc {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

struct WsRpcInner {
    ws: RefCell<Option<WebSocket>>,
    next_id: Cell<u64>,
    pending: RefCell<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    notification_handlers: RefCell<HashMap<String, Vec<NotificationHandler>>>,
    reconnect_delay: Cell<u32>,
    reconnect_scheduled: Cell<bool>,
    manual_disconnect: Cell<bool>,
    closures: RefCell<Vec<JsValue>>,
}

impl WsRpc {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(WsRpcInner {
                ws: RefCell::new(None),
                next_id: Cell::new(1),
                pending: RefCell::new(HashMap::new()),
                notification_handlers: RefCell::new(HashMap::new()),
                reconnect_delay: Cell::new(1000),
                reconnect_scheduled: Cell::new(false),
                manual_disconnect: Cell::new(false),
                closures: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Register a handler for server-push notifications (messages with `method` but no `id`).
    pub fn on_notification(&self, method: &str, handler: impl Fn(Value) + 'static) {
        self.inner
            .notification_handlers
            .borrow_mut()
            .entry(method.to_string())
            .or_default()
            .push(Box::new(handler));
    }

    /// Register a mutable handler for server-push notifications.
    pub fn on_notification_mut(&self, method: &str, handler: impl FnMut(Value) + 'static) {
        // Use RefCell to allow FnMut in Fn context
        let handler = std::cell::RefCell::new(handler);
        self.inner
            .notification_handlers
            .borrow_mut()
            .entry(method.to_string())
            .or_default()
            .push(Box::new(move |v| {
                if let Ok(mut h) = handler.try_borrow_mut() {
                    h(v);
                }
            }));
    }

    pub fn connected(&self) -> bool {
        self.inner
            .ws
            .borrow()
            .as_ref()
            .map(|ws| ws.ready_state() == WebSocket::OPEN)
            .unwrap_or(false)
    }

    /// Wait for the WebSocket connection to be established, polling up to ~5 seconds.
    pub async fn wait_connected(&self) {
        for _ in 0..50 {
            if self.connected() {
                return;
            }
            let promise = js_sys::Promise::new(&mut |resolve, _| {
                if let Some(win) = web_sys::window() {
                    let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &resolve, 100,
                    );
                }
            });
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        }
    }

    fn schedule_reconnect(&self, mut connected_signal: Signal<bool>, reconnect_epoch: Signal<u64>) {
        if self.inner.manual_disconnect.get() || self.inner.reconnect_scheduled.get() {
            return;
        }

        connected_signal.set(false);
        let delay = self.inner.reconnect_delay.get();
        self.inner.reconnect_delay.set((delay * 2).min(30_000));
        self.inner.reconnect_scheduled.set(true);

        let sc = self.clone();
        let reconnect = Closure::once(move || {
            sc.inner.reconnect_scheduled.set(false);
            sc.connect(connected_signal, reconnect_epoch);
        });

        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                reconnect.as_ref().unchecked_ref(),
                delay as i32,
            );
            reconnect.forget();
        } else {
            self.inner.reconnect_scheduled.set(false);
        }
    }

    pub fn connect(&self, mut connected_signal: Signal<bool>, mut reconnect_epoch: Signal<u64>) {
        self.inner.manual_disconnect.set(false);

        // Don't reconnect if already open or connecting.
        if let Some(ws) = self.inner.ws.borrow().as_ref() {
            let state = ws.ready_state();
            if state == WebSocket::OPEN || state == WebSocket::CONNECTING {
                return;
            }
        }

        let window = match web_sys::window() {
            Some(w) => w,
            None => return,
        };
        let location = window.location();
        let protocol = if location.protocol().unwrap_or_default() == "https:" {
            "wss:"
        } else {
            "ws:"
        };
        let host = location.host().unwrap_or_default();
        let token = get_token().unwrap_or_default();
        let url = format!("{protocol}//{host}/ws?token={token}");

        let ws = match WebSocket::new(&url) {
            Ok(ws) => ws,
            Err(_) => {
                self.schedule_reconnect(connected_signal, reconnect_epoch);
                return;
            }
        };

        self.inner.closures.borrow_mut().clear();

        // onopen
        let inner = self.inner.clone();
        let mut on_open_connected = connected_signal;
        let mut on_open_epoch = reconnect_epoch;
        let on_open = Closure::wrap(Box::new(move || {
            inner.reconnect_delay.set(1000);
            inner.reconnect_scheduled.set(false);
            on_open_connected.set(true);
            on_open_epoch += 1;
        }) as Box<dyn FnMut()>);
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        self.inner
            .closures
            .borrow_mut()
            .push(on_open.into_js_value());

        // onmessage
        let inner = self.inner.clone();
        let on_message = Closure::wrap(Box::new(move |e: MessageEvent| {
            let data = e.data();
            if let Some(text) = data.as_string() {
                // Try as JSON-RPC response (has `id` field) first.
                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&text) {
                    if let Some(tx) = inner.pending.borrow_mut().remove(&resp.id) {
                        if let Some(err) = resp.error {
                            let _ = tx.send(Err(err.message));
                        } else {
                            let _ = tx.send(Ok(resp.result.unwrap_or(Value::Null)));
                        }
                    }
                } else if let Ok(notif) = serde_json::from_str::<JsonRpcNotification>(&text) {
                    // Server-push notification (has `method` but no `id`).
                    let handlers = inner.notification_handlers.borrow();
                    if let Some(list) = handlers.get(&notif.method) {
                        let params = notif.params.unwrap_or(Value::Null);
                        for handler in list {
                            handler(params.clone());
                        }
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        self.inner
            .closures
            .borrow_mut()
            .push(on_message.into_js_value());

        // onclose
        let self_clone = self.clone();
        let mut on_close_connected = connected_signal;
        let on_close_epoch = reconnect_epoch;
        let on_close = Closure::wrap(Box::new(move || {
            on_close_connected.set(false);
            // Reject all pending requests.
            for (_, tx) in self_clone.inner.pending.borrow_mut().drain() {
                let _ = tx.send(Err("Connection closed".into()));
            }
            self_clone.schedule_reconnect(on_close_connected, on_close_epoch);
        }) as Box<dyn FnMut()>);
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        self.inner
            .closures
            .borrow_mut()
            .push(on_close.into_js_value());

        // onerror – just close so the onclose handler can reconnect.
        let ws_err = ws.clone();
        let on_error = Closure::wrap(Box::new(move || {
            ws_err.close().ok();
        }) as Box<dyn FnMut()>);
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        self.inner
            .closures
            .borrow_mut()
            .push(on_error.into_js_value());

        *self.inner.ws.borrow_mut() = Some(ws);
    }

    pub fn disconnect(&self) {
        self.inner.manual_disconnect.set(true);
        self.inner.reconnect_scheduled.set(false);
        if let Some(ws) = self.inner.ws.borrow_mut().take() {
            ws.set_onopen(None);
            ws.set_onmessage(None);
            ws.set_onclose(None);
            ws.set_onerror(None);
            ws.close().ok();
        }
        self.inner.closures.borrow_mut().clear();
    }

    pub async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T, String> {
        let id = self.inner.next_id.get();
        self.inner.next_id.set(id + 1);

        let (tx, rx) = oneshot::channel();
        self.inner.pending.borrow_mut().insert(id, tx);

        // Send within a limited borrow scope.
        {
            let ws_guard = self.inner.ws.borrow();
            let ws = ws_guard.as_ref().ok_or_else(|| {
                self.inner.pending.borrow_mut().remove(&id);
                "WebSocket not connected".to_string()
            })?;
            if ws.ready_state() != WebSocket::OPEN {
                self.inner.pending.borrow_mut().remove(&id);
                return Err("WebSocket not connected".into());
            }

            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            });

            ws.send_with_str(&serde_json::to_string(&request).map_err(|e| e.to_string())?)
                .map_err(|e| {
                    self.inner.pending.borrow_mut().remove(&id);
                    format!("{e:?}")
                })?;
        }

        // Await the response (borrow is released).
        let result = rx.await.map_err(|_| "Channel closed".to_string())??;
        serde_json::from_value(result).map_err(|e| e.to_string())
    }

    /// Send a fire-and-forget notification (no id, no response expected).
    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), String> {
        let ws_guard = self.inner.ws.borrow();
        let ws = ws_guard.as_ref().ok_or("WebSocket not connected")?;
        if ws.ready_state() != WebSocket::OPEN {
            return Err("WebSocket not connected".into());
        }

        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        ws.send_with_str(&serde_json::to_string(&notification).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{e:?}"))
    }

    /// Remove a notification handler for a method.
    pub fn off_notification(&self, method: &str) {
        self.inner.notification_handlers.borrow_mut().remove(method);
    }
}
