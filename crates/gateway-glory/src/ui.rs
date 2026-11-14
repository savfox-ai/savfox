//! Shared semantic controls and application services for the Glory frontend.
use std::future::Future;

use glory::prelude::*;
use serde_json::Value;

use crate::api::ws::WsRpc;
use crate::i18n::{self, Locale};
mod page_strings;

pub fn label(ctx: &AppContext, source: impl Into<String>) -> Vary<String> {
    label_for(ctx.locale, source)
}
pub fn label_for(locale: Cage<Locale>, source: impl Into<String>) -> Vary<String> {
    let source = source.into();
    locale
        .map(move |locale| page_strings::translate(*locale, &source))
        .into()
}
pub fn translate(locale: Locale, source: &str) -> String {
    page_strings::translate(locale, source)
}

#[derive(Clone)]
pub struct AppContext {
    pub scope: Scope,
    pub rpc: WsRpc,
    pub connected: Cage<bool>,
    pub locale: Cage<Locale>,
    pub(crate) path: Cage<String>,
    pub(crate) authenticated: Cage<bool>,
    pub(crate) modals: Cage<Option<sapid::ModalsHandle>>,
}

impl AppContext {
    pub fn new(scope: &Scope) -> Self {
        Self {
            scope: scope.clone(),
            rpc: WsRpc::new(),
            connected: scope.cage(false),
            locale: scope.cage(i18n::detect_locale()),
            path: scope.cage(crate::route::current_path()),
            authenticated: scope.cage(crate::api::ws::get_token().is_some()),
            modals: scope.cage(None),
        }
    }

    pub fn with_scope(&self, scope: &Scope) -> Self {
        Self {
            scope: scope.clone(),
            ..self.clone()
        }
    }

    pub fn navigate(&self, path: &str) {
        #[cfg(target_arch = "wasm32")]
        if let Some(window) = web_sys::window()
            && let Ok(history) = window.history()
        {
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path));
        }
        set(self.path, path.to_owned());
    }

    pub fn tr(&self, key: &str) -> String {
        i18n::t(self.locale.get(), key)
    }

    /// Sapid owns dismissal, safe initial focus and focus return. The request
    /// retires with its page, so a stale confirmation cannot mutate another page.
    pub fn confirm_action(&self, message: impl Into<String>, action: impl FnOnce() + 'static) {
        let Some(modals) = self.modals.get_untracked() else {
            return;
        };
        let locale = self.locale.get_untracked();
        let (title, confirm, cancel) = if locale == Locale::ZhCn {
            ("确认操作", "确认", "取消")
        } else {
            ("Confirm action", "Confirm", "Cancel")
        };
        let owner = self.scope.downgrade();
        let mut action = Some(action);
        let request = sapid::confirmation_request(title, translate(locale, &message.into()))
            .confirm_label(confirm)
            .close_label(cancel)
            .on_result(move |outcome| {
                if outcome == sapid::ModalOutcome::Confirmed
                    && owner.upgrade().is_some()
                    && let Some(action) = action.take()
                {
                    action();
                }
            });
        match modals.open(request) {
            Ok(token) => {
                let _ = self.scope.on_cleanup(move || {
                    let _ = modals.close(&token);
                });
                flush();
            }
            Err(error) => self.scope.report_error(error),
        }
    }

    pub fn on_notification(&self, method: &str, handler: impl Fn(Value) + 'static) {
        let subscription = self.rpc.subscribe(method, handler);
        let rpc = self.rpc.clone();
        let method = method.to_owned();
        let _ = self
            .scope
            .on_cleanup(move || rpc.unsubscribe(&method, subscription));
    }

    /// Browser work is cancelled when its page owner retires. This also makes
    /// late transport completions harmless after navigation.
    pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
        #[cfg(target_arch = "wasm32")]
        {
            use futures::future::{AbortHandle, Abortable};
            let (handle, registration) = AbortHandle::new_pair();
            let _ = self.scope.on_cleanup(move || handle.abort());
            wasm_bindgen_futures::spawn_local(async move {
                let _ = Abortable::new(future, registration).await;
                flush();
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        drop(future);
    }
}

/// Async callbacks run outside Glory's host event boundary.
pub fn flush() {
    #[cfg(target_arch = "wasm32")]
    crate::browser::flush();
}

pub fn set<T: 'static>(cage: Cage<T>, value: T) {
    let _ = cage.try_revise(|current| *current = value);
    flush();
}

pub fn modal_host(ctx: &AppContext) -> Box<dyn Widget> {
    let modals = ctx.modals;
    Box::new(
        sapid::modals_manager("gateway-modals").on_ready(move |handle| {
            let _ = modals.try_revise(|current| *current = Some(handle));
        }),
    )
}

pub fn page(
    title: impl Into<Vary<String>>,
    description: impl Into<Vary<String>>,
    children: Vec<Box<dyn Widget>>,
) -> Box<dyn Widget> {
    Box::new(
        col().class("gateway-page management-page").fill((
            text()
                .class("page-title")
                .kind(TextKind::Heading(1))
                .content(title.into()),
            text().class("page-description").content(description.into()),
            col().class("page-body page-content-body").fill(children),
        )),
    )
}

/// Locale changes update the heading binding while preserving the page Owner,
/// including unsent prompts, edited forms and in-flight requests.
pub fn localized_page(
    ctx: &AppContext,
    key: &str,
    description: impl Into<String>,
    children: Vec<Box<dyn Widget>>,
) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let key = key.to_owned();
    Box::new(
        col()
            .class(format!(
                "gateway-page management-page management-page--{}",
                key.trim_start_matches("nav.")
            ))
            .fill((
                text()
                    .class("page-title")
                    .kind(TextKind::Heading(1))
                    .content(ctx.scope.bond(move || i18n::t(locale.get(), &key))),
                text()
                    .class("page-description")
                    .content(label(ctx, description.into())),
                col().class("page-body page-content-body").fill(children),
            )),
    )
}

pub fn field(
    label: impl Into<Vary<String>>,
    value: Cage<String>,
    multiline: bool,
) -> Box<dyn Widget> {
    let label: Vary<String> = label.into();
    Box::new(
        col()
            .class(value.map(|value| {
                if value.is_empty() {
                    "form-field form-group form-field--empty".into()
                } else {
                    "form-field form-group".into()
                }
            }))
            .fill((
                text()
                    .class("field-label form-label")
                    .content(label.clone()),
                sapid::text_field()
                    .unstyled()
                    .class("form-input")
                    .name(label)
                    .value(value)
                    .multiline(multiline)
                    .on_value_change(move |next| set(value, next)),
            )),
    )
}

pub fn action(label: impl Into<Vary<String>>, callback: impl FnMut() + 'static) -> Box<dyn Widget> {
    action_button(label, "btn btn-secondary", callback)
}

pub fn primary_action(
    label: impl Into<Vary<String>>,
    callback: impl FnMut() + 'static,
) -> Box<dyn Widget> {
    action_button(label, "btn btn-primary", callback)
}

fn action_button(
    label: impl Into<Vary<String>>,
    class: &'static str,
    callback: impl FnMut() + 'static,
) -> Box<dyn Widget> {
    Box::new(
        sapid::button()
            .unstyled()
            .class(class)
            .label(label.into())
            .on_press(callback),
    )
}

pub fn error_text(ctx: &AppContext, error: Cage<String>) -> Box<dyn Widget> {
    Box::new(
        text()
            .class("error-message")
            .content(ctx.scope.bond(move || error.get())),
    )
}

pub fn json_text(ctx: &AppContext, value: Cage<Value>) -> Box<dyn Widget> {
    Box::new(text().class("json-output").content(ctx.scope.bond(move || {
        let value = value.get();
        if value.is_null() {
            String::new()
        } else {
            serde_json::to_string_pretty(&value).unwrap_or_default()
        }
    })))
}

/// A mounted panel preserves the controls' state across disclosure changes.
pub fn details(
    key: &str,
    label: impl Into<Vary<String>>,
    content: Box<dyn Widget>,
) -> Box<dyn Widget> {
    let mut content = Some(content);
    Box::new(
        sapid::collapsible(key.to_owned())
            .label(label.into())
            .content_policy(sapid::ContentPolicy::KeepMounted)
            .content(move || content.take().unwrap_or_else(|| Box::new(col()))),
    )
}

/// Keep Sapid's cursor styling and keyboard behavior while the CSS palette
/// supplies surfaces and text for both application color schemes.
pub fn select_control(key: impl Into<String>) -> sapid::Select {
    sapid::select(key)
        .check_position(sapid::SelectCheckPosition::Right)
        .theme(sapid::Theme {
            name: "savfox-select".into(),
            accent_soft: glory::style::Color(236, 118, 126, 46),
            ..sapid::Theme::default()
        })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    #[test]
    fn pending_confirmation_retires_with_its_page_without_running_the_action() {
        let app = App::new(glory::ssr::Renderer::new);
        let root = AppContext::new(app.scope());
        let visible = root.scope.cage(true);
        let mounted = Rc::new(RefCell::new(None));
        let output = mounted.clone();
        let page = root.clone();
        app.mount(view().fill((
            modal_host(&root),
            when(visible, move || {
                let page = page.clone();
                let output = output.clone();
                widget(move |scope| {
                    *output.borrow_mut() = Some(page.with_scope(scope));
                    view().build(scope)
                })
            }),
        )))
        .expect("mount page and Sapid modal host");
        let calls = Rc::new(Cell::new(0));
        let action_calls = calls.clone();
        mounted
            .borrow()
            .as_ref()
            .unwrap()
            .confirm_action("Delete this agent?", move || {
                action_calls.set(action_calls.get() + 1);
            });
        app.flush().unwrap();
        let modals = root.modals.get_untracked().expect("Sapid modal manager");
        assert_eq!(modals.pending().unwrap(), 1);
        assert_eq!(calls.get(), 0);

        set(visible, false);
        app.flush().unwrap();
        assert_eq!(modals.pending().unwrap(), 0);
        assert_eq!(calls.get(), 0);
    }
}
