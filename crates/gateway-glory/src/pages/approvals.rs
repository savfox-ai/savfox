use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let state = query(ctx, "exec.approvals.get", None);
    for event in ["exec.approval.requested", "exec.approval.resolved"] {
        let state = state.clone();
        ctx.on_notification(event, move |_| state.refresh());
    }
    let item_state = state.clone();
    ui::localized_page(
        ctx,
        "nav.approvals",
        "Approve once, deny, or stop execution. A recorded decision does not prove the tool has executed.",
        vec![
            status(ctx, &state),
            refresh(ctx, &state),
            records(
                ctx,
                &state,
                &["approvals", "pending"],
                move |request, context| {
                    let mut fields = vec![metadata(
                        &request,
                        &[
                            ("command", "Command"),
                            ("cwd", "Directory"),
                            ("reason", "Reason"),
                            ("agent_id", "Agent"),
                            ("session_id", "Session"),
                        ],
                    )];
                    let answers = context.scope.cage(serde_json::Map::<String, Value>::new());
                    let questions = request
                        .pointer("/request/questions")
                        .or_else(|| request.get("questions"))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let has_questions = !questions.is_empty();
                    for (index, question) in questions.into_iter().enumerate() {
                        let target = index.to_string();
                        let value = context.scope.cage(String::new());
                        fields.push(Box::new(
                            sapid::text_field()
                                .name(string(&question, &["question", "header"]))
                                .value(value)
                                .on_value_change(move |next| {
                                    ui::set(value, next.clone());
                                    let mut current = answers.get();
                                    current.insert(target.clone(), json!(next));
                                    ui::set(answers, current);
                                }),
                        ) as Box<dyn Widget>);
                        let options = array(&question, &["options"]);
                        if !options.is_empty() {
                            fields.push(paragraph(
                                options
                                    .iter()
                                    .map(|o| {
                                        format!(
                                            "{} — {}",
                                            string(o, &["label"]),
                                            string(o, &["description"])
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ));
                        }
                    }
                    let mut actions = vec![];
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
                        let context = context.clone();
                        let state = item_state.clone();
                        let id = string(&request, &["id", "request_id"]);
                        let nonce = string(&request, &["nonce"]);
                        let enabled = !nonce.is_empty();
                        actions.push(Box::new(
                            sapid::button()
                                .label(label)
                                .enabled(state.busy.map(move |busy| enabled && !*busy))
                                .on_press(move || {
                                    let mut params =
                                        json!({"request_id":id,"nonce":nonce,"decision":decision});
                                    if has_questions {
                                        params["answers"] = Value::Object(answers.get());
                                    }
                                    mutate(&context, &state, "exec.approval.resolve", params);
                                }),
                        ) as Box<dyn Widget>);
                    }
                    fields.push(Box::new(row().class("actions").fill(actions)));
                    card(string(&request, &["title", "id", "request_id"]), fields)
                },
            ),
        ],
    )
}
