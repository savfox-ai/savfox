//! OpenAI account authentication through the gateway's existing account RPCs.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glory::prelude::*;
use serde_json::{Value, json};

use super::super::common::{card, paragraph, secret, string};
use crate::api::ws::WsRpc;
use crate::ui::{self, AppContext};

#[derive(Clone, Copy)]
struct Login {
    id: Cage<String>,
    url: Cage<String>,
    code: Cage<String>,
    status: Cage<String>,
    error: Cage<String>,
    busy: Cage<bool>,
    generation: Cage<u64>,
}
#[derive(Default)]
struct Worker {
    id: RefCell<Option<String>>,
    retired: Cell<bool>,
}
impl Worker {
    fn retire(&self) -> Option<String> {
        self.retired.set(true);
        self.id.borrow_mut().take()
    }
}
fn detached(future: impl std::future::Future<Output = ()> + 'static) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(future);
    #[cfg(not(target_arch = "wasm32"))]
    drop(future);
}
fn cancel_worker(rpc: WsRpc, id: String) {
    detached(async move {
        let _ = rpc
            .call::<Value>("account/login/cancel", Some(json!({"loginId":id})))
            .await;
    });
}
pub fn render(ctx: &AppContext, key: Cage<String>) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = Login {
        id: ctx.scope.cage(String::new()),
        url: ctx.scope.cage(String::new()),
        code: ctx.scope.cage(String::new()),
        status: ctx.scope.cage(String::new()),
        error: ctx.scope.cage(String::new()),
        busy: ctx.scope.cage(false),
        generation: ctx.scope.cage(0),
    };
    let worker = Rc::new(Worker::default());
    let mut children = vec![
        paragraph(ui::label_for(
            locale,
            "Sign in with ChatGPT, a device code, or an OpenAI API key.",
        )),
        ui::error_text(ctx, state.error),
        Box::new(text().content(state.status)),
    ];
    for (label, kind) in [
        ("Sign in with ChatGPT", "chatgpt"),
        ("Sign in with device code", "deviceCode"),
    ] {
        let context = ctx.clone();
        let lease = worker.clone();
        children.push(Box::new(
            sapid::button()
                .label(label)
                .enabled(
                    ctx.scope
                        .bond(move || !state.busy.get() && state.id.get().is_empty()),
                )
                .on_press(move || start(&context, state, kind, lease.clone())),
        ));
    }
    children.push(Box::new(text().class("pairing-code").content(state.code)));
    let url = state.url;
    children.push(Box::new(when(
        url.map(|value| !value.is_empty()),
        move || {
            sapid::anchor()
                .href(url)
                .label(ui::label_for(locale, "Open sign-in page"))
                .open_in(LinkOpen::NewContext)
        },
    )));
    let context = ctx.clone();
    let lease = worker.clone();
    children.push(ui::action(
        ui::label_for(locale, "Check sign-in status"),
        move || {
            let context_async = context.clone();
            let lease = lease.clone();
            context.spawn(async move {
                check(&context_async, state, &lease).await;
            });
        },
    ));
    let context = ctx.clone();
    let lease = worker.clone();
    children.push(ui::action(
        ui::label_for(locale, "Cancel sign-in"),
        move || cancel(&context, state, &lease),
    ));
    children.push(secret(ui::label_for(locale, "OpenAI account API key"), key));
    let context = ctx.clone();
    children.push(Box::new(
        sapid::button()
            .label(ui::label_for(locale, "Save OpenAI API key"))
            .loading(state.busy)
            .on_press(move || {
                if key.get().trim().is_empty() {
                    ui::set(state.error, "Enter an OpenAI API key".to_owned());
                    return;
                }
                if !state.id.get().is_empty() {
                    ui::set(
                        state.error,
                        "Cancel the current sign-in before saving an API key".to_owned(),
                    );
                    return;
                }
                let context_async = context.clone();
                let api_key = key.get();
                ui::set(state.busy, true);
                ui::set(state.error, String::new());
                context.spawn(async move {
                    match context_async
                        .rpc
                        .call::<Value>(
                            "account/login/start",
                            Some(json!({"type":"apiKey","apiKey":api_key})),
                        )
                        .await
                    {
                        Ok(value) if value["type"].as_str() == Some("apiKey") => {
                            ui::set(state.status, "OpenAI API key saved".to_owned())
                        }
                        Ok(_) => ui::set(
                            state.error,
                            "Gateway did not confirm the API key login".to_owned(),
                        ),
                        Err(message) => ui::set(state.error, message),
                    }
                    ui::set(state.busy, false);
                });
            }),
    ));
    // Cleanup runs after reactive identities retire. The worker lease remains readable.
    let rpc = ctx.rpc.clone();
    let _ = ctx.scope.on_cleanup(move || {
        if let Some(id) = worker.retire() {
            cancel_worker(rpc, id);
        }
    });
    card(ui::label_for(locale, "OpenAI account"), children)
}
fn start(ctx: &AppContext, state: Login, kind: &'static str, worker: Rc<Worker>) {
    if state.busy.get() || !state.id.get().is_empty() {
        return;
    }
    let generation = state.generation.get().wrapping_add(1);
    ui::set(state.generation, generation);
    ui::set(state.busy, true);
    ui::set(state.error, String::new());
    ui::set(state.status, String::new());
    let context = ctx.clone();
    // Keep the start reply alive across route retirement so a late-created server
    // worker can still be cancelled after its ID becomes known.
    detached(async move {
        let response = context
            .rpc
            .call::<Value>(
                "account/login/start",
                Some(json!({"type":kind,"openBrowser":false})),
            )
            .await;
        if worker.retired.get()
            || !state.generation.is_alive()
            || state.generation.get() != generation
        {
            if let Ok(value) = response {
                let id = string(&value, &["loginId"]);
                if !id.is_empty() {
                    cancel_worker(context.rpc.clone(), id);
                }
            }
            return;
        }
        match response {
            Ok(value) => {
                let id = string(&value, &["loginId"]);
                let url = string(&value, &["authUrl", "verificationUrl"]);
                if id.is_empty() || url.is_empty() {
                    if !id.is_empty() {
                        cancel_worker(context.rpc.clone(), id);
                    }
                    ui::set(
                        state.error,
                        "Gateway did not return a sign-in URL and login ID".to_owned(),
                    );
                    ui::set(state.busy, false);
                    return;
                }
                *worker.id.borrow_mut() = Some(id.clone());
                ui::set(state.id, id);
                ui::set(state.url, url);
                ui::set(state.code, string(&value, &["userCode"]));
                ui::set(state.status, "Waiting for sign-in…".to_owned());
            }
            Err(message) => {
                ui::set(state.error, message);
                ui::set(state.busy, false);
                return;
            }
        }
        ui::set(state.busy, false);
        for _ in 0..450 {
            crate::utils::sleep_ms(2000).await;
            if worker.retired.get()
                || !state.generation.is_alive()
                || state.generation.get() != generation
                || state.id.get().is_empty()
            {
                return;
            }
            if check(&context, state, &worker).await {
                return;
            }
        }
        if !worker.retired.get()
            && state.generation.is_alive()
            && state.generation.get() == generation
        {
            ui::set(
                state.error,
                "Sign-in timed out. Start again to request a new code.".to_owned(),
            );
            cancel(&context, state, &worker);
        }
    });
}
async fn check(ctx: &AppContext, state: Login, worker: &Worker) -> bool {
    let generation = state.generation.get();
    let response = ctx
        .rpc
        .call::<Value>("account/read", Some(json!({"refreshToken":true})))
        .await;
    if worker.retired.get() || !state.generation.is_alive() || state.generation.get() != generation
    {
        return false;
    }
    match response {
        Ok(value) if value["account"]["type"].as_str() == Some("chatgpt") => {
            let account = &value["account"];
            let email = string(account, &["email"]);
            let plan = string(account, &["planType"]);
            ui::set(
                state.status,
                format!(
                    "Signed in successfully{email}{plan}",
                    email = if email.is_empty() {
                        String::new()
                    } else {
                        format!(" · {email}")
                    },
                    plan = if plan.is_empty() {
                        String::new()
                    } else {
                        format!(" · {plan}")
                    }
                ),
            );
            worker.id.borrow_mut().take();
            ui::set(state.error, String::new());
            ui::set(state.id, String::new());
            ui::set(state.url, String::new());
            ui::set(state.code, String::new());
            true
        }
        Ok(_) => false,
        Err(message) => {
            ui::set(state.error, message);
            false
        }
    }
}
fn cancel(ctx: &AppContext, state: Login, worker: &Worker) {
    let id = worker.id.borrow_mut().take();
    ui::set(state.generation, state.generation.get().wrapping_add(1));
    ui::set(state.id, String::new());
    ui::set(state.url, String::new());
    ui::set(state.code, String::new());
    ui::set(state.busy, false);
    if let Some(id) = id {
        let context = ctx.clone();
        let generation = state.generation.get();
        detached(async move {
            let result = context
                .rpc
                .call::<Value>("account/login/cancel", Some(json!({"loginId":id})))
                .await;
            if !state.generation.is_alive() || state.generation.get() != generation {
                return;
            }
            match result {
                Ok(_) => ui::set(state.status, "Sign-in cancelled".to_owned()),
                Err(message) => ui::set(state.error, message),
            }
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_id_survives_owner_disposal_and_is_retired_once() {
        let app = App::memory();
        let worker = Rc::new(Worker::default());
        let observed = Rc::new(RefCell::new(Vec::new()));
        let worker_mount = worker.clone();
        let observed_mount = observed.clone();
        let mounted = app
            .scope()
            .mount_with(|| {
                widget(move |scope| {
                    let identity = scope.cage("server-login-id".to_owned());
                    *worker_mount.id.borrow_mut() = Some(identity.get());
                    let worker = worker_mount.clone();
                    let observed = observed_mount.clone();
                    scope.on_cleanup(move || {
                        assert!(!identity.is_alive());
                        observed.borrow_mut().push(worker.retire());
                    })?;
                    Ok(())
                })
            })
            .expect("mount lifecycle probe");
        drop(mounted);
        assert_eq!(*observed.borrow(), vec![Some("server-login-id".to_owned())]);
        assert!(worker.retired.get());
        assert_eq!(worker.retire(), None);
    }
}
