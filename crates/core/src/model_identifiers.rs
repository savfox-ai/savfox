//! Helpers for normalizing model identifiers shared by core and gateway code.
//!
//! We support two identifier forms:
//! - bare model code: `glm-5`
//! - provider-prefixed model id: `zhipuai-coding-plan/glm-5`
//!
//! For provider-scoped request payloads, only strip the prefix when it matches
//! the configured provider id. This avoids breaking providers whose native
//! model codes contain `/` (for example `anthropic/claude-*` on OpenRouter).

/// Parse a provider-prefixed model identifier.
///
/// Returns `(provider_id, model_code)` when `identifier` contains at least one
/// `/` and both parts are non-empty after trimming.
///
/// Parsing uses the **last** `/` so provider ids may themselves contain `/`.
pub fn parse_provider_prefixed_model(identifier: &str) -> Option<(&str, &str)> {
    let trimmed = identifier.trim();
    let (provider_id, model_code) = trimmed.rsplit_once('/')?;
    let provider_id = provider_id.trim();
    let model_code = model_code.trim();
    if provider_id.is_empty() || model_code.is_empty() {
        return None;
    }
    Some((provider_id, model_code))
}

/// Normalize a model identifier for a provider request payload.
///
/// If `model` is in `provider/model_code` form and `provider` matches
/// `provider_id` (case-insensitive), returns `model_code`.
/// Otherwise returns the original trimmed `model`.
pub fn request_model_for_provider(model: &str, provider_id: &str) -> String {
    let provider_id = provider_id.trim();
    let model = model.trim();
    if let Some((provider, model_code)) = parse_provider_prefixed_model(model)
        && provider.eq_ignore_ascii_case(provider_id)
    {
        return model_code.to_string();
    }
    model.to_string()
}

#[cfg(test)]
mod tests {
    use super::{parse_provider_prefixed_model, request_model_for_provider};

    #[test]
    fn parse_provider_prefixed_model_handles_valid_values() {
        assert_eq!(
            parse_provider_prefixed_model("zhipuai-coding-plan/glm-5"),
            Some(("zhipuai-coding-plan", "glm-5"))
        );
        assert_eq!(
            parse_provider_prefixed_model("team/provider/glm-5"),
            Some(("team/provider", "glm-5"))
        );
    }

    #[test]
    fn parse_provider_prefixed_model_rejects_invalid_values() {
        assert_eq!(parse_provider_prefixed_model(""), None);
        assert_eq!(parse_provider_prefixed_model("glm-5"), None);
        assert_eq!(parse_provider_prefixed_model("provider/"), None);
        assert_eq!(parse_provider_prefixed_model("/model"), None);
    }

    #[test]
    fn request_model_for_provider_strips_matching_provider_prefix() {
        assert_eq!(
            request_model_for_provider("zhipuai-coding-plan/glm-5", "zhipuai-coding-plan"),
            "glm-5"
        );
        assert_eq!(
            request_model_for_provider(" ZHIPUAI-CODING-PLAN/glm-5 ", "zhipuai-coding-plan"),
            "glm-5"
        );
        assert_eq!(
            request_model_for_provider("acme/team/glm-5", "acme/team"),
            "glm-5"
        );
    }

    #[test]
    fn request_model_for_provider_keeps_mismatched_provider_prefix() {
        assert_eq!(
            request_model_for_provider("anthropic/claude-3.7-sonnet", "openrouter"),
            "anthropic/claude-3.7-sonnet"
        );
    }
}
