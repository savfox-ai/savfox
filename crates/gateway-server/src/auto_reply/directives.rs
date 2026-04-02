//! Inline directive parser - extracts `@directive value` from message text.
//!
//! Supported directives:
//! - `@thinking <level>` - off, minimal, low, medium, high, xhigh
//! - `@model <name>` - switch model for this message
//! - `@model <name>@<profile>` - model plus provider profile hint
//! - `@verbose <on|off>` - toggle verbose output
//! - `@elevated <on|off>` - toggle elevated/sudo mode
//! - `@reasoning <off|brief|verbose>` - control reasoning output

use serde::{Deserialize, Serialize};

/// A parsed inline directive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Directive {
    pub kind: DirectiveKind,
    pub value: String,
}

/// Known directive types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveKind {
    Thinking,
    Model,
    Verbose,
    Elevated,
    Reasoning,
    Status,
}

/// Result of directive parsing: extracted directives + cleaned message text.
#[derive(Debug, Clone)]
pub struct DirectiveParseResult {
    pub directives: Vec<Directive>,
    pub cleaned_text: String,
}

/// Parsed model target from `/model`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDirectiveTarget {
    pub model: String,
    pub profile: Option<String>,
}

/// Parse inline directives from a message body and return cleaned text.
///
/// Supports both `@directive value` and `/directive value` prefixes.
/// Directives are case-insensitive and are parsed line-by-line.
///
/// If a directive line contains remaining message content after the directive
/// value, that remainder is preserved in `cleaned_text`.
pub fn parse_directives(text: &str) -> DirectiveParseResult {
    let mut directives = Vec::new();
    let mut cleaned_lines = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some((d, remainder)) = try_parse_directive(trimmed) {
            directives.push(d);
            if !remainder.is_empty() {
                cleaned_lines.push(remainder);
            }
        } else {
            cleaned_lines.push(line.to_owned());
        }
    }

    DirectiveParseResult {
        directives,
        cleaned_text: cleaned_lines
            .join("\n")
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned(),
    }
}

/// Parse `/model foo@prod` into model + optional profile.
pub fn parse_model_target(value: &str) -> ModelDirectiveTarget {
    let raw = value.trim();
    let Some((model, profile)) = raw.rsplit_once('@') else {
        return ModelDirectiveTarget {
            model: raw.to_owned(),
            profile: None,
        };
    };
    let model = model.trim();
    let profile = profile.trim();
    if model.is_empty() || profile.is_empty() {
        return ModelDirectiveTarget {
            model: raw.to_owned(),
            profile: None,
        };
    }
    ModelDirectiveTarget {
        model: model.to_owned(),
        profile: Some(profile.to_owned()),
    }
}

/// Resolve a user-provided model hint against available model IDs.
///
/// Matching order:
/// 1. Case-insensitive exact ID.
/// 2. Exact normalized token match (ignores separators).
/// 3. Suffix match on ID tail after provider (`provider/model`).
/// 4. Contains match on normalized ID (shortest candidate wins).
pub fn fuzzy_match_model_name(hint: &str, candidates: &[String]) -> Option<String> {
    let hint = hint.trim();
    if hint.is_empty() {
        return None;
    }
    let hint_lower = hint.to_lowercase();
    let hint_norm = normalize_token(hint);

    for candidate in candidates {
        if candidate.eq_ignore_ascii_case(hint) {
            return Some(candidate.clone());
        }
    }

    for candidate in candidates {
        if normalize_token(candidate) == hint_norm {
            return Some(candidate.clone());
        }
    }

    for candidate in candidates {
        if let Some((_, tail)) = candidate.split_once('/')
            && (tail.eq_ignore_ascii_case(hint) || normalize_token(tail) == hint_norm)
        {
            return Some(candidate.clone());
        }
    }

    let mut contains_matches: Vec<&String> = candidates
        .iter()
        .filter(|candidate| normalize_token(candidate).contains(&hint_norm))
        .collect();
    contains_matches.sort_by_key(|candidate| candidate.len());
    if let Some(best) = contains_matches.first() {
        return Some((*best).clone());
    }

    for candidate in candidates {
        if candidate.to_lowercase().contains(&hint_lower) {
            return Some(candidate.clone());
        }
    }

    None
}

fn try_parse_directive(line: &str) -> Option<(Directive, String)> {
    let (name, rest) = if let Some(stripped) = line.strip_prefix('@') {
        parse_name_rest(stripped)
    } else if let Some(stripped) = line.strip_prefix('/') {
        parse_name_rest(stripped)
    } else {
        return None;
    };

    let name_lower = name.to_lowercase();
    let (value_token, remainder) = split_first_token(rest);

    match name_lower.as_str() {
        "thinking" | "think" | "t" => Some((
            Directive {
                kind: DirectiveKind::Thinking,
                value: normalize_thinking_level(value_token),
            },
            remainder.to_owned(),
        )),
        "model" | "m" => {
            if value_token.is_empty() {
                None
            } else {
                Some((
                    Directive {
                        kind: DirectiveKind::Model,
                        value: value_token.to_owned(),
                    },
                    remainder.to_owned(),
                ))
            }
        }
        "verbose" | "v" => Some((
            Directive {
                kind: DirectiveKind::Verbose,
                value: if value_token.is_empty() {
                    "on".to_owned()
                } else {
                    value_token.to_owned()
                },
            },
            remainder.to_owned(),
        )),
        "elevated" | "elev" | "sudo" => Some((
            Directive {
                kind: DirectiveKind::Elevated,
                value: if value_token.is_empty() {
                    "on".to_owned()
                } else {
                    value_token.to_owned()
                },
            },
            remainder.to_owned(),
        )),
        "reasoning" | "reason" => Some((
            Directive {
                kind: DirectiveKind::Reasoning,
                value: if value_token.is_empty() {
                    "verbose".to_owned()
                } else {
                    value_token.to_owned()
                },
            },
            remainder.to_owned(),
        )),
        "status" => Some((
            Directive {
                kind: DirectiveKind::Status,
                value: String::new(),
            },
            rest.trim().to_owned(),
        )),
        _ => None,
    }
}

fn parse_name_rest(s: &str) -> (&str, &str) {
    // Handle optional colon separator: `@thinking:high` / `@thinking : high`
    if let Some((name, rest)) = s.split_once(|c: char| c.is_whitespace() || c == ':') {
        let rest = rest.trim_start_matches(':').trim();
        (name, rest)
    } else {
        (s, "")
    }
}

fn split_first_token(s: &str) -> (&str, &str) {
    let s = s.trim();
    if s.is_empty() {
        return ("", "");
    }
    if let Some(idx) = s.find(char::is_whitespace) {
        let (first, rest) = s.split_at(idx);
        (first, rest.trim())
    } else {
        (s, "")
    }
}

fn normalize_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_thinking_level(value: &str) -> String {
    match value.to_lowercase().as_str() {
        "off" | "none" | "0" => "off",
        "minimal" | "min" | "1" => "minimal",
        "low" | "2" => "low",
        "medium" | "med" | "3" => "medium",
        "high" | "4" => "high",
        "xhigh" | "max" | "5" => "xhigh",
        "" => "medium", // default when no value given
        other => return other.to_owned(),
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{DirectiveKind, fuzzy_match_model_name, parse_directives, parse_model_target};

    #[test]
    fn parse_model_directive_with_colon_and_profile() {
        let parsed = parse_directives("/model: gpt-4o@production\nExplain this diff");
        assert_eq!(parsed.directives.len(), 1);
        assert_eq!(parsed.directives[0].kind, DirectiveKind::Model);
        assert_eq!(parsed.directives[0].value, "gpt-4o@production");
        assert_eq!(parsed.cleaned_text, "Explain this diff");

        let target = parse_model_target(&parsed.directives[0].value);
        assert_eq!(target.model, "gpt-4o");
        assert_eq!(target.profile.as_deref(), Some("production"));
    }

    #[test]
    fn parse_directive_preserves_inline_remainder() {
        let parsed = parse_directives("/model gpt-4o summarize release notes");
        assert_eq!(parsed.directives.len(), 1);
        assert_eq!(parsed.directives[0].kind, DirectiveKind::Model);
        assert_eq!(parsed.cleaned_text, "summarize release notes");
    }

    #[test]
    fn fuzzy_match_model_names() {
        let candidates = vec![
            "openai/gpt-4o".to_owned(),
            "openai/gpt-4o-mini".to_owned(),
            "anthropic/claude-sonnet-4-20250514".to_owned(),
        ];

        assert_eq!(
            fuzzy_match_model_name("gpt4o", &candidates).as_deref(),
            Some("openai/gpt-4o")
        );
        assert_eq!(
            fuzzy_match_model_name("claude-sonnet-4", &candidates).as_deref(),
            Some("anthropic/claude-sonnet-4-20250514")
        );
        assert!(fuzzy_match_model_name("unknown-model", &candidates).is_none());
    }
}
