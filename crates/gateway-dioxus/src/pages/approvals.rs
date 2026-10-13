use dioxus::prelude::*;
use lucide_dioxus::ShieldCheck;
use savfox_gateway_shared::ExecApprovalFull;
use serde_json::{Value, json};

use crate::api::ws::WsRpc;
use crate::components::empty_state::EmptyState;

#[component]
pub fn Approvals() -> Element {
    let ws = use_context::<WsRpc>();
    let connected = use_context::<Signal<bool>>();
    let mut refresh = use_signal(|| 0_u64);
    use_future(move || async move {
        loop {
            crate::utils::sleep_ms(2000).await;
            refresh += 1;
        }
    });
    let data = use_resource(move || {
        let _ = connected();
        let _ = refresh();
        let ws = ws.clone();
        async move {
            let result = ws
                .call::<Value>("exec.approvals.get", None)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::from_value::<Vec<ExecApprovalFull>>(result["approvals"].clone())
                .map_err(|error| error.to_string())
        }
    });
    let read = data.read();
    let (pending, error) = match read.as_ref() {
        Some(Ok(pending)) => (pending.clone(), None),
        Some(Err(error)) => (Vec::new(), Some(error.clone())),
        None => (Vec::new(), None),
    };
    rsx! {
        div { class: "page-content",
            h2 { style: "font-size:20px;font-weight:600;margin-bottom:16px;", "Approvals" }
            p { "Approve once, deny, or stop the requested execution. Recording a decision does not prove that the tool has executed." }
            if let Some(error) = error { p { style: "color:var(--danger);", "{error}" } }
            if pending.is_empty() {
                EmptyState { icon: rsx! { ShieldCheck { size: 20 } }, message: "No pending approvals".to_owned() }
            }
            for request in pending {
                ApprovalCard { key: "{request.id}", request, refresh }
            }
        }
    }
}

#[component]
fn ApprovalCard(request: ExecApprovalFull, mut refresh: Signal<u64>) -> Element {
    let ws = use_context::<WsRpc>();
    let mut answers = use_signal(serde_json::Map::new);
    let mut error = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let questions = request
        .questions
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let command = request.command.as_deref().unwrap_or("Permission request");
    let target = request.kind.as_deref().unwrap_or("exec");
    rsx! {
        div { class: "ui-card", style: "margin-bottom:12px;",
            p { "{target}" }
            pre { style: "white-space:pre-wrap;overflow-wrap:anywhere;", "{command}" }
            if let Some(cwd) = &request.cwd { p { "Directory: {cwd}" } }
            for (index, question) in questions.iter().enumerate() {
                {
                    let key = index.to_string();
                    let text = question["question"].as_str().unwrap_or("Question").to_owned();
                    let multiple = question["multiSelect"].as_bool().unwrap_or(false);
                    let options = question["options"].as_array().map(|options| options.iter().map(|option| format!("{}: {}", option["label"].as_str().unwrap_or(""), option["description"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ")).unwrap_or_default();
                    rsx! {
                        div { style: "margin:12px 0;",
                            label { "{text}" }
                            p { style: "font-size:12px;color:var(--text-muted);", "{options}" }
                            input {
                                class: "ui-input", value: answers().get(&key).and_then(Value::as_str).unwrap_or("").to_owned(),
                                placeholder: if multiple { "Selected labels separated by commas, or your answer" } else { "Selected label or your answer" },
                                oninput: move |event| { answers.write().insert(key.clone(), Value::String(event.value())); },
                            }
                        }
                    }
                }
            }
            if !error().is_empty() { p { style: "color:var(--danger);", "{error}" } }
            div { style: "display:flex;gap:8px;",
                for (decision, label) in [("approve-once", if questions.is_empty() { "Approve once" } else { "Send answers" }), ("deny", "Deny"), ("abort", "Stop task")] {
                    {
                        let ws = ws.clone();
                        let id = request.id.clone();
                        let nonce = request.nonce.clone().unwrap_or_default();
                        let decision = decision.to_owned();
                        let has_questions = !questions.is_empty();
                        rsx! {
                            button {
                                class: "tool-btn", disabled: sending() || nonce.is_empty(),
                                onclick: move |_| {
                                    let ws = ws.clone(); let id = id.clone(); let nonce = nonce.clone(); let decision = decision.clone();
                                    let mut params = json!({"request_id":id,"nonce":nonce,"decision":decision});
                                    if has_questions { params["answers"] = Value::Object(answers()); }
                                    sending.set(true); error.set(String::new());
                                    spawn(async move {
                                        match ws.call::<Value>("exec.approval.resolve", Some(params)).await {
                                            Ok(_) => { refresh += 1; }
                                            Err(err) => error.set(err.to_string()),
                                        }
                                        sending.set(false);
                                    });
                                }, "{label}"
                            }
                        }
                    }
                }
            }
        }
    }
}
