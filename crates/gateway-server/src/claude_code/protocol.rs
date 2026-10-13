use std::collections::{HashSet, VecDeque};

use anyhow::{Context, ensure};
use savfox_protocol::protocol::TokenUsage;
use serde_json::{Value, json};

use crate::agent_terminal_delegate::TerminalEventSink;
use crate::terminal_agent::TerminalAgentEvent;

pub(super) const FRAME_LIMIT: usize = 1024 * 1024;
pub(super) const REPLY_LIMIT: usize = 1024 * 1024;

pub(super) fn control_response(id: &str, response: Value) -> Value {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": response}})
}

pub(super) fn deny(message: &str) -> Value {
    json!({"behavior": "deny", "message": message})
}

pub(super) fn unsupported(id: &str) -> Value {
    json!({"type": "control_response", "response": {"subtype": "error", "request_id": id, "error": "Savfox does not support this control request"}})
}

pub(super) fn questions(input: &Value) -> anyhow::Result<Value> {
    let questions = input["questions"]
        .as_array()
        .context("AskUserQuestion missing questions")?;
    ensure!(
        !questions.is_empty() && questions.len() <= 4,
        "invalid question count"
    );
    let mut seen = HashSet::new();
    for question in questions {
        ensure!(
            question["question"]
                .as_str()
                .is_some_and(|q| !q.is_empty() && q.len() <= 4096),
            "invalid question text"
        );
        ensure!(
            seen.insert(question["question"].as_str()),
            "duplicate question text"
        );
        let options = question["options"]
            .as_array()
            .context("question missing options")?;
        ensure!(
            (2..=4).contains(&options.len())
                && options.iter().all(|option| option["label"]
                    .as_str()
                    .is_some_and(|label| !label.is_empty() && label.len() <= 1024)),
            "invalid question options"
        );
    }
    // Rendered questions are useful, but can also contain tool-produced secrets.
    let text = crate::redaction::redact_text_always(&serde_json::to_string(&input["questions"])?);
    Ok(serde_json::from_str(&text)?)
}

pub(super) fn approved_input(input: &Value, answers: &Value) -> anyhow::Result<Value> {
    questions(input)?;
    let answers = answers.as_object().context("answers must be an object")?;
    let questions = input["questions"]
        .as_array()
        .context("questions must be an array")?;
    ensure!(
        answers.len() == questions.len(),
        "answer every question exactly once"
    );
    let mut mapped = serde_json::Map::new();
    for (index, question) in questions.iter().enumerate() {
        let key = question["question"].as_str().context("invalid question")?;
        let answer = answers
            .get(&index.to_string())
            .and_then(Value::as_str)
            .context("answer keys must be question indices (0, 1, ...)")?;
        ensure!(
            !answer.is_empty() && answer.len() <= 4096,
            "missing or invalid question answer"
        );
        mapped.insert(key.to_owned(), Value::String(answer.to_owned()));
    }
    let mut updated = input.clone();
    updated["answers"] = Value::Object(mapped);
    Ok(updated)
}

#[derive(Default)]
pub(super) struct State {
    pub(super) initialized: bool,
    pub(super) result: Option<Value>,
    pub(super) session_id: Option<String>,
    pub(super) reply: String,
    pub(super) usage: Option<TokenUsage>,
    pub(super) vendor_usage: Option<Value>,
    pub(super) running: bool,
    pub(super) tasks: HashSet<String>,
    partial: String,
    completed: VecDeque<String>,
    unknown_count: usize,
    actual_model: Option<String>,
}

impl State {
    pub(super) fn ready(&self) -> bool {
        self.initialized && self.result.is_some() && !self.running && self.tasks.is_empty()
    }

    pub(super) fn reply_snapshot(&self) -> String {
        if self.partial.is_empty() {
            self.reply.clone()
        } else {
            self.partial.clone()
        }
    }

    pub(super) fn event(
        &mut self,
        message: &Value,
        sink: Option<&TerminalEventSink>,
        expected_session: &str,
    ) -> anyhow::Result<()> {
        ensure!(
            message.is_object() && message["type"].is_string(),
            "invalid Claude event envelope"
        );
        if let Some(id) = message["session_id"].as_str() {
            ensure!(
                id == expected_session,
                "Claude returned a different session identity"
            );
            self.session_id = Some(id.to_owned());
        }
        // Subagents have their own output. Only the root conversation is a reply.
        if !message["parent_tool_use_id"].is_null() {
            return Ok(());
        }
        match message["type"].as_str().unwrap_or_default() {
            "stream_event" => {
                let event = &message["event"];
                match event["type"].as_str() {
                    Some("message_start") => {
                        self.partial.clear();
                        self.result = None;
                    }
                    Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                        let text = event["delta"]["text"]
                            .as_str()
                            .context("Claude delta missing text")?;
                        ensure!(
                            self.partial.len().saturating_add(text.len()) <= REPLY_LIMIT,
                            "Claude reply exceeds output limit"
                        );
                        self.partial.push_str(text);
                        if let Some(sink) = sink {
                            sink(TerminalAgentEvent::OutputDelta {
                                stream: "stdout".to_owned(),
                                text: text.to_owned(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let content = message["message"]["content"]
                    .as_array()
                    .context("Claude assistant missing content")?;
                let text = content
                    .iter()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .collect::<String>();
                ensure!(
                    text.len() <= REPLY_LIMIT,
                    "Claude assistant exceeds output limit"
                );
                let id = message["message"]["id"]
                    .as_str()
                    .context("Claude assistant missing message id")?;
                if !self.completed.iter().any(|completed| completed == id) {
                    self.result = None;
                    if self.partial.is_empty()
                        && !text.is_empty()
                        && let Some(sink) = sink
                    {
                        sink(TerminalAgentEvent::OutputDelta {
                            stream: "stdout".to_owned(),
                            text: text.clone(),
                        });
                    }
                    if !text.is_empty() {
                        self.reply = text;
                    }
                    self.completed.push_back(id.to_owned());
                    if self.completed.len() > 128 {
                        self.completed.pop_front();
                    }
                    self.partial.clear();
                }
            }
            "result" => {
                ensure!(self.result.is_none(), "duplicate Claude result");
                if let Some(text) = message["result"].as_str() {
                    ensure!(
                        text.len() <= REPLY_LIMIT,
                        "Claude result exceeds output limit"
                    );
                    self.reply = text.to_owned();
                }
                self.partial.clear();
                self.vendor_usage = Some(
                    json!({"usage":message.get("usage"),"model_usage":message.get("modelUsage"),"total_cost_usd":message.get("total_cost_usd"),"model":self.actual_model}),
                );
                self.usage = usage(message.get("usage"));
                self.result = Some(message.clone());
            }
            "system" => match message["subtype"].as_str() {
                Some("init") => {
                    self.actual_model = message["model"]
                        .as_str()
                        .filter(|model| model.len() <= 256)
                        .map(ToOwned::to_owned);
                }
                Some("session_state_changed") => {
                    self.running = message["state"] != "idle";
                }
                Some("task_started")
                    if matches!(
                        message["task_type"].as_str(),
                        Some("local_agent" | "local_workflow")
                    ) =>
                {
                    ensure!(self.tasks.len() < 128, "too many Claude background tasks");
                    self.tasks.insert(
                        message["task_id"]
                            .as_str()
                            .context("task missing identity")?
                            .to_owned(),
                    );
                }
                Some("task_notification") => {
                    if let Some(id) = message["task_id"].as_str() {
                        self.tasks.remove(id);
                    }
                }
                Some("task_updated")
                    if matches!(
                        message["patch"]["status"].as_str(),
                        Some("completed" | "failed" | "killed" | "cancelled")
                    ) =>
                {
                    if let Some(id) = message["task_id"].as_str() {
                        self.tasks.remove(id);
                    }
                }
                _ => {}
            },
            "user"
            | "control_response"
            | "control_cancel_request"
            | "keep_alive"
            | "rate_limit_event" => {}
            _ => {
                if self.unknown_count < 8 {
                    tracing::debug!(
                        event_type = message["type"].as_str(),
                        "ignoring unknown Claude display event"
                    );
                }
                self.unknown_count += 1;
            }
        }
        Ok(())
    }
}

fn usage(value: Option<&Value>) -> Option<TokenUsage> {
    let usage = value?;
    let input = usage["input_tokens"].as_i64()?;
    let output = usage["output_tokens"].as_i64()?;
    let read = usage["cache_read_input_tokens"].as_i64()?;
    let creation = usage["cache_creation_input_tokens"].as_i64()?;
    if [input, output, read, creation]
        .iter()
        .any(|value| *value < 0)
    {
        return None;
    }
    // TokenUsage.input includes all input categories; cached_input is the
    // cache-read subset. Missing vendor categories remain unknown (None).
    let total_input = input.checked_add(read)?.checked_add(creation)?;
    Some(TokenUsage {
        input_tokens: total_input,
        cached_input_tokens: read,
        output_tokens: output,
        reasoning_output_tokens: 0,
        total_tokens: total_input.checked_add(output)?,
    })
}
