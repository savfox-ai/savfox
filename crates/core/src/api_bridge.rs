use chrono::{DateTime, Utc};
use http::HeaderMap;
use savfox_api_client::error::ApiError;
use savfox_api_client::rate_limits::{parse_promo_message, parse_rate_limit};
use savfox_api_client::{AuthProvider as ApiAuthProvider, TransportError};
use serde::Deserialize;

use crate::auth::SavfoxAuth;
use crate::error::{
    ModelCapError, RetryLimitReachedError, SavfoxError, UnexpectedResponseError,
    UsageLimitReachedError,
};
use crate::model_provider_info::ModelProviderInfo;
use crate::token_data::PlanType;

pub(crate) fn map_api_error(err: ApiError) -> SavfoxError {
    match err {
        ApiError::ContextWindowExceeded => SavfoxError::ContextWindowExceeded,
        ApiError::QuotaExceeded => SavfoxError::QuotaExceeded,
        ApiError::UsageNotIncluded => SavfoxError::UsageNotIncluded,
        ApiError::Retryable { message, delay } => SavfoxError::Stream(message, delay),
        ApiError::Stream(msg) => SavfoxError::Stream(msg, None),
        ApiError::Api { status, message } => {
            SavfoxError::UnexpectedStatus(UnexpectedResponseError {
                status,
                body: message,
                url: None,
                request_id: None,
            })
        }
        ApiError::InvalidRequest { message } => SavfoxError::InvalidRequest(message),
        ApiError::Transport(transport) => match transport {
            TransportError::Http {
                status,
                url,
                headers,
                body,
            } => {
                let body_text = body.unwrap_or_default();

                if status == http::StatusCode::BAD_REQUEST {
                    if body_text
                        .contains("The image data you provided does not represent a valid image")
                    {
                        SavfoxError::InvalidImageRequest()
                    } else {
                        SavfoxError::InvalidRequest(body_text)
                    }
                } else if status == http::StatusCode::INTERNAL_SERVER_ERROR {
                    SavfoxError::InternalServerError
                } else if status == http::StatusCode::TOO_MANY_REQUESTS {
                    if let Some(model) = headers
                        .as_ref()
                        .and_then(|map| map.get(MODEL_CAP_MODEL_HEADER))
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned)
                    {
                        let reset_after_seconds = headers
                            .as_ref()
                            .and_then(|map| map.get(MODEL_CAP_RESET_AFTER_HEADER))
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<u64>().ok());
                        return SavfoxError::ModelCap(ModelCapError {
                            model,
                            reset_after_seconds,
                        });
                    }

                    if let Ok(err) = serde_json::from_str::<UsageErrorResponse>(&body_text) {
                        if err.error.error_type.as_deref() == Some("usage_limit_reached") {
                            let rate_limits = headers.as_ref().and_then(parse_rate_limit);
                            let promo_message = headers.as_ref().and_then(parse_promo_message);
                            let resets_at = err
                                .error
                                .resets_at
                                .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
                            return SavfoxError::UsageLimitReached(UsageLimitReachedError {
                                plan_type: err.error.plan_type,
                                resets_at,
                                rate_limits,
                                promo_message,
                            });
                        } else if err.error.error_type.as_deref() == Some("usage_not_included") {
                            return SavfoxError::UsageNotIncluded;
                        }
                    }

                    SavfoxError::RetryLimit(RetryLimitReachedError {
                        status,
                        request_id: extract_request_id(headers.as_ref()),
                    })
                } else {
                    SavfoxError::UnexpectedStatus(UnexpectedResponseError {
                        status,
                        body: body_text,
                        url,
                        request_id: extract_request_id(headers.as_ref()),
                    })
                }
            }
            TransportError::RetryLimit => SavfoxError::RetryLimit(RetryLimitReachedError {
                status: http::StatusCode::INTERNAL_SERVER_ERROR,
                request_id: None,
            }),
            TransportError::Timeout => SavfoxError::Timeout,
            TransportError::Network(msg) | TransportError::Build(msg) => {
                SavfoxError::Stream(msg, None)
            }
        },
        ApiError::RateLimit(msg) => SavfoxError::Stream(msg, None),
    }
}

const MODEL_CAP_MODEL_HEADER: &str = "x-savfox-model-cap-model";
const MODEL_CAP_RESET_AFTER_HEADER: &str = "x-savfox-model-cap-reset-after-seconds";

#[cfg(test)]
mod tests {
    use http::{HeaderMap, StatusCode};
    use savfox_api_client::TransportError;

    use super::*;

    #[test]
    fn map_api_error_maps_model_cap_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            MODEL_CAP_MODEL_HEADER,
            http::HeaderValue::from_static("boomslang"),
        );
        headers.insert(
            MODEL_CAP_RESET_AFTER_HEADER,
            http::HeaderValue::from_static("120"),
        );
        let err = map_api_error(ApiError::Transport(TransportError::Http {
            status: StatusCode::TOO_MANY_REQUESTS,
            url: Some("http://example.com/v1/responses".to_owned()),
            headers: Some(headers),
            body: Some(String::new()),
        }));

        let SavfoxError::ModelCap(model_cap) = err else {
            panic!("expected SavfoxError::ModelCap, got {err:?}");
        };
        assert_eq!(model_cap.model, "boomslang");
        assert_eq!(model_cap.reset_after_seconds, Some(120));
    }
}

fn extract_request_id(headers: Option<&HeaderMap>) -> Option<String> {
    headers.and_then(|map| {
        ["cf-ray", "x-request-id", "x-oai-request-id"]
            .iter()
            .find_map(|name| {
                map.get(*name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            })
    })
}

pub(crate) fn auth_provider_from_auth(
    auth: Option<SavfoxAuth>,
    provider: &ModelProviderInfo,
    provider_id: &str,
) -> crate::error::Result<CoreAuthProvider> {
    if let Some(api_key) = provider.api_key()? {
        return Ok(CoreAuthProvider {
            token: Some(api_key),
            account_id: None,
        });
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(CoreAuthProvider {
            token: Some(token),
            account_id: None,
        });
    }

    if let Some(auth) = auth {
        let token = auth.get_token()?;
        return Ok(CoreAuthProvider {
            token: Some(token),
            account_id: auth.get_account_id(),
        });
    }

    // Last resort: check the runtime bearer-token override map.  The gateway
    // injects tokens here when a provider is imported so the engine can
    // authenticate immediately without a restart, even for providers whose
    // built-in `ModelProviderInfo` has `env_key: None`.
    if let Some(token) = crate::model_provider_info::get_bearer_token_override(provider_id) {
        return Ok(CoreAuthProvider {
            token: Some(token),
            account_id: None,
        });
    }

    Ok(CoreAuthProvider {
        token: None,
        account_id: None,
    })
}

#[derive(Debug, Deserialize)]
struct UsageErrorResponse {
    error: UsageErrorBody,
}

#[derive(Debug, Deserialize)]
struct UsageErrorBody {
    #[serde(rename = "type")]
    error_type: Option<String>,
    plan_type: Option<PlanType>,
    resets_at: Option<i64>,
}

#[derive(Clone, Default)]
pub(crate) struct CoreAuthProvider {
    token: Option<String>,
    account_id: Option<String>,
}

impl CoreAuthProvider {
    pub(crate) fn has_bearer_token(&self) -> bool {
        self.token
            .as_ref()
            .is_some_and(|token| !token.trim().is_empty())
    }

    pub(crate) fn has_account_id(&self) -> bool {
        self.account_id
            .as_ref()
            .is_some_and(|account_id| !account_id.trim().is_empty())
    }
}

impl ApiAuthProvider for CoreAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        self.token.clone()
    }

    fn account_id(&self) -> Option<String> {
        self.account_id.clone()
    }
}
