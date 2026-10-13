use std::io::BufRead;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{StreamExt, TryStreamExt};
use savfox_http_client::{ByteStream, StreamResponse, TransportError};
use savfox_protocol::models::ResponseItem;
use savfox_protocol::protocol::TokenUsage;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};
use tokio_util::io::ReaderStream;
use tracing::{debug, trace};

use crate::common::{ResponseEvent, ResponseStream};
use crate::error::ApiError;
use crate::rate_limits::parse_rate_limit;
use crate::telemetry::SseTelemetry;

const X_REASONING_INCLUDED_HEADER: &str = "x-reasoning-included";

/// Streams SSE events from an on-disk fixture for tests.
pub fn stream_from_fixture(
    path: impl AsRef<Path>,
    idle_timeout: Duration,
) -> Result<ResponseStream, ApiError> {
    let file =
        std::fs::File::open(path.as_ref()).map_err(|err| ApiError::Stream(err.to_string()))?;
    let mut content = String::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|err| ApiError::Stream(err.to_string()))?;
        content.push_str(&line);
        content.push_str("\n\n");
    }

    let reader = std::io::Cursor::new(content);
    let stream = ReaderStream::new(reader).map_err(|err| TransportError::Network(err.to_string()));
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(process_sse(Box::pin(stream), tx_event, idle_timeout, None));
    Ok(ResponseStream { rx_event })
}

pub fn spawn_response_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    let rate_limits = parse_rate_limit(&stream_response.headers);
    let models_etag = stream_response
        .headers
        .get("X-Models-Etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let reasoning_included = stream_response
        .headers
        .get(X_REASONING_INCLUDED_HEADER)
        .is_some();
    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get("x-savfox-turn-state")
            .and_then(|v| v.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_owned());
    }
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        if let Some(snapshot) = rate_limits {
            let _ = tx_event.send(Ok(ResponseEvent::RateLimits(snapshot))).await;
        }
        if let Some(etag) = models_etag {
            let _ = tx_event.send(Ok(ResponseEvent::ModelsEtag(etag))).await;
        }
        if reasoning_included {
            let _ = tx_event
                .send(Ok(ResponseEvent::ServerReasoningIncluded(true)))
                .await;
        }
        process_sse(stream_response.bytes, tx_event, idle_timeout, telemetry).await;
    });

    ResponseStream { rx_event }
}

#[derive(Debug, Deserialize)]
struct Error {
    // Only `code` and `message` are consumed; other fields the API may send
    // (type, plan_type, resets_at) are ignored by serde's default behavior.
    code: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseCompleted {
    id: String,
    #[serde(default)]
    usage: Option<ResponseCompletedUsage>,
}

#[derive(Debug, Deserialize)]
struct ResponseDone {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<ResponseCompletedUsage>,
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedUsage {
    input_tokens: i64,
    input_tokens_details: Option<ResponseCompletedInputTokensDetails>,
    output_tokens: i64,
    output_tokens_details: Option<ResponseCompletedOutputTokensDetails>,
    total_tokens: i64,
}

impl From<ResponseCompletedUsage> for TokenUsage {
    fn from(val: ResponseCompletedUsage) -> Self {
        Self {
            input_tokens: val.input_tokens,
            cached_input_tokens: val
                .input_tokens_details
                .map(|d| d.cached_tokens)
                .unwrap_or(0),
            output_tokens: val.output_tokens,
            reasoning_output_tokens: val
                .output_tokens_details
                .map(|d| d.reasoning_tokens)
                .unwrap_or(0),
            total_tokens: val.total_tokens,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedInputTokensDetails {
    cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedOutputTokensDetails {
    reasoning_tokens: i64,
}

#[derive(Deserialize, Debug)]
pub struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    response: Option<Value>,
    item: Option<Value>,
    delta: Option<String>,
    summary_index: Option<i64>,
    content_index: Option<i64>,
}

#[derive(Debug)]
pub enum ResponsesEventError {
    Api(ApiError),
}

impl ResponsesEventError {
    #[must_use]
    pub fn into_api_error(self) -> ApiError {
        match self {
            Self::Api(error) => error,
        }
    }
}

pub fn process_responses_event(
    event: ResponsesStreamEvent,
) -> std::result::Result<Option<ResponseEvent>, ResponsesEventError> {
    match event.kind.as_str() {
        "response.output_item.done" => {
            if let Some(item_val) = event.item {
                if let Ok(item) = serde_json::from_value::<ResponseItem>(item_val) {
                    return Ok(Some(ResponseEvent::OutputItemDone(item)));
                }
                debug!("failed to parse ResponseItem from output_item.done");
            }
        }
        "response.output_text.delta" => {
            if let Some(delta) = event.delta {
                return Ok(Some(ResponseEvent::OutputTextDelta(delta)));
            }
        }
        "response.reasoning_summary_text.delta" => {
            if let (Some(delta), Some(summary_index)) = (event.delta, event.summary_index) {
                return Ok(Some(ResponseEvent::ReasoningSummaryDelta {
                    delta,
                    summary_index,
                }));
            }
        }
        "response.reasoning_text.delta" => {
            if let (Some(delta), Some(content_index)) = (event.delta, event.content_index) {
                return Ok(Some(ResponseEvent::ReasoningContentDelta {
                    delta,
                    content_index,
                }));
            }
        }
        "response.created" => {
            if event.response.is_some() {
                return Ok(Some(ResponseEvent::Created {}));
            }
        }
        "response.failed" => {
            if let Some(resp_val) = event.response {
                let mut response_error = ApiError::Stream("response.failed event received".into());
                if let Some(error) = resp_val.get("error")
                    && let Ok(error) = serde_json::from_value::<Error>(error.clone())
                {
                    if is_context_window_error(&error) {
                        response_error = ApiError::ContextWindowExceeded;
                    } else if is_quota_exceeded_error(&error) {
                        response_error = ApiError::QuotaExceeded;
                    } else if is_usage_not_included(&error) {
                        response_error = ApiError::UsageNotIncluded;
                    } else if is_invalid_prompt_error(&error) {
                        let message = error
                            .message
                            .unwrap_or_else(|| "Invalid request.".to_owned());
                        response_error = ApiError::InvalidRequest { message };
                    } else {
                        let delay = try_parse_retry_after(&error);
                        let message = error.message.unwrap_or_default();
                        response_error = ApiError::Retryable { message, delay };
                    }
                }
                return Err(ResponsesEventError::Api(response_error));
            }

            return Err(ResponsesEventError::Api(ApiError::Stream(
                "response.failed event received".into(),
            )));
        }
        "response.completed" => {
            if let Some(resp_val) = event.response {
                match serde_json::from_value::<ResponseCompleted>(resp_val) {
                    Ok(resp) => {
                        return Ok(Some(ResponseEvent::Completed {
                            response_id: resp.id,
                            token_usage: resp.usage.map(Into::into),
                        }));
                    }
                    Err(err) => {
                        let error = format!("failed to parse ResponseCompleted: {err}");
                        debug!("{error}");
                        return Err(ResponsesEventError::Api(ApiError::Stream(error)));
                    }
                }
            }
        }
        "response.done" => {
            if let Some(resp_val) = event.response {
                match serde_json::from_value::<ResponseDone>(resp_val) {
                    Ok(resp) => {
                        return Ok(Some(ResponseEvent::Completed {
                            response_id: resp.id.unwrap_or_default(),
                            token_usage: resp.usage.map(Into::into),
                        }));
                    }
                    Err(err) => {
                        let error = format!("failed to parse ResponseCompleted: {err}");
                        debug!("{error}");
                        return Err(ResponsesEventError::Api(ApiError::Stream(error)));
                    }
                }
            }

            debug!("response.done missing response payload");
            return Ok(Some(ResponseEvent::Completed {
                response_id: String::new(),
                token_usage: None,
            }));
        }
        "response.output_item.added" => {
            if let Some(item_val) = event.item {
                if let Ok(item) = serde_json::from_value::<ResponseItem>(item_val) {
                    return Ok(Some(ResponseEvent::OutputItemAdded(item)));
                }
                debug!("failed to parse ResponseItem from output_item.added");
            }
        }
        "response.reasoning_summary_part.added" => {
            if let Some(summary_index) = event.summary_index {
                return Ok(Some(ResponseEvent::ReasoningSummaryPartAdded {
                    summary_index,
                }));
            }
        }
        _ => {
            trace!("unhandled responses event: {}", event.kind);
        }
    }

    Ok(None)
}

pub async fn process_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut response_error: Option<ApiError> = None;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("SSE Error: {e:#}");
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                let error = response_error.unwrap_or(ApiError::Stream(
                    "stream closed before response.completed".into(),
                ));
                let _ = tx_event.send(Err(error)).await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        trace!("SSE event: {}", &sse.data);

        let event: ResponsesStreamEvent = match serde_json::from_str(&sse.data) {
            Ok(event) => event,
            Err(e) => {
                debug!("Failed to parse SSE event: {e}, data: {}", &sse.data);
                continue;
            }
        };

        match process_responses_event(event) {
            Ok(Some(event)) => {
                let is_completed = matches!(event, ResponseEvent::Completed { .. });
                if tx_event.send(Ok(event)).await.is_err() {
                    return;
                }
                if is_completed {
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                response_error = Some(error.into_api_error());
            }
        };
    }
}

/// Quiet fixed-text parser. Ordinary Responses parsing remains unchanged.
#[must_use]
pub fn spawn_native_response_stream(
    response: StreamResponse,
    idle_timeout: Duration,
    _telemetry: Option<Arc<dyn SseTelemetry>>,
    _turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    let (tx_event, rx_event) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut stream = response.bytes.eventsource();
        let mut state = NativeTextStreamState::default();
        loop {
            let next = tokio::select! {
                _ = tx_event.closed() => return,
                next = timeout(idle_timeout, stream.next()) => next,
            };
            let event = match next {
                Ok(Some(Ok(event))) if event.data.len() <= 1_048_576 => state.process(&event.data),
                _ => Err(ApiError::Stream(
                    "native SSE unavailable or oversized".into(),
                )),
            };
            if let Ok(events) = event {
                for event in events {
                    let completed = matches!(event, ResponseEvent::Completed { .. });
                    if tx_event.send(Ok(event)).await.is_err() || completed {
                        return;
                    }
                }
            } else {
                let _ = tx_event
                    .send(Err(ApiError::Stream("native SSE rejected".into())))
                    .await;
                return;
            }
        }
    });
    ResponseStream { rx_event }
}

const NATIVE_TEXT_MAX: usize = 65_536;

// No Debug or public DTO: this belongs to one native response connection.
// Standard Responses item/delta frames need not contain response_id; when
// present it must agree. Their item_id/output_index bind the unique message,
// whose final identity and complete text are checked in completed.output.
#[derive(Default)]
struct NativeTextStreamState {
    response_id: Option<String>,
    message_id: Option<String>,
    output_index: Option<u64>,
    parts: std::collections::BTreeMap<u64, String>,
    declared_parts: std::collections::BTreeMap<u64, String>,
    text: String,
    saw_delta: bool,
    message_done: Option<String>,
    completed: bool,
}

impl NativeTextStreamState {
    fn invalid() -> ApiError {
        ApiError::Stream("native SSE identity or complete text rejected".into())
    }

    fn bind_message(&mut self, id: &str, index: u64) -> Result<(), ApiError> {
        if id.is_empty() || id.len() > 1024 {
            return Err(Self::invalid());
        }
        match (&self.message_id, self.output_index) {
            (None, None) => {
                self.message_id = Some(id.to_owned());
                self.output_index = Some(index);
            }
            (Some(original), Some(original_index)) if original == id && original_index == index => {
            }
            _ => return Err(Self::invalid()),
        }
        Ok(())
    }

    fn complete_message(value: &Value) -> Result<String, ApiError> {
        // Typed validation rejects tools, non-assistant messages and non-text.
        native_text_item(value.clone())?;
        if value
            .get("status")
            .is_some_and(|status| status.as_str() != Some("completed"))
        {
            return Err(Self::invalid());
        }
        if value.get("type").and_then(Value::as_str) != Some("message") {
            return Err(Self::invalid());
        }
        let parts = value
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(Self::invalid)?;
        if parts.is_empty() {
            return Err(Self::invalid());
        }
        let mut text = String::new();
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                return Err(Self::invalid());
            }
            let value = part
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(Self::invalid)?;
            if text.len().saturating_add(value.len()) > NATIVE_TEXT_MAX {
                return Err(Self::invalid());
            }
            text.push_str(value);
        }
        if text.trim().is_empty() {
            return Err(Self::invalid());
        }
        Ok(text)
    }

    fn process(&mut self, raw: &str) -> Result<Vec<ResponseEvent>, ApiError> {
        if self.completed {
            return Err(Self::invalid());
        }
        let parsed = parse_native_text_event(raw)?;
        let value: Value = serde_json::from_str(raw).map_err(|_| Self::invalid())?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(Self::invalid)?;
        if kind == "response.created" {
            let response = value.get("response").ok_or_else(Self::invalid)?;
            let id = response
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(Self::invalid)?;
            if self.response_id.is_some()
                || id.is_empty()
                || id.len() > 1024
                || !matches!(
                    response.get("status").and_then(Value::as_str),
                    Some("in_progress" | "queued")
                )
            {
                return Err(Self::invalid());
            }
            self.response_id = Some(id.to_owned());
        } else {
            let response_id = self.response_id.as_deref().ok_or_else(Self::invalid)?;
            if let Some(id) = value.get("response_id")
                && id.as_str() != Some(response_id)
            {
                return Err(Self::invalid());
            }
            if let Some(response) = value.get("response")
                && response.get("id").and_then(Value::as_str) != Some(response_id)
            {
                return Err(Self::invalid());
            }
            if kind == "response.in_progress"
                && value
                    .get("response")
                    .and_then(|response| response.get("status"))
                    .and_then(Value::as_str)
                    != Some("in_progress")
            {
                return Err(Self::invalid());
            }
            match kind {
                "response.output_item.added" | "response.output_item.done" => {
                    let item = value.get("item").ok_or_else(Self::invalid)?;
                    if item.get("type").and_then(Value::as_str) == Some("message") {
                        let id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or_else(Self::invalid)?;
                        let index = value
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .ok_or_else(Self::invalid)?;
                        self.bind_message(id, index)?;
                        if kind.ends_with("done") {
                            if self.message_done.is_some() {
                                return Err(Self::invalid());
                            }
                            let text = Self::complete_message(item)?;
                            if self.saw_delta && text != self.text {
                                return Err(Self::invalid());
                            }
                            self.message_done = Some(text);
                        }
                    }
                }
                "response.output_text.delta"
                | "response.output_text.done"
                | "response.content_part.added"
                | "response.content_part.done" => {
                    let id = value
                        .get("item_id")
                        .and_then(Value::as_str)
                        .ok_or_else(Self::invalid)?;
                    let index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .ok_or_else(Self::invalid)?;
                    let content = value
                        .get("content_index")
                        .and_then(Value::as_u64)
                        .ok_or_else(Self::invalid)?;
                    self.bind_message(id, index)?;
                    if kind == "response.output_text.delta" {
                        if self.message_done.is_some() || self.declared_parts.contains_key(&content)
                        {
                            return Err(Self::invalid());
                        }
                        let delta = value
                            .get("delta")
                            .and_then(Value::as_str)
                            .ok_or_else(Self::invalid)?;
                        if self.text.len().saturating_add(delta.len()) > NATIVE_TEXT_MAX {
                            return Err(Self::invalid());
                        }
                        self.text.push_str(delta);
                        self.parts.entry(content).or_default().push_str(delta);
                        self.saw_delta = true;
                    } else if kind == "response.output_text.done"
                        || kind == "response.content_part.done"
                    {
                        let text = if kind == "response.output_text.done" {
                            value.get("text")
                        } else {
                            value.get("part").and_then(|part| part.get("text"))
                        }
                        .and_then(Value::as_str)
                        .ok_or_else(Self::invalid)?;
                        if text.len() > NATIVE_TEXT_MAX
                            || self.parts.get(&content).is_some_and(|part| part != text)
                        {
                            return Err(Self::invalid());
                        }
                        if self
                            .declared_parts
                            .get(&content)
                            .is_some_and(|part| part != text)
                        {
                            return Err(Self::invalid());
                        }
                        self.declared_parts.insert(content, text.to_owned());
                    }
                }
                "response.completed" | "response.done" => {
                    let response = value.get("response").ok_or_else(Self::invalid)?;
                    let output = response
                        .get("output")
                        .and_then(Value::as_array)
                        .ok_or_else(Self::invalid)?;
                    let messages: Vec<_> = output
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| {
                            item.get("type").and_then(Value::as_str) == Some("message")
                        })
                        .collect();
                    if messages.len() != 1 {
                        return Err(Self::invalid());
                    }
                    let (index, item) = messages[0];
                    let id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(Self::invalid)?;
                    self.bind_message(id, index as u64)?;
                    let complete = Self::complete_message(item)?;
                    if (self.saw_delta && self.text != complete)
                        || self
                            .message_done
                            .as_ref()
                            .is_some_and(|text| text != &complete)
                    {
                        return Err(Self::invalid());
                    }
                    let parts = item
                        .get("content")
                        .and_then(Value::as_array)
                        .ok_or_else(Self::invalid)?;
                    for (index, text) in self.parts.iter().chain(self.declared_parts.iter()) {
                        if parts
                            .get(usize::try_from(*index).map_err(|_| Self::invalid())?)
                            .and_then(|part| part.get("text"))
                            .and_then(Value::as_str)
                            != Some(text.as_str())
                        {
                            return Err(Self::invalid());
                        }
                    }
                    self.completed = true;
                    let mut events = Vec::new();
                    if !self.saw_delta {
                        events.push(ResponseEvent::OutputTextDelta(complete));
                    }
                    events.push(parsed.ok_or_else(Self::invalid)?);
                    return Ok(events);
                }
                _ => {} // Only native parser's explicit no-effect frames remain.
            }
        }
        Ok(parsed.into_iter().collect())
    }
}

fn native_text_item(value: Value) -> Result<ResponseItem, ApiError> {
    let item: ResponseItem = serde_json::from_value(value)
        .map_err(|_| ApiError::Stream("invalid native item".into()))?;
    match &item {
        ResponseItem::Message { role, content, .. }
            if role == "assistant"
                && content.iter().all(|part| {
                    matches!(
                        part,
                        savfox_protocol::models::ContentItem::OutputText { .. }
                    )
                }) =>
        {
            Ok(item)
        }
        ResponseItem::Reasoning { .. } => Ok(item),
        _ => Err(ApiError::Stream("native non-text item rejected".into())),
    }
}

fn parse_native_text_event(raw: &str) -> Result<Option<ResponseEvent>, ApiError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|_| ApiError::Stream("invalid native SSE JSON".into()))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Stream("missing native SSE kind".into()))?;
    let invalid = || ApiError::Stream("invalid native SSE event".into());
    match kind {
        "response.created" => {
            if !value.get("response").is_some_and(Value::is_object) {
                return Err(invalid());
            }
            Ok(Some(ResponseEvent::Created))
        }
        "response.output_item.added" | "response.output_item.done" => {
            let item = native_text_item(value.get("item").cloned().ok_or_else(invalid)?)?;
            if matches!(item, ResponseItem::Reasoning { .. }) {
                return Ok(None);
            }
            Ok(Some(if kind.ends_with("added") {
                ResponseEvent::OutputItemAdded(item)
            } else {
                ResponseEvent::OutputItemDone(item)
            }))
        }
        "response.output_text.delta" => Ok(Some(ResponseEvent::OutputTextDelta(
            value
                .get("delta")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?
                .to_owned(),
        ))),
        "response.completed" | "response.done" => {
            let response = value.get("response").cloned().ok_or_else(invalid)?;
            // The outer event label is insufficient: embedded failure/unknown
            // status must never settle already received text as successful.
            if response.get("status").and_then(Value::as_str) != Some("completed") {
                return Err(invalid());
            }
            for item in response
                .get("output")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?
            {
                native_text_item(item.clone())?;
            }
            let response: ResponseCompleted =
                serde_json::from_value(response).map_err(|_| invalid())?;
            if response.id.is_empty() {
                return Err(invalid());
            }
            Ok(Some(ResponseEvent::Completed {
                response_id: response.id,
                token_usage: response.usage.map(Into::into),
            }))
        }
        // Known no-effect housekeeping frames only; never tool argument frames.
        "response.in_progress"
        | "response.output_text.done"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_text.delta"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_text.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done" => Ok(None),
        "response.content_part.added" | "response.content_part.done" => {
            let part = value.get("part").cloned().ok_or_else(invalid)?;
            let part: savfox_protocol::models::ContentItem =
                serde_json::from_value(part).map_err(|_| invalid())?;
            if !matches!(
                part,
                savfox_protocol::models::ContentItem::OutputText { .. }
            ) {
                return Err(invalid());
            }
            Ok(None)
        }
        // This includes response.failed/incomplete/error, tool calls and unknown kinds.
        _ => Err(invalid()),
    }
}

fn try_parse_retry_after(err: &Error) -> Option<Duration> {
    if err.code.as_deref() != Some("rate_limit_exceeded") {
        return None;
    }

    let re = rate_limit_regex();
    if let Some(message) = &err.message
        && let Some(captures) = re.captures(message)
    {
        let seconds = captures.get(1);
        let unit = captures.get(2);

        if let (Some(value), Some(unit)) = (seconds, unit) {
            let value = value.as_str().parse::<f64>().ok()?;
            let unit = unit.as_str().to_ascii_lowercase();

            if unit == "s" || unit.starts_with("second") {
                return Some(Duration::from_secs_f64(value));
            } else if unit == "ms" {
                return Some(Duration::from_millis(value as u64));
            }
        }
    }
    None
}

fn is_context_window_error(error: &Error) -> bool {
    error.code.as_deref() == Some("context_length_exceeded")
}

fn is_quota_exceeded_error(error: &Error) -> bool {
    error.code.as_deref() == Some("insufficient_quota")
}

fn is_usage_not_included(error: &Error) -> bool {
    error.code.as_deref() == Some("usage_not_included")
}

fn is_invalid_prompt_error(error: &Error) -> bool {
    error.code.as_deref() == Some("invalid_prompt")
}

fn rate_limit_regex() -> &'static regex_lite::Regex {
    static RE: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
    #[expect(clippy::unwrap_used)]
    RE.get_or_init(|| {
        regex_lite::Regex::new(r"(?i)try again in\s*(\d+(?:\.\d+)?)\s*(s|ms|seconds?)").unwrap()
    })
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use bytes::Bytes;
    use futures::stream;
    use pretty_assertions::assert_eq;
    use savfox_protocol::models::{MessagePhase, ResponseItem};
    use serde_json::json;
    use tokio::sync::mpsc;
    use tokio_test::io::Builder as IoBuilder;

    use super::*;

    fn native_created_state() -> NativeTextStreamState {
        let mut state = NativeTextStreamState::default();
        state.process(&json!({"type":"response.created","response":{"id":"native-result","status":"in_progress"}}).to_string()).unwrap();
        state
    }

    fn native_complete_value(text: &str) -> Value {
        json!({"type":"response.completed","response":{"id":"native-result","status":"completed","output":[{
            "type":"message","id":"native-message","role":"assistant","content":[{"type":"output_text","text":text}]
        }]}})
    }

    fn native_delta_value(text: &str) -> Value {
        json!({"type":"response.output_text.delta","item_id":"native-message","output_index":0,"content_index":0,"delta":text})
    }

    #[test]
    fn native_text_completion_requires_actual_completed_status() {
        for kind in ["response.completed", "response.done"] {
            let mut completed = native_complete_value("original plain text");
            completed["type"] = json!(kind);
            let events = native_created_state()
                .process(&completed.to_string())
                .unwrap();
            assert!(
                matches!(events.as_slice(), [ResponseEvent::OutputTextDelta(text), ResponseEvent::Completed { .. }] if text == "original plain text")
            );
            for status in [
                "failed",
                "incomplete",
                "in_progress",
                "cancelled",
                "unknown",
                "",
            ] {
                let mut invalid = completed.clone();
                invalid["response"]["status"] = json!(status);
                assert!(
                    native_created_state()
                        .process(&invalid.to_string())
                        .is_err()
                );
            }
            let mut absent = completed.clone();
            absent["response"].as_object_mut().unwrap().remove("status");
            assert!(native_created_state().process(&absent.to_string()).is_err());
            let mut wrong_type = completed;
            wrong_type["response"]["status"] = json!(true);
            assert!(
                native_created_state()
                    .process(&wrong_type.to_string())
                    .is_err()
            );
        }
    }

    #[test]
    fn native_text_completion_refuses_terminal_tool_unknown_and_missing_identity() {
        for kind in [
            "response.failed",
            "response.incomplete",
            "error",
            "response.function_call_arguments.delta",
            "unregistered",
        ] {
            let event = json!({"type":kind,"response":{"id":"native-result","status":"completed"}});
            assert!(native_created_state().process(&event.to_string()).is_err());
        }
        for field in ["id", "output"] {
            let mut absent = native_complete_value("original plain text");
            absent["response"].as_object_mut().unwrap().remove(field);
            assert!(native_created_state().process(&absent.to_string()).is_err());
        }
        let mut empty_id = native_complete_value("original plain text");
        empty_id["response"]["id"] = json!("");
        assert!(
            native_created_state()
                .process(&empty_id.to_string())
                .is_err()
        );
        let mut tool = native_complete_value("original plain text");
        tool["response"]["output"] = json!([{"type":"function_call","name":"not_allowed","arguments":"{}","call_id":"call"}]);
        assert!(native_created_state().process(&tool.to_string()).is_err());
    }

    #[test]
    fn native_text_delta_is_supported_but_malformed_and_tools_are_refused() {
        let mut state = native_created_state();
        let events = state
            .process(&native_delta_value("original plain text").to_string())
            .unwrap();
        assert!(
            matches!(events.as_slice(), [ResponseEvent::OutputTextDelta(text)] if text == "original plain text")
        );
        let events = state
            .process(&native_complete_value("original plain text").to_string())
            .unwrap();
        assert!(matches!(
            events.as_slice(),
            [ResponseEvent::Completed { .. }]
        ));
        assert!(native_created_state().process("invalid JSON").is_err());
        let mut bad_delta = native_delta_value("original plain text");
        bad_delta["delta"] = json!({"text":"not string"});
        assert!(
            native_created_state()
                .process(&bad_delta.to_string())
                .is_err()
        );
        let tool = json!({"type":"response.output_item.added","item":{"type":"function_call","name":"not_allowed","arguments":"{}","call_id":"call"}});
        assert!(native_created_state().process(&tool.to_string()).is_err());
    }

    #[test]
    fn native_text_stream_refuses_response_and_message_identity_rebinding() {
        assert!(
            NativeTextStreamState::default()
                .process(&native_delta_value("text").to_string())
                .is_err()
        );
        let mut state = native_created_state();
        assert!(state.process(&json!({"type":"response.created","response":{"id":"native-result","status":"in_progress"}}).to_string()).is_err());
        for field in ["response_id", "item_id", "output_index", "content_index"] {
            let mut state = native_created_state();
            state
                .process(&native_delta_value("one").to_string())
                .unwrap();
            let mut delta = native_delta_value("two");
            if field == "response_id" || field == "item_id" {
                delta[field] = json!("different");
            } else {
                delta[field] = json!(1);
            }
            let result = state.process(&delta.to_string());
            // A second content index may be valid, but is not allowed to
            // disguise these deltas as one complete output part.
            if field == "content_index" {
                assert!(result.is_ok());
                assert!(
                    state
                        .process(&native_complete_value("onetwo").to_string())
                        .is_err()
                );
            } else {
                assert!(result.is_err());
            }
        }
        for field in ["item_id", "output_index", "content_index"] {
            let mut delta = native_delta_value("text");
            delta.as_object_mut().unwrap().remove(field);
            assert!(native_created_state().process(&delta.to_string()).is_err());
        }
        let mut state = native_created_state();
        state
            .process(&native_delta_value("text").to_string())
            .unwrap();
        let mut done = native_complete_value("text");
        done["response"]["id"] = json!("another-response");
        assert!(state.process(&done.to_string()).is_err());
        let mut state = native_created_state();
        state
            .process(&native_delta_value("text").to_string())
            .unwrap();
        let mut done = native_complete_value("text");
        done["response"]["output"][0]["id"] = json!("another-message");
        assert!(state.process(&done.to_string()).is_err());
    }

    #[test]
    fn native_text_stream_requires_complete_output_equal_to_all_deltas() {
        let mut state = native_created_state();
        state
            .process(&native_delta_value("one").to_string())
            .unwrap();
        state
            .process(&native_delta_value("two").to_string())
            .unwrap();
        let events = state
            .process(&native_complete_value("onetwo").to_string())
            .unwrap();
        assert!(matches!(
            events.as_slice(),
            [ResponseEvent::Completed { .. }]
        ));
        assert!(
            state
                .process(&native_complete_value("onetwo").to_string())
                .is_err()
        );
        for text in ["different", "one", "", "  "] {
            let mut state = native_created_state();
            state
                .process(&native_delta_value("onetwo").to_string())
                .unwrap();
            assert!(
                state
                    .process(&native_complete_value(text).to_string())
                    .is_err()
            );
        }
        for output in [
            json!([]),
            json!(null),
            json!([{"type":"unknown"}]),
            json!([
                native_complete_value("one")["response"]["output"][0].clone(),
                native_complete_value("two")["response"]["output"][0].clone()
            ]),
        ] {
            let mut done = native_complete_value("one");
            done["response"]["output"] = output;
            assert!(native_created_state().process(&done.to_string()).is_err());
        }
        let mut state = native_created_state();
        let oversized = "x".repeat(NATIVE_TEXT_MAX + 1);
        assert!(
            state
                .process(&native_delta_value(&oversized).to_string())
                .is_err()
        );
        assert!(
            native_created_state()
                .process(&native_complete_value(&oversized).to_string())
                .is_err()
        );
        // Real complete-output-only provider responses emit one text event
        // only after complete response identity/output validation succeeds.
        let events = native_created_state()
            .process(&native_complete_value("complete only").to_string())
            .unwrap();
        assert!(
            matches!(events.as_slice(), [ResponseEvent::OutputTextDelta(text), ResponseEvent::Completed { .. }] if text == "complete only")
        );
    }

    #[tokio::test]
    async fn native_text_stream_eof_never_settles_partial_deltas() {
        let created = json!({"type":"response.created","response":{"id":"native-result","status":"in_progress"}});
        let body = format!(
            "data: {}\n\ndata: {}\n\n",
            created,
            native_delta_value("partial")
        );
        let bytes = ReaderStream::new(std::io::Cursor::new(body))
            .map_err(|_| TransportError::Network("test reader".into()));
        let response = StreamResponse {
            status: http::StatusCode::OK,
            headers: http::HeaderMap::new(),
            bytes: Box::pin(bytes),
        };
        let mut events = spawn_native_response_stream(response, idle_timeout(), None, None);
        let mut completed = false;
        let mut rejected = false;
        while let Some(event) = events.next().await {
            completed |= matches!(&event, Ok(ResponseEvent::Completed { .. }));
            rejected |= event.is_err();
        }
        assert!(rejected && !completed);
    }

    async fn collect_events(chunks: &[&[u8]]) -> Vec<Result<ResponseEvent, ApiError>> {
        let mut builder = IoBuilder::new();
        for chunk in chunks {
            builder.read(chunk);
        }

        let reader = builder.build();
        let stream =
            ReaderStream::new(reader).map_err(|err| TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
        tokio::spawn(process_sse(Box::pin(stream), tx, idle_timeout(), None));

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        events
    }

    async fn run_sse(events: Vec<serde_json::Value>) -> Vec<ResponseEvent> {
        let mut body = String::new();
        for e in events {
            let kind = e
                .get("type")
                .and_then(|v| v.as_str())
                .expect("fixture event missing type");
            if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
                body.push_str(&format!("event: {kind}\n\n"));
            } else {
                body.push_str(&format!("event: {kind}\ndata: {e}\n\n"));
            }
        }

        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
        let stream = ReaderStream::new(std::io::Cursor::new(body))
            .map_err(|err| TransportError::Network(err.to_string()));
        tokio::spawn(process_sse(Box::pin(stream), tx, idle_timeout(), None));

        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev.expect("channel closed"));
        }
        out
    }

    fn idle_timeout() -> Duration {
        Duration::from_millis(1000)
    }

    #[tokio::test]
    async fn parses_items_and_completed() {
        let item1 = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello"}],
                "phase": "commentary"
            }
        })
        .to_string();

        let item2 = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "World"}]
            }
        })
        .to_string();

        let completed = json!({
            "type": "response.completed",
            "response": { "id": "resp1" }
        })
        .to_string();

        let sse1 = format!("event: response.output_item.done\ndata: {item1}\n\n");
        let sse2 = format!("event: response.output_item.done\ndata: {item2}\n\n");
        let sse3 = format!("event: response.completed\ndata: {completed}\n\n");

        let events = collect_events(&[sse1.as_bytes(), sse2.as_bytes(), sse3.as_bytes()]).await;

        assert_eq!(events.len(), 3);

        assert_matches!(
            &events[0],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                role,
                phase: Some(MessagePhase::Commentary),
                ..
            })) if role == "assistant"
        );

        assert_matches!(
            &events[1],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { role, .. }))
                if role == "assistant"
        );

        match &events[2] {
            Ok(ResponseEvent::Completed {
                response_id,
                token_usage,
            }) => {
                assert_eq!(response_id, "resp1");
                assert!(token_usage.is_none());
            }
            other => panic!("unexpected third event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn error_when_missing_completed() {
        let item1 = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello"}]
            }
        })
        .to_string();

        let sse1 = format!("event: response.output_item.done\ndata: {item1}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 2);

        assert_matches!(events[0], Ok(ResponseEvent::OutputItemDone(_)));

        match &events[1] {
            Err(ApiError::Stream(msg)) => {
                assert_eq!(msg, "stream closed before response.completed")
            }
            other => panic!("unexpected second event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn response_done_emits_completed() {
        let done = json!({
            "type": "response.done",
            "response": {
                "usage": {
                    "input_tokens": 1,
                    "input_tokens_details": null,
                    "output_tokens": 2,
                    "output_tokens_details": null,
                    "total_tokens": 3
                }
            }
        })
        .to_string();

        let sse1 = format!("event: response.done\ndata: {done}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);

        match &events[0] {
            Ok(ResponseEvent::Completed {
                response_id,
                token_usage,
            }) => {
                assert_eq!(response_id, "");
                assert!(token_usage.is_some());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn response_done_without_payload_emits_completed() {
        let done = json!({
            "type": "response.done"
        })
        .to_string();

        let sse1 = format!("event: response.done\ndata: {done}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);

        match &events[0] {
            Ok(ResponseEvent::Completed {
                response_id,
                token_usage,
            }) => {
                assert_eq!(response_id, "");
                assert!(token_usage.is_none());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn emits_completed_without_stream_end() {
        let completed = json!({
            "type": "response.completed",
            "response": { "id": "resp1" }
        })
        .to_string();

        let sse1 = format!("event: response.completed\ndata: {completed}\n\n");
        let stream = stream::iter(vec![Ok(Bytes::from(sse1))]).chain(stream::pending());
        let stream: ByteStream = Box::pin(stream);

        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
        tokio::spawn(process_sse(stream, tx, idle_timeout(), None));

        let events = tokio::time::timeout(Duration::from_millis(1000), async {
            let mut events = Vec::new();
            while let Some(ev) = rx.recv().await {
                events.push(ev);
            }
            events
        })
        .await
        .expect("timed out collecting events");

        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(ResponseEvent::Completed {
                response_id,
                token_usage,
            }) => {
                assert_eq!(response_id, "resp1");
                assert!(token_usage.is_none());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn error_when_error_event() {
        let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_689bcf18d7f08194bf3440ba62fe05d803fee0cdac429894","object":"response","created_at":1755041560,"status":"failed","background":false,"error":{"code":"rate_limit_exceeded","message":"Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more."}, "usage":null,"user":null,"metadata":{}}}"#;

        let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);

        match &events[0] {
            Err(ApiError::Retryable { message, delay }) => {
                assert_eq!(
                    message,
                    "Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more."
                );
                assert_eq!(*delay, Some(Duration::from_secs_f64(11.054)));
            }
            other => panic!("unexpected second event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn context_window_error_is_fatal() {
        let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_5c66275b97b9baef1ed95550adb3b7ec13b17aafd1d2f11b","object":"response","created_at":1759510079,"status":"failed","background":false,"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model. Please adjust your input and try again."},"usage":null,"user":null,"metadata":{}}}"#;

        let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);

        assert_matches!(events[0], Err(ApiError::ContextWindowExceeded));
    }

    #[tokio::test]
    async fn context_window_error_with_newline_is_fatal() {
        let raw_error = r#"{"type":"response.failed","sequence_number":4,"response":{"id":"resp_fatal_newline","object":"response","created_at":1759510080,"status":"failed","background":false,"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model. Please adjust your input and try\nagain."},"usage":null,"user":null,"metadata":{}}}"#;

        let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);

        assert_matches!(events[0], Err(ApiError::ContextWindowExceeded));
    }

    #[tokio::test]
    async fn quota_exceeded_error_is_fatal() {
        let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_fatal_quota","object":"response","created_at":1759771626,"status":"failed","background":false,"error":{"code":"insufficient_quota","message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, read the docs: https://platform.openai.com/docs/guides/error-codes/api-errors."},"incomplete_details":null}}"#;

        let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);

        assert_matches!(events[0], Err(ApiError::QuotaExceeded));
    }

    #[tokio::test]
    async fn invalid_prompt_without_type_is_invalid_request() {
        let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_invalid_prompt_no_type","object":"response","created_at":1759771628,"status":"failed","background":false,"error":{"code":"invalid_prompt","message":"Invalid prompt: we've limited access to this content for safety reasons."},"incomplete_details":null}}"#;

        let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);

        match &events[0] {
            Err(ApiError::InvalidRequest { message }) => {
                assert_eq!(
                    message,
                    "Invalid prompt: we've limited access to this content for safety reasons."
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn table_driven_event_kinds() {
        struct TestCase {
            name: &'static str,
            event: serde_json::Value,
            expect_first: fn(&ResponseEvent) -> bool,
            expected_len: usize,
        }

        fn is_created(ev: &ResponseEvent) -> bool {
            matches!(ev, ResponseEvent::Created)
        }
        fn is_output(ev: &ResponseEvent) -> bool {
            matches!(ev, ResponseEvent::OutputItemDone(_))
        }
        fn is_completed(ev: &ResponseEvent) -> bool {
            matches!(ev, ResponseEvent::Completed { .. })
        }

        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": "c",
                "usage": {
                    "input_tokens": 0,
                    "input_tokens_details": null,
                    "output_tokens": 0,
                    "output_tokens_details": null,
                    "total_tokens": 0
                },
                "output": []
            }
        });

        let cases = vec![
            TestCase {
                name: "created",
                event: json!({"type": "response.created", "response": {}}),
                expect_first: is_created,
                expected_len: 2,
            },
            TestCase {
                name: "output_item.done",
                event: json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {"type": "output_text", "text": "hi"}
                        ]
                    }
                }),
                expect_first: is_output,
                expected_len: 2,
            },
            TestCase {
                name: "unknown",
                event: json!({"type": "response.new_tool_event"}),
                expect_first: is_completed,
                expected_len: 1,
            },
        ];

        for case in cases {
            let mut evs = vec![case.event];
            evs.push(completed.clone());

            let out = run_sse(evs).await;
            assert_eq!(out.len(), case.expected_len, "case {}", case.name);
            assert!(
                (case.expect_first)(&out[0]),
                "first event mismatch in case {}",
                case.name
            );
        }
    }

    #[test]
    fn test_try_parse_retry_after() {
        let err = Error {
            message: Some("Rate limit reached for gpt-5.1 in organization org- on tokens per min (TPM): Limit 1, Used 1, Requested 19304. Please try again in 28ms. Visit https://platform.openai.com/account/rate-limits to learn more.".to_owned()),
            code: Some("rate_limit_exceeded".to_owned()),
        };

        let delay = try_parse_retry_after(&err);
        assert_eq!(delay, Some(Duration::from_millis(28)));
    }

    #[test]
    fn test_try_parse_retry_after_no_delay() {
        let err = Error {
            message: Some("Rate limit reached for gpt-5.1 in organization <ORG> on tokens per min (TPM): Limit 30000, Used 6899, Requested 24050. Please try again in 1.898s. Visit https://platform.openai.com/account/rate-limits to learn more.".to_owned()),
            code: Some("rate_limit_exceeded".to_owned()),
        };
        let delay = try_parse_retry_after(&err);
        assert_eq!(delay, Some(Duration::from_secs_f64(1.898)));
    }

    #[test]
    fn test_try_parse_retry_after_azure() {
        let err = Error {
            message: Some("Rate limit exceeded. Try again in 35 seconds.".to_owned()),
            code: Some("rate_limit_exceeded".to_owned()),
        };
        let delay = try_parse_retry_after(&err);
        assert_eq!(delay, Some(Duration::from_secs(35)));
    }
}
