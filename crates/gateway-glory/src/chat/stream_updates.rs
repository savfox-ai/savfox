//! Batch transport fragments before invalidating the active message's widgets.
use std::cell::RefCell;
use std::rc::Rc;

use glory::Cage;

use crate::api::types::ChatMessage;

#[derive(Clone)]
pub(super) struct StreamUpdates(Rc<RefCell<Pending>>);

struct Pending {
    content: Cage<String>,
    thinking: Cage<Option<String>>,
    text: String,
    reasoning: String,
    #[cfg(target_arch = "wasm32")]
    timer: Option<i32>,
    #[cfg(target_arch = "wasm32")]
    callback: Option<wasm_bindgen::closure::Closure<dyn FnMut()>>,
}

impl StreamUpdates {
    pub(super) fn new(message: Cage<ChatMessage>) -> Self {
        let pending = Rc::new(RefCell::new(Pending {
            content: message.field("content", |m| &m.content, |m| &mut m.content),
            thinking: message.field("thinking", |m| &m.thinking, |m| &mut m.thinking),
            text: String::new(),
            reasoning: String::new(),
            #[cfg(target_arch = "wasm32")]
            timer: None,
            #[cfg(target_arch = "wasm32")]
            callback: None,
        }));
        #[cfg(target_arch = "wasm32")]
        {
            let weak = Rc::downgrade(&pending);
            pending.borrow_mut().callback = Some(wasm_bindgen::closure::Closure::new(move || {
                if let Some(pending) = weak.upgrade() {
                    pending.borrow_mut().timer = None;
                    pending.borrow_mut().apply();
                    crate::ui::flush();
                }
            }));
        }
        Self(pending)
    }

    pub(super) fn push(&self, text: &str, reasoning: bool) {
        if text.is_empty() {
            return;
        }
        let mut pending = self.0.borrow_mut();
        if reasoning {
            pending.reasoning.push_str(text);
        } else {
            pending.text.push_str(text);
        }
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            if pending.timer.is_some() {
                return;
            }
            // A timer also progresses in background tabs where rAF is suspended.
            pending.timer = web_sys::window().and_then(|window| {
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        pending.callback.as_ref()?.as_ref().unchecked_ref(),
                        32,
                    )
                    .ok()
            });
            if pending.timer.is_none() {
                pending.apply();
                drop(pending);
                crate::ui::flush();
            }
        }
    }

    /// Drain synchronously on completion, error, abort or component cleanup.
    pub(super) fn finish(&self) {
        let mut pending = self.0.borrow_mut();
        #[cfg(target_arch = "wasm32")]
        pending.cancel_timer();
        pending.apply();
        drop(pending);
        crate::ui::flush();
    }
}

impl Pending {
    fn apply(&mut self) {
        if self.text.is_empty() && self.reasoning.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        let reasoning = std::mem::take(&mut self.reasoning);
        if !text.is_empty() {
            let _ = self.content.try_revise(|content| content.push_str(&text));
        }
        if !reasoning.is_empty() {
            let _ = self
                .thinking
                .try_revise(|thinking| thinking.get_or_insert_default().push_str(&reasoning));
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn cancel_timer(&mut self) {
        if let Some(timer) = self.timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        self.cancel_timer();
        self.apply();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn empty_message() -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            thinking: None,
            attachments: Vec::new(),
            timestamp: None,
            terminal_events: Vec::new(),
        }
    }

    #[test]
    fn batches_fragments_and_drains_final_text_once() {
        let runtime = glory::Runtime::new();
        let owner = runtime.owner();
        let message = owner.cage(empty_message());
        let updates = StreamUpdates::new(message);
        for _ in 0..1000 {
            updates.push("你", false);
            updates.push("想", true);
        }
        assert!(message.get().content.is_empty());
        updates.finish();
        assert_eq!(message.get().content, "你".repeat(1000));
        assert_eq!(message.get().thinking, Some("想".repeat(1000)));
        updates.finish();
        assert_eq!(message.get().content, "你".repeat(1000));
        updates.push("结束", false);
        drop(updates);
        assert!(message.get().content.ends_with("结束"));
    }

    #[test]
    fn streaming_does_not_invalidate_history_or_attachment_observers() {
        let runtime = glory::Runtime::new();
        let owner = runtime.owner();
        let messages = owner.cage(vec![empty_message(); 2000]);
        let history = messages.item(0);
        let active = messages.item(1999);
        let attachments = active.field("attachments", |m| &m.attachments, |m| &mut m.attachments);
        let runs = Rc::new(Cell::new(0));
        let observed_runs = runs.clone();
        owner.effect(move || {
            history.with(|_| ());
            attachments.with(|_| ());
            observed_runs.set(observed_runs.get() + 1);
        });
        runtime.flush().unwrap();
        assert_eq!(runs.get(), 1);
        let updates = StreamUpdates::new(active);
        for _ in 0..1000 {
            updates.push("fragment", false);
            updates.push("reasoning", true);
        }
        updates.finish();
        runtime.flush().unwrap();
        assert_eq!(runs.get(), 1);
        assert_eq!(active.get().content, "fragment".repeat(1000));
    }
}
