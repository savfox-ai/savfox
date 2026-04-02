use std::io;
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, Utc};
use reqwest::StatusCode;
use savfox_async_utils::CancelErr;
use savfox_protocol::SessionId;
use savfox_protocol::protocol::{ErrorEvent, RateLimitSnapshot, SavfoxErrorInfo};
use serde_json;
use thiserror::Error;
use tokio::task::JoinError;

use crate::exec::ExecToolCallOutput;
use crate::token_data::{KnownPlan, PlanType};
use crate::truncate::{TruncationPolicy, truncate_text};

/// Result type alias for Savfox operations.
pub type Result<T> = std::result::Result<T, SavfoxError>;

/// Limit UI error messages to a reasonable size while keeping useful context.
const ERROR_MESSAGE_UI_MAX_BYTES: usize = 2 * 1024; // 2 KiB

/// Errors related to sandbox execution environments.
///
/// These errors occur when running commands in a restricted/sandboxed environment
/// for security purposes.
#[derive(Error, Debug)]
pub enum SandboxErr {
    /// Error from sandbox execution
    #[error(
        "sandbox denied exec error, exit code: {}, stdout: {}, stderr: {}",
        .output.exit_code, .output.stdout.text, .output.stderr.text
    )]
    Denied { output: Box<ExecToolCallOutput> },

    /// Error from linux seccomp filter setup
    #[cfg(target_os = "linux")]
    #[error("seccomp setup error")]
    SeccompInstall(#[from] seccompiler::Error),

    /// Error from linux seccomp backend
    #[cfg(target_os = "linux")]
    #[error("seccomp backend error")]
    SeccompBackend(#[from] seccompiler::BackendError),

    /// Command timed out
    #[error("command timed out")]
    Timeout { output: Box<ExecToolCallOutput> },

    /// Command was killed by a signal
    #[error("command was killed by a signal")]
    Signal(i32),

    /// Error from linux landlock
    #[error("Landlock was not able to fully enforce all sandbox rules")]
    LandlockRestrict,
}

/// Main error type for the Savfox core library.
///
/// This enum represents all possible errors that can occur when interacting with
/// the Savfox system, including network errors, authentication failures, quota issues,
/// and internal errors.
///
/// # Error Categories
///
/// - **Transient Errors**: Can be retried (e.g., `Stream`, `ConnectionFailed`)
/// - **Permanent Errors**: Should not be retried (e.g., `InvalidRequest`, `QuotaExceeded`)
/// - **User Errors**: Require user action (e.g., `UsageNotIncluded`, `ContextWindowExceeded`)
///
/// Use [`SavfoxError::is_retryable()`] to determine if an error can be safely retried.
///
/// # Related Types
///
/// - [`savfox_protocol::protocol::SavfoxErrorInfo`]: Client-facing error information (serializable)
/// - [`crate::error::SandboxErr`]: Sandbox-specific errors
///
/// # Conversions
///
/// `SavfoxError` can be converted to `SavfoxErrorInfo` for client responses:
/// ```ignore
/// use savfox_core::error::SavfoxError;
/// use savfox_protocol::protocol::SavfoxErrorInfo;
///
/// let error = SavfoxError::ContextWindowExceeded;
/// let info: SavfoxErrorInfo = error.into();
/// ```
#[derive(Error, Debug)]
pub enum SavfoxError {
    /// Turn was aborted, typically due to cancellation or internal error.
    #[error("turn aborted. Something went wrong? Hit `/feedback` to report the issue.")]
    TurnAborted,

    /// Returned by ResponsesClient when the SSE stream disconnects or errors out **after** the HTTP
    /// handshake has succeeded but **before** it finished emitting `response.completed`.
    ///
    /// The Session loop treats this as a transient error and will automatically retry the turn.
    ///
    /// Optionally includes the requested delay before retrying the turn.
    #[error("stream disconnected before completion: {0}")]
    Stream(String, Option<Duration>),

    #[error(
        "Savfox ran out of room in the model's context window. Start a new session or clear earlier history before retrying."
    )]
    ContextWindowExceeded,

    #[error("no session with id: {0}")]
    SessionNotFound(SessionId),

    #[error("agent session limit reached (max {max_sessions})")]
    AgentLimitReached { max_sessions: usize },

    #[error("session configured event was not the first event in the stream")]
    SessionConfiguredNotFirstEvent,

    /// Returned by run_command_stream when the spawned child process timed out (10s).
    #[error("timeout waiting for child process to exit")]
    Timeout,

    /// Returned by run_command_stream when the child could not be spawned (its stdout/stderr pipes
    /// could not be captured). Analogous to the previous `SavfoxError::Spawn` variant.
    #[error("spawn failed: child stdout/stderr not captured")]
    Spawn,

    /// Returned by run_command_stream when the user pressed Ctrl‑C (SIGINT). Session uses this to
    /// surface a polite FunctionCallOutput back to the model instead of crashing the CLI.
    #[error("interrupted (Ctrl-C). Something went wrong? Hit `/feedback` to report the issue.")]
    Interrupted,

    /// Unexpected HTTP status code.
    #[error("{0}")]
    UnexpectedStatus(UnexpectedResponseError),

    /// Invalid request.
    #[error("{0}")]
    InvalidRequest(String),

    /// Invalid image.
    #[error("Image poisoning")]
    InvalidImageRequest(),

    #[error("{0}")]
    UsageLimitReached(UsageLimitReachedError),

    #[error("{0}")]
    ModelCap(ModelCapError),

    #[error("{0}")]
    ResponseStreamFailed(ResponseStreamFailed),

    #[error("{0}")]
    ConnectionFailed(ConnectionFailedError),

    #[error("Quota exceeded. Check your plan and billing details.")]
    QuotaExceeded,

    #[error(
        "To use Savfox with your ChatGPT plan, upgrade to Plus: https://savfox.ai/explore/plus."
    )]
    UsageNotIncluded,

    #[error("We're currently experiencing high demand, which may cause temporary errors.")]
    InternalServerError,

    /// Retry limit exceeded.
    #[error("{0}")]
    RetryLimit(RetryLimitReachedError),

    /// Agent loop died unexpectedly
    #[error("internal error; agent loop died unexpectedly")]
    InternalAgentDied,

    /// Sandbox error
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxErr),

    #[error("savfox-linux-sandbox was required but not provided")]
    LandlockSandboxExecutableNotProvided,

    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),

    #[error("{0}")]
    RefreshTokenFailed(RefreshTokenFailedError),

    #[error("Fatal error: {0}")]
    Fatal(String),

    // -----------------------------------------------------------------
    // Automatic conversions for common external error types
    // -----------------------------------------------------------------
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    LandlockRuleset(#[from] landlock::RulesetError),

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    LandlockPathFd(#[from] landlock::PathFdError),

    #[error(transparent)]
    TokioJoin(#[from] JoinError),

    #[error("{0}")]
    EnvVar(EnvVarError),
}

impl From<CancelErr> for SavfoxError {
    fn from(_: CancelErr) -> Self {
        Self::TurnAborted
    }
}

impl SavfoxError {
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::TurnAborted
            | Self::Interrupted
            | Self::EnvVar(_)
            | Self::Fatal(_)
            | Self::UsageNotIncluded
            | Self::QuotaExceeded
            | Self::InvalidImageRequest()
            | Self::InvalidRequest(_)
            | Self::RefreshTokenFailed(_)
            | Self::UnsupportedOperation(_)
            | Self::Sandbox(_)
            | Self::LandlockSandboxExecutableNotProvided
            | Self::RetryLimit(_)
            | Self::ContextWindowExceeded
            | Self::SessionNotFound(_)
            | Self::AgentLimitReached { .. }
            | Self::Spawn
            | Self::SessionConfiguredNotFirstEvent
            | Self::UsageLimitReached(_)
            | Self::ModelCap(_) => false,
            Self::Stream(..)
            | Self::Timeout
            | Self::UnexpectedStatus(_)
            | Self::ResponseStreamFailed(_)
            | Self::ConnectionFailed(_)
            | Self::InternalServerError
            | Self::InternalAgentDied
            | Self::Io(_)
            | Self::Json(_)
            | Self::TokioJoin(_) => true,
            #[cfg(target_os = "linux")]
            SavfoxError::LandlockRuleset(_) | SavfoxError::LandlockPathFd(_) => false,
        }
    }
}

#[derive(Debug)]
pub struct ConnectionFailedError {
    pub source: reqwest::Error,
}

impl std::fmt::Display for ConnectionFailedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Connection failed: {}", self.source)
    }
}

#[derive(Debug)]
pub struct ResponseStreamFailed {
    pub source: reqwest::Error,
    pub request_id: Option<String>,
}

impl std::fmt::Display for ResponseStreamFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Error while reading the server response: {}{}",
            self.source,
            self.request_id
                .as_ref()
                .map(|id| format!(", request id: {id}"))
                .unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct RefreshTokenFailedError {
    pub reason: RefreshTokenFailedReason,
    pub message: String,
}

impl RefreshTokenFailedError {
    pub fn new(reason: RefreshTokenFailedReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTokenFailedReason {
    Expired,
    Exhausted,
    Revoked,
    Other,
}

#[derive(Debug)]
pub struct UnexpectedResponseError {
    pub status: StatusCode,
    pub body: String,
    pub url: Option<String>,
    pub request_id: Option<String>,
}

const CLOUDFLARE_BLOCKED_MESSAGE: &str =
    "Access blocked by Cloudflare. This usually happens when connecting from a restricted region";

impl UnexpectedResponseError {
    fn friendly_message(&self) -> Option<String> {
        if self.status != StatusCode::FORBIDDEN {
            return None;
        }

        if !self.body.contains("Cloudflare") || !self.body.contains("blocked") {
            return None;
        }

        let status = self.status;
        let mut message = format!("{CLOUDFLARE_BLOCKED_MESSAGE} (status {status})");
        if let Some(url) = &self.url {
            message.push_str(&format!(", url: {url}"));
        }
        if let Some(id) = &self.request_id {
            message.push_str(&format!(", request id: {id}"));
        }

        Some(message)
    }
}

impl std::fmt::Display for UnexpectedResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(friendly) = self.friendly_message() {
            write!(f, "{friendly}")
        } else {
            let status = self.status;
            let body = &self.body;
            let mut message = format!("unexpected status {status}: {body}");
            if let Some(url) = &self.url {
                message.push_str(&format!(", url: {url}"));
            }
            if let Some(id) = &self.request_id {
                message.push_str(&format!(", request id: {id}"));
            }
            write!(f, "{message}")
        }
    }
}

impl std::error::Error for UnexpectedResponseError {}
#[derive(Debug)]
pub struct RetryLimitReachedError {
    pub status: StatusCode,
    pub request_id: Option<String>,
}

impl std::fmt::Display for RetryLimitReachedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exceeded retry limit, last status: {}{}",
            self.status,
            self.request_id
                .as_ref()
                .map(|id| format!(", request id: {id}"))
                .unwrap_or_default()
        )
    }
}

#[derive(Debug)]
pub struct UsageLimitReachedError {
    pub(crate) plan_type: Option<PlanType>,
    pub(crate) resets_at: Option<DateTime<Utc>>,
    pub(crate) rate_limits: Option<RateLimitSnapshot>,
    pub(crate) promo_message: Option<String>,
}

impl std::fmt::Display for UsageLimitReachedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(promo_message) = &self.promo_message {
            return write!(
                f,
                "You've hit your usage limit. {promo_message},{}",
                retry_suffix_after_or(self.resets_at.as_ref())
            );
        }

        let message = match self.plan_type.as_ref() {
            Some(PlanType::Known(KnownPlan::Plus)) => format!(
                "You've hit your usage limit. Upgrade to Pro (https://savfox.ai/explore/pro), visit https://savfox.ai/savfox/settings/usage to purchase more credits{}",
                retry_suffix_after_or(self.resets_at.as_ref())
            ),
            Some(PlanType::Known(KnownPlan::Team | KnownPlan::Business)) => {
                format!(
                    "You've hit your usage limit. To get more access now, send a request to your admin{}",
                    retry_suffix_after_or(self.resets_at.as_ref())
                )
            }
            Some(PlanType::Known(KnownPlan::Free | KnownPlan::Go)) => {
                format!(
                    "You've hit your usage limit. Upgrade to Plus to continue using Savfox (https://savfox.ai/explore/plus),{}",
                    retry_suffix_after_or(self.resets_at.as_ref())
                )
            }
            Some(PlanType::Known(KnownPlan::Pro)) => format!(
                "You've hit your usage limit. Visit https://savfox.ai/savfox/settings/usage to purchase more credits{}",
                retry_suffix_after_or(self.resets_at.as_ref())
            ),
            Some(PlanType::Known(KnownPlan::Enterprise | KnownPlan::Edu)) => format!(
                "You've hit your usage limit.{}",
                retry_suffix(self.resets_at.as_ref())
            ),
            Some(PlanType::Unknown(_)) | None => format!(
                "You've hit your usage limit.{}",
                retry_suffix(self.resets_at.as_ref())
            ),
        };

        write!(f, "{message}")
    }
}

#[derive(Debug)]
pub struct ModelCapError {
    pub(crate) model: String,
    pub(crate) reset_after_seconds: Option<u64>,
}

impl std::fmt::Display for ModelCapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut message = format!(
            "Model {} is at capacity. Please try a different model.",
            self.model
        );
        if let Some(seconds) = self.reset_after_seconds {
            message.push_str(&format!(
                " Try again in {}.",
                format_duration_short(seconds)
            ));
        } else {
            message.push_str(" Try again later.");
        }
        write!(f, "{message}")
    }
}

fn retry_suffix(resets_at: Option<&DateTime<Utc>>) -> String {
    if let Some(resets_at) = resets_at {
        let formatted = format_retry_timestamp(resets_at);
        format!(" Try again at {formatted}.")
    } else {
        " Try again later.".to_owned()
    }
}

fn retry_suffix_after_or(resets_at: Option<&DateTime<Utc>>) -> String {
    if let Some(resets_at) = resets_at {
        let formatted = format_retry_timestamp(resets_at);
        format!(" or try again at {formatted}.")
    } else {
        " or try again later.".to_owned()
    }
}

fn format_retry_timestamp(resets_at: &DateTime<Utc>) -> String {
    let local_reset = resets_at.with_timezone(&Local);
    let local_now = now_for_retry().with_timezone(&Local);
    if local_reset.date_naive() == local_now.date_naive() {
        local_reset.format("%-I:%M %p").to_string()
    } else {
        let suffix = day_suffix(local_reset.day());
        local_reset
            .format(&format!("%b %-d{suffix}, %Y %-I:%M %p"))
            .to_string()
    }
}

fn format_duration_short(seconds: u64) -> String {
    if seconds < 60 {
        "less than a minute".to_owned()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn day_suffix(day: u32) -> &'static str {
    match day {
        11..=13 => "th",
        _ => match day % 10 {
            1 => "st",
            2 => "nd", // codespell:ignore
            3 => "rd",
            _ => "th",
        },
    }
}

#[cfg(test)]
thread_local! {
    static NOW_OVERRIDE: std::cell::RefCell<Option<DateTime<Utc>>> =
        const { std::cell::RefCell::new(None) };
}

fn now_for_retry() -> DateTime<Utc> {
    #[cfg(test)]
    {
        if let Some(now) = NOW_OVERRIDE.with(|cell| *cell.borrow()) {
            return now;
        }
    }
    Utc::now()
}

#[derive(Debug)]
pub struct EnvVarError {
    /// Name of the environment variable that is missing.
    pub var: String,

    /// Optional instructions to help the user get a valid value for the
    /// variable and set it.
    pub instructions: Option<String>,
}

impl std::fmt::Display for EnvVarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Missing environment variable: `{}`.", self.var)?;
        if let Some(instructions) = &self.instructions {
            write!(f, " {instructions}")?;
        }
        Ok(())
    }
}

impl SavfoxError {
    /// Minimal shim so that existing `e.downcast_ref::<SavfoxError>()` checks continue to compile
    /// after replacing `anyhow::Error` in the return signature. This mirrors the behavior of
    /// `anyhow::Error::downcast_ref` but works directly on our concrete enum.
    #[must_use]
    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        (self as &dyn std::any::Any).downcast_ref::<T>()
    }

    /// Translate core error to client-facing protocol error.
    #[must_use]
    pub fn to_savfox_protocol_error(&self) -> SavfoxErrorInfo {
        match self {
            Self::ContextWindowExceeded => SavfoxErrorInfo::ContextWindowExceeded,
            Self::UsageLimitReached(_) | Self::QuotaExceeded | Self::UsageNotIncluded => {
                SavfoxErrorInfo::UsageLimitExceeded
            }
            Self::ModelCap(err) => SavfoxErrorInfo::ModelCap {
                model: err.model.clone(),
                reset_after_seconds: err.reset_after_seconds,
            },
            Self::RetryLimit(_) => SavfoxErrorInfo::ResponseTooManyFailedAttempts {
                http_status_code: self.http_status_code_value(),
            },
            Self::ConnectionFailed(_) => SavfoxErrorInfo::HttpConnectionFailed {
                http_status_code: self.http_status_code_value(),
            },
            Self::ResponseStreamFailed(_) => SavfoxErrorInfo::ResponseStreamConnectionFailed {
                http_status_code: self.http_status_code_value(),
            },
            Self::RefreshTokenFailed(_) => SavfoxErrorInfo::Unauthorized,
            Self::SessionConfiguredNotFirstEvent
            | Self::InternalServerError
            | Self::InternalAgentDied => SavfoxErrorInfo::InternalServerError,
            Self::UnsupportedOperation(_)
            | Self::SessionNotFound(_)
            | Self::AgentLimitReached { .. } => SavfoxErrorInfo::BadRequest,
            Self::Sandbox(_) => SavfoxErrorInfo::SandboxError,
            _ => SavfoxErrorInfo::Other,
        }
    }

    #[must_use]
    pub fn to_error_event(&self, message_prefix: Option<String>) -> ErrorEvent {
        let error_message = self.to_string();
        let message: String = match message_prefix {
            Some(prefix) => format!("{prefix}: {error_message}"),
            None => error_message,
        };
        ErrorEvent {
            message,
            savfox_error_info: Some(self.to_savfox_protocol_error()),
        }
    }

    pub fn http_status_code_value(&self) -> Option<u16> {
        let http_status_code = match self {
            Self::RetryLimit(err) => Some(err.status),
            Self::UnexpectedStatus(err) => Some(err.status),
            Self::ConnectionFailed(err) => err.source.status(),
            Self::ResponseStreamFailed(err) => err.source.status(),
            _ => None,
        };
        http_status_code.as_ref().map(StatusCode::as_u16)
    }
}

#[must_use]
pub fn get_error_message_ui(e: &SavfoxError) -> String {
    let message = match e {
        SavfoxError::Sandbox(SandboxErr::Denied { output }) => {
            let aggregated = output.aggregated_output.text.trim();
            if !aggregated.is_empty() {
                output.aggregated_output.text.clone()
            } else {
                let stderr = output.stderr.text.trim();
                let stdout = output.stdout.text.trim();
                match (stderr.is_empty(), stdout.is_empty()) {
                    (false, false) => format!("{stderr}\n{stdout}"),
                    (false, true) => output.stderr.text.clone(),
                    (true, false) => output.stdout.text.clone(),
                    (true, true) => format!(
                        "command failed inside sandbox with exit code {}",
                        output.exit_code
                    ),
                }
            }
        }
        // Timeouts are not sandbox errors from a UX perspective; present them plainly
        SavfoxError::Sandbox(SandboxErr::Timeout { output }) => {
            format!(
                "error: command timed out after {} ms",
                output.duration.as_millis()
            )
        }
        _ => e.to_string(),
    };

    truncate_text(
        &message,
        TruncationPolicy::Bytes(ERROR_MESSAGE_UI_MAX_BYTES),
    )
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
    use pretty_assertions::assert_eq;
    use reqwest::{Response, ResponseBuilderExt, StatusCode, Url};
    use savfox_protocol::protocol::RateLimitWindow;

    use super::*;
    use crate::exec::StreamOutput;

    fn rate_limit_snapshot() -> RateLimitSnapshot {
        let primary_reset_at = Utc
            .with_ymd_and_hms(2024, 1, 1, 1, 0, 0)
            .unwrap()
            .timestamp();
        let secondary_reset_at = Utc
            .with_ymd_and_hms(2024, 1, 1, 2, 0, 0)
            .unwrap()
            .timestamp();
        RateLimitSnapshot {
            primary: Some(RateLimitWindow {
                used_percent: 50.0,
                window_minutes: Some(60),
                resets_at: Some(primary_reset_at),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(120),
                resets_at: Some(secondary_reset_at),
            }),
            credits: None,
            plan_type: None,
        }
    }

    fn with_now_override<T>(now: DateTime<Utc>, f: impl FnOnce() -> T) -> T {
        NOW_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = Some(now);
            let result = f();
            *cell.borrow_mut() = None;
            result
        })
    }

    #[test]
    fn usage_limit_reached_error_formats_plus_plan() {
        let err = UsageLimitReachedError {
            plan_type: Some(PlanType::Known(KnownPlan::Plus)),
            resets_at: None,
            rate_limits: Some(rate_limit_snapshot()),
            promo_message: None,
        };
        assert_eq!(
            err.to_string(),
            "You've hit your usage limit. Upgrade to Pro (https://savfox.ai/explore/pro), visit https://savfox.ai/savfox/settings/usage to purchase more credits or try again later."
        );
    }

    #[test]
    fn model_cap_error_formats_message() {
        let err = ModelCapError {
            model: "boomslang".to_owned(),
            reset_after_seconds: Some(120),
        };
        assert_eq!(
            err.to_string(),
            "Model boomslang is at capacity. Please try a different model. Try again in 2m."
        );
    }

    #[test]
    fn model_cap_error_formats_message_without_reset() {
        let err = ModelCapError {
            model: "boomslang".to_owned(),
            reset_after_seconds: None,
        };
        assert_eq!(
            err.to_string(),
            "Model boomslang is at capacity. Please try a different model. Try again later."
        );
    }

    #[test]
    fn model_cap_error_maps_to_protocol() {
        let err = SavfoxError::ModelCap(ModelCapError {
            model: "boomslang".to_owned(),
            reset_after_seconds: Some(30),
        });
        assert_eq!(
            err.to_savfox_protocol_error(),
            SavfoxErrorInfo::ModelCap {
                model: "boomslang".to_owned(),
                reset_after_seconds: Some(30),
            }
        );
    }

    #[test]
    fn sandbox_denied_uses_aggregated_output_when_stderr_empty() {
        let output = ExecToolCallOutput {
            exit_code: 77,
            stdout: StreamOutput::new(String::new()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new("aggregate detail".to_owned()),
            duration: Duration::from_millis(10),
            timed_out: false,
        };
        let err = SavfoxError::Sandbox(SandboxErr::Denied {
            output: Box::new(output),
        });
        assert_eq!(get_error_message_ui(&err), "aggregate detail");
    }

    #[test]
    fn sandbox_denied_reports_both_streams_when_available() {
        let output = ExecToolCallOutput {
            exit_code: 9,
            stdout: StreamOutput::new("stdout detail".to_owned()),
            stderr: StreamOutput::new("stderr detail".to_owned()),
            aggregated_output: StreamOutput::new(String::new()),
            duration: Duration::from_millis(10),
            timed_out: false,
        };
        let err = SavfoxError::Sandbox(SandboxErr::Denied {
            output: Box::new(output),
        });
        assert_eq!(get_error_message_ui(&err), "stderr detail\nstdout detail");
    }

    #[test]
    fn sandbox_denied_reports_stdout_when_no_stderr() {
        let output = ExecToolCallOutput {
            exit_code: 11,
            stdout: StreamOutput::new("stdout only".to_owned()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new(String::new()),
            duration: Duration::from_millis(8),
            timed_out: false,
        };
        let err = SavfoxError::Sandbox(SandboxErr::Denied {
            output: Box::new(output),
        });
        assert_eq!(get_error_message_ui(&err), "stdout only");
    }

    #[test]
    fn to_error_event_handles_response_stream_failed() {
        let response = http::Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .url(Url::parse("http://example.com").unwrap())
            .body("")
            .unwrap();
        let source = Response::from(response).error_for_status_ref().unwrap_err();
        let err = SavfoxError::ResponseStreamFailed(ResponseStreamFailed {
            source,
            request_id: Some("req-123".to_owned()),
        });

        let event = err.to_error_event(Some("prefix".to_owned()));

        assert_eq!(
            event.message,
            "prefix: Error while reading the server response: HTTP status client error (429 Too Many Requests) for url (http://example.com/), request id: req-123"
        );
        assert_eq!(
            event.savfox_error_info,
            Some(SavfoxErrorInfo::ResponseStreamConnectionFailed {
                http_status_code: Some(429)
            })
        );
    }

    #[test]
    fn sandbox_denied_reports_exit_code_when_no_output_available() {
        let output = ExecToolCallOutput {
            exit_code: 13,
            stdout: StreamOutput::new(String::new()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new(String::new()),
            duration: Duration::from_millis(5),
            timed_out: false,
        };
        let err = SavfoxError::Sandbox(SandboxErr::Denied {
            output: Box::new(output),
        });
        assert_eq!(
            get_error_message_ui(&err),
            "command failed inside sandbox with exit code 13"
        );
    }

    #[test]
    fn usage_limit_reached_error_formats_free_plan() {
        let err = UsageLimitReachedError {
            plan_type: Some(PlanType::Known(KnownPlan::Free)),
            resets_at: None,
            rate_limits: Some(rate_limit_snapshot()),
            promo_message: None,
        };
        assert_eq!(
            err.to_string(),
            "You've hit your usage limit. Upgrade to Plus to continue using Savfox (https://savfox.ai/explore/plus), or try again later."
        );
    }

    #[test]
    fn usage_limit_reached_error_formats_go_plan() {
        let err = UsageLimitReachedError {
            plan_type: Some(PlanType::Known(KnownPlan::Go)),
            resets_at: None,
            rate_limits: Some(rate_limit_snapshot()),
            promo_message: None,
        };
        assert_eq!(
            err.to_string(),
            "You've hit your usage limit. Upgrade to Plus to continue using Savfox (https://savfox.ai/explore/plus), or try again later."
        );
    }

    #[test]
    fn usage_limit_reached_error_formats_default_when_none() {
        let err = UsageLimitReachedError {
            plan_type: None,
            resets_at: None,
            rate_limits: Some(rate_limit_snapshot()),
            promo_message: None,
        };
        assert_eq!(
            err.to_string(),
            "You've hit your usage limit. Try again later."
        );
    }

    #[test]
    fn usage_limit_reached_error_formats_team_plan() {
        let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let resets_at = base + ChronoDuration::hours(1);
        with_now_override(base, move || {
            let expected_time = format_retry_timestamp(&resets_at);
            let err = UsageLimitReachedError {
                plan_type: Some(PlanType::Known(KnownPlan::Team)),
                resets_at: Some(resets_at),
                rate_limits: Some(rate_limit_snapshot()),
                promo_message: None,
            };
            let expected = format!(
                "You've hit your usage limit. To get more access now, send a request to your admin or try again at {expected_time}."
            );
            assert_eq!(err.to_string(), expected);
        });
    }

    #[test]
    fn usage_limit_reached_error_formats_business_plan_without_reset() {
        let err = UsageLimitReachedError {
            plan_type: Some(PlanType::Known(KnownPlan::Business)),
            resets_at: None,
            rate_limits: Some(rate_limit_snapshot()),
            promo_message: None,
        };
        assert_eq!(
            err.to_string(),
            "You've hit your usage limit. To get more access now, send a request to your admin or try again later."
        );
    }

    #[test]
    fn usage_limit_reached_error_formats_default_for_other_plans() {
        let err = UsageLimitReachedError {
            plan_type: Some(PlanType::Known(KnownPlan::Enterprise)),
            resets_at: None,
            rate_limits: Some(rate_limit_snapshot()),
            promo_message: None,
        };
        assert_eq!(
            err.to_string(),
            "You've hit your usage limit. Try again later."
        );
    }

    #[test]
    fn usage_limit_reached_error_formats_pro_plan_with_reset() {
        let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let resets_at = base + ChronoDuration::hours(1);
        with_now_override(base, move || {
            let expected_time = format_retry_timestamp(&resets_at);
            let err = UsageLimitReachedError {
                plan_type: Some(PlanType::Known(KnownPlan::Pro)),
                resets_at: Some(resets_at),
                rate_limits: Some(rate_limit_snapshot()),
                promo_message: None,
            };
            let expected = format!(
                "You've hit your usage limit. Visit https://savfox.ai/savfox/settings/usage to purchase more credits or try again at {expected_time}."
            );
            assert_eq!(err.to_string(), expected);
        });
    }

    #[test]
    fn usage_limit_reached_includes_minutes_when_available() {
        let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let resets_at = base + ChronoDuration::minutes(5);
        with_now_override(base, move || {
            let expected_time = format_retry_timestamp(&resets_at);
            let err = UsageLimitReachedError {
                plan_type: None,
                resets_at: Some(resets_at),
                rate_limits: Some(rate_limit_snapshot()),
                promo_message: None,
            };
            let expected = format!("You've hit your usage limit. Try again at {expected_time}.");
            assert_eq!(err.to_string(), expected);
        });
    }

    #[test]
    fn unexpected_status_cloudflare_html_is_simplified() {
        let err = UnexpectedResponseError {
            status: StatusCode::FORBIDDEN,
            body: "<html><body>Cloudflare error: Sorry, you have been blocked</body></html>"
                .to_owned(),
            url: Some("http://example.com/blocked".to_owned()),
            request_id: Some("ray-id".to_owned()),
        };
        let status = StatusCode::FORBIDDEN.to_string();
        let url = "http://example.com/blocked";
        assert_eq!(
            err.to_string(),
            format!(
                "{CLOUDFLARE_BLOCKED_MESSAGE} (status {status}), url: {url}, request id: ray-id"
            )
        );
    }

    #[test]
    fn unexpected_status_non_html_is_unchanged() {
        let err = UnexpectedResponseError {
            status: StatusCode::FORBIDDEN,
            body: "plain text error".to_owned(),
            url: Some("http://example.com/plain".to_owned()),
            request_id: None,
        };
        let status = StatusCode::FORBIDDEN.to_string();
        let url = "http://example.com/plain";
        assert_eq!(
            err.to_string(),
            format!("unexpected status {status}: plain text error, url: {url}")
        );
    }

    #[test]
    fn usage_limit_reached_includes_hours_and_minutes() {
        let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let resets_at = base + ChronoDuration::hours(3) + ChronoDuration::minutes(32);
        with_now_override(base, move || {
            let expected_time = format_retry_timestamp(&resets_at);
            let err = UsageLimitReachedError {
                plan_type: Some(PlanType::Known(KnownPlan::Plus)),
                resets_at: Some(resets_at),
                rate_limits: Some(rate_limit_snapshot()),
                promo_message: None,
            };
            let expected = format!(
                "You've hit your usage limit. Upgrade to Pro (https://savfox.ai/explore/pro), visit https://savfox.ai/savfox/settings/usage to purchase more credits or try again at {expected_time}."
            );
            assert_eq!(err.to_string(), expected);
        });
    }

    #[test]
    fn usage_limit_reached_includes_days_hours_minutes() {
        let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let resets_at =
            base + ChronoDuration::days(2) + ChronoDuration::hours(3) + ChronoDuration::minutes(5);
        with_now_override(base, move || {
            let expected_time = format_retry_timestamp(&resets_at);
            let err = UsageLimitReachedError {
                plan_type: None,
                resets_at: Some(resets_at),
                rate_limits: Some(rate_limit_snapshot()),
                promo_message: None,
            };
            let expected = format!("You've hit your usage limit. Try again at {expected_time}.");
            assert_eq!(err.to_string(), expected);
        });
    }

    #[test]
    fn usage_limit_reached_less_than_minute() {
        let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let resets_at = base + ChronoDuration::seconds(30);
        with_now_override(base, move || {
            let expected_time = format_retry_timestamp(&resets_at);
            let err = UsageLimitReachedError {
                plan_type: None,
                resets_at: Some(resets_at),
                rate_limits: Some(rate_limit_snapshot()),
                promo_message: None,
            };
            let expected = format!("You've hit your usage limit. Try again at {expected_time}.");
            assert_eq!(err.to_string(), expected);
        });
    }

    #[test]
    fn usage_limit_reached_with_promo_message() {
        let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let resets_at = base + ChronoDuration::seconds(30);
        with_now_override(base, move || {
            let expected_time = format_retry_timestamp(&resets_at);
            let err = UsageLimitReachedError {
                plan_type: None,
                resets_at: Some(resets_at),
                rate_limits: Some(rate_limit_snapshot()),
                promo_message: Some(
                    "To continue using Savfox, start a free trial of <PLAN> today".to_owned(),
                ),
            };
            let expected = format!(
                "You've hit your usage limit. To continue using Savfox, start a free trial of <PLAN> today, or try again at {expected_time}."
            );
            assert_eq!(err.to_string(), expected);
        });
    }
}
