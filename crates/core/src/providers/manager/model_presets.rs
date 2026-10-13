//! Legacy notice keys kept for config compatibility with older migration prompts.
//!
//! The bundled model catalog is gone: model listings are derived entirely from
//! the active catalog (remote `/models`, the provider store, or `config.toml`).
//! Savfox talks to a dozen providers, so shipping one vendor's model list as a
//! built-in baseline put the wrong entries in front of everyone else and handed
//! third-party models a GPT-shaped feature set.

pub const HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG: &str = "hide_gpt5_1_migration_prompt";
pub const HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG: &str =
    "hide_gpt-5.1-codex-max_migration_prompt";
