//! Credential-scoped approval and questionnaire notifications remain visible
//! while a chat request is in flight, so resolving one never retires that chat.
use glory::prelude::*;
use serde_json::{Value, json};

use crate::pages::common;
use crate::ui::{self, AppContext};

fn identity(value: &Value) -> String {
    common::string(value, &["request_id", "id"])
}

pub fn center(ctx: &AppContext) -> Box<dyn Widget> {
    let pending = ctx.scope.cage(Vec::<Value>::new());
    let load_context = ctx.clone();
    ctx.scope.effect(move || {
        if !load_context.connected.get() {
            return;
        }
        let task = load_context.clone();
        load_context.spawn(async move {
            if let Ok(value) = task.rpc.call::<Value>("exec.approvals.get", None).await {
                let _ = pending.try_revise(|items| {
                    for value in common::array(&value, &["approvals", "pending"]) {
                        let id = identity(&value);
                        if !id.is_empty() && !items.iter().any(|item| identity(item) == id) {
                            items.push(value);
                        }
                    }
                });
                ui::flush();
            }
        });
    });
    for event in ["exec.approval.requested", "claude.question"] {
        ctx.on_notification(event, move |value| {
            let id = identity(&value);
            if id.is_empty() {
                return;
            }
            crate::utils::notifications::notify_if_enabled(
                "Savfox needs your attention",
                "An execution approval or answer is waiting.",
            );
            let _ = pending.try_revise(|items| {
                items.retain(|item| identity(item) != id);
                items.push(value);
            });
            ui::flush();
        });
    }
    ctx.on_notification("exec.approval.resolved", move |value| {
        let id = identity(&value);
        let _ = pending.try_revise(|items| items.retain(|item| identity(item) != id));
        ui::flush();
    });
    let list_ctx = ctx.clone();
    Box::new(
        col().class("approval-center").fill(
            each(pending)
                .key(|value| format!("{}:{}", identity(value), value["nonce"]))
                .children(move |each_row| {
                    let request = each_row.value;
                    let ctx = list_ctx.clone();
                    widget(move |scope| {
                        card(&ctx.with_scope(scope), request.get_untracked(), pending).build(scope)
                    })
                }),
        ),
    )
}

fn card(ctx: &AppContext, request: Value, pending: Cage<Vec<Value>>) -> Box<dyn Widget> {
    let id = identity(&request);
    let nonce = common::string(&request, &["nonce"]);
    let busy = ctx.scope.cage(false);
    let error = ctx.scope.cage(String::new());
    let answers = ctx.scope.cage(serde_json::Map::<String, Value>::new());
    let mut children = vec![
        common::paragraph(common::string(&request, &["command", "summary", "tool"])),
        common::metadata(
            &request,
            &[
                ("agent_id", "Agent"),
                ("session_id", "Session"),
                ("cwd", "Directory"),
            ],
        ),
    ];
    let questions = request
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_questions = !questions.is_empty();
    for (index, question) in questions.into_iter().enumerate() {
        let value = ctx.scope.cage(String::new());
        let key = index.to_string();
        let label = common::string(&question, &["question", "header"]);
        let update_key = key.clone();
        children.push(Box::new(text().class("field-label").content(label.clone())));
        children.push(Box::new(
            sapid::text_field()
                .name(label.clone())
                .value(value)
                .on_value_change(move |next| {
                    let _ = answers.try_revise(|answers| {
                        answers.insert(update_key.clone(), json!(next));
                    });
                    ui::set(value, next);
                }),
        ));
        let mut options = Vec::<Box<dyn Widget>>::new();
        for option in common::array(&question, &["options"]) {
            let label = common::string(&option, &["label"]);
            let answer = label.clone();
            let key = key.clone();
            options.push(ui::action(label, move || {
                let _ = answers.try_revise(|answers| {
                    answers.insert(key.clone(), json!(answer));
                });
                ui::set(value, answer.clone());
            }));
        }
        children.push(Box::new(row().class("question-options").fill(options)));
    }
    let mut actions = Vec::<Box<dyn Widget>>::new();
    for (decision, label) in [
        (
            "approve-once",
            if has_questions {
                "Send answers"
            } else {
                "Approve once"
            },
        ),
        ("deny", "Deny"),
        ("abort", "Stop task"),
    ] {
        let ctx = ctx.clone();
        let id = id.clone();
        let nonce = nonce.clone();
        let enabled = !nonce.is_empty();
        actions.push(Box::new(
            sapid::button()
                .label(label)
                .enabled(busy.map(move |busy| enabled && !*busy))
                .on_press(move || {
                    if busy.get() {
                        return;
                    }
                    ui::set(busy, true);
                    ui::set(error, String::new());
                    let task = ctx.clone();
                    let id = id.clone();
                    let nonce = nonce.clone();
                    let mut params = json!({"request_id":id,"nonce":nonce,"decision":decision});
                    if has_questions {
                        params["answers"] = Value::Object(answers.get());
                    }
                    ctx.spawn(async move {
                        match task
                            .rpc
                            .call::<Value>("exec.approval.resolve", Some(params))
                            .await
                        {
                            Ok(_) => {
                                let _ = pending
                                    .try_revise(|items| items.retain(|item| identity(item) != id));
                                ui::flush();
                            }
                            Err(message) => {
                                ui::set(error, message);
                                ui::set(busy, false);
                            }
                        }
                    });
                }),
        ));
    }
    children.push(ui::error_text(ctx, error));
    children.push(Box::new(row().class("approval-actions").fill(actions)));
    common::card(
        if has_questions {
            "Answer required"
        } else {
            "Execution approval required"
        },
        children,
    )
}
