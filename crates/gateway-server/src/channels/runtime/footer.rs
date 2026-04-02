use std::sync::{OnceLock, RwLock};

use savfox_protocol::protocol::TokenUsage;

use crate::config::ResponseFooterConfig;

fn footer_config_store() -> &'static RwLock<ResponseFooterConfig> {
    static STORE: OnceLock<RwLock<ResponseFooterConfig>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(ResponseFooterConfig::default()))
}

pub(crate) fn set_response_footer_config(config: ResponseFooterConfig) {
    if let Ok(mut lock) = footer_config_store().write() {
        *lock = config;
    }
}

pub(super) fn current_response_footer_config() -> ResponseFooterConfig {
    footer_config_store()
        .read()
        .map(|cfg| cfg.clone())
        .unwrap_or_else(|_| ResponseFooterConfig::default())
}

fn truncate_chars(text: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_len {
        return text.to_owned();
    }
    if max_len <= 3 {
        return ".".repeat(max_len);
    }
    let mut out = String::new();
    for ch in text.chars().take(max_len - 3) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn render_footer_template(
    template: &str,
    model: &str,
    provider: &str,
    profile: Option<&str>,
    usage: Option<&TokenUsage>,
) -> String {
    let profile_value = profile.unwrap_or("").trim().to_owned();
    let token_value = usage.map(|u| u.total_tokens.max(0));
    let cost_value = token_value.map(|v| v as f64 * 0.00001);

    let profile_segment = if profile_value.is_empty() {
        String::new()
    } else {
        format!(" | profile: {profile_value}")
    };
    let tokens_segment = token_value
        .map(|v| format!(" | tokens: {v}"))
        .unwrap_or_default();
    let cost_segment = cost_value
        .map(|v| format!(" | est. cost: ${v:.4}"))
        .unwrap_or_default();
    let tokens_text = token_value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "n/a".to_owned());
    let cost_text = cost_value
        .map(|v| format!("{v:.4}"))
        .unwrap_or_else(|| "n/a".to_owned());

    template
        .replace("{model}", model)
        .replace("{provider}", provider)
        .replace("{profile}", &profile_value)
        .replace("{tokens}", &tokens_text)
        .replace("{cost}", &cost_text)
        .replace("{profile_segment}", &profile_segment)
        .replace("{tokens_segment}", &tokens_segment)
        .replace("{cost_segment}", &cost_segment)
}

pub(super) fn format_model_footer(
    config: &ResponseFooterConfig,
    platform: &str,
    model: &str,
    provider: &str,
    profile: Option<&str>,
    usage: Option<&TokenUsage>,
) -> Option<String> {
    if !config.enabled {
        return None;
    }
    let template = config
        .channel_templates
        .get(platform)
        .map(String::as_str)
        .unwrap_or(config.template.as_str());
    let rendered = render_footer_template(template, model, provider, profile, usage);
    let rendered = rendered.trim();
    if rendered.is_empty() {
        return None;
    }
    let max_len = config
        .channel_max_length
        .get(platform)
        .copied()
        .unwrap_or(config.max_length);
    let footer = truncate_chars(rendered, max_len);
    if footer.trim().is_empty() {
        None
    } else {
        Some(footer)
    }
}

pub(super) fn append_footer(reply: &str, footer: &str) -> String {
    let reply = reply.trim_end();
    if reply.is_empty() {
        footer.to_owned()
    } else {
        format!("{reply}\n\n---\n{footer}")
    }
}
