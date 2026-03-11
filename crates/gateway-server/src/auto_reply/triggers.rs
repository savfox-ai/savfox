use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Defines when an auto-reply rule should fire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum Trigger {
    /// Fire when the bot is @mentioned.
    Mention,
    /// Fire when message contains any of the keywords (case-insensitive).
    Keywords { keywords: Vec<String> },
    /// Fire when message matches a regex pattern.
    Regex { pattern: String },
    /// Fire on all messages in a channel.
    Always,
    /// Fire when message starts with a prefix (e.g. "!" or "/").
    Prefix { prefix: String },
}

/// A complete auto-reply rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AutoReplyRule {
    /// Unique rule identifier.
    pub(crate) id: String,
    /// Human-readable name.
    pub(crate) name: String,
    /// Whether this rule is active.
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
    /// Trigger conditions (any match fires the rule).
    pub(crate) triggers: Vec<Trigger>,
    /// Extra rule conditions, all of which must match.
    #[serde(default)]
    pub(crate) conditions: Vec<RuleCondition>,
    /// Channel restrictions (empty = all channels).
    #[serde(default)]
    pub(crate) channels: Vec<String>,
    /// Who can trigger this rule.
    #[serde(default)]
    pub(crate) permissions: ReplyPermissions,
    /// Response template.
    pub(crate) response: ResponseTemplate,
    /// Priority (lower = higher priority). Default: 100.
    #[serde(default = "default_priority")]
    pub(crate) priority: u32,
}

fn default_true() -> bool {
    true
}

fn default_priority() -> u32 {
    100
}

/// Permission gating for who can trigger auto-replies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ReplyPermissions {
    /// Allowed user IDs (empty = all).
    #[serde(default)]
    pub(crate) allow_users: Vec<String>,
    /// Denied user IDs.
    #[serde(default)]
    pub(crate) deny_users: Vec<String>,
    /// Require specific role/scope.
    #[serde(default)]
    pub(crate) require_scope: Option<String>,
}

/// Template for auto-reply responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum ResponseTemplate {
    /// Static text response (with variable substitution).
    Text { template: String },
    /// Forward to agent for AI-powered response.
    Agent {
        model: Option<String>,
        system_prompt: Option<String>,
    },
    /// Redirect/proxy to another channel or webhook.
    Forward { target_channel: String },
}

/// Additional rule conditions chained with logical AND.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum RuleCondition {
    /// Require an exact channel match (case-insensitive).
    ChannelEquals { value: String },
    /// Require user to be in one of the listed groups.
    UserInGroup { groups: Vec<String> },
    /// Time window condition (supports overnight ranges and optional weekdays).
    TimeWindow {
        /// Start time in HH:MM.
        start: String,
        /// End time in HH:MM.
        end: String,
        /// Optional weekdays, e.g. ["sat", "sun"].
        #[serde(default)]
        weekdays: Vec<String>,
    },
}

/// Incoming message context for trigger evaluation.
#[derive(Debug)]
pub(crate) struct MessageContext<'a> {
    pub(crate) text: &'a str,
    pub(crate) channel: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) is_mention: bool,
    pub(crate) model: Option<&'a str>,
    pub(crate) user_groups: &'a [String],
    pub(crate) now: DateTime<Utc>,
}

impl<'a> MessageContext<'a> {
    pub(crate) fn new(text: &'a str, channel: &'a str, user_id: &'a str) -> Self {
        Self {
            text,
            channel,
            user_id,
            is_mention: false,
            model: None,
            user_groups: &[],
            now: Utc::now(),
        }
    }
}

impl Trigger {
    /// Check if this trigger matches the given message context.
    pub(crate) fn matches(&self, ctx: &MessageContext<'_>) -> bool {
        match self {
            Trigger::Mention => ctx.is_mention,
            Trigger::Keywords { keywords } => {
                let lower = ctx.text.to_lowercase();
                keywords.iter().any(|k| lower.contains(&k.to_lowercase()))
            }
            Trigger::Regex { pattern } => Regex::new(pattern)
                .map(|re| re.is_match(ctx.text))
                .unwrap_or(false),
            Trigger::Always => true,
            Trigger::Prefix { prefix } => ctx.text.starts_with(prefix),
        }
    }
}

impl RuleCondition {
    fn matches(&self, ctx: &MessageContext<'_>) -> bool {
        match self {
            Self::ChannelEquals { value } => ctx.channel.eq_ignore_ascii_case(value.trim()),
            Self::UserInGroup { groups } => {
                if groups.is_empty() {
                    return false;
                }
                ctx.user_groups.iter().any(|actual| {
                    groups
                        .iter()
                        .any(|expected| actual.eq_ignore_ascii_case(expected))
                })
            }
            Self::TimeWindow {
                start,
                end,
                weekdays,
            } => {
                let Some(start_time) = parse_hhmm(start) else {
                    return false;
                };
                let Some(end_time) = parse_hhmm(end) else {
                    return false;
                };
                if !weekday_matches(ctx.now.weekday(), weekdays) {
                    return false;
                }
                time_in_window(ctx.now.time(), start_time, end_time)
            }
        }
    }
}

impl AutoReplyRule {
    /// Check if this rule should fire for the given context.
    pub(crate) fn should_fire(&self, ctx: &MessageContext<'_>) -> bool {
        if !self.enabled {
            return false;
        }
        // Channel restriction
        if !self.channels.is_empty() && !self.channels.contains(&ctx.channel.to_owned()) {
            return false;
        }
        // Permission check
        if !self.permissions.deny_users.is_empty()
            && self
                .permissions
                .deny_users
                .contains(&ctx.user_id.to_owned())
        {
            return false;
        }
        if !self.permissions.allow_users.is_empty()
            && !self
                .permissions
                .allow_users
                .contains(&ctx.user_id.to_owned())
        {
            return false;
        }
        // Chained conditions (logical AND).
        if !self.conditions.iter().all(|cond| cond.matches(ctx)) {
            return false;
        }
        // Any trigger matches
        self.triggers.iter().any(|t| t.matches(ctx))
    }
}

/// Evaluate all rules and return the first matching rule (by priority).
pub(crate) fn evaluate_rules<'a>(
    rules: &'a [AutoReplyRule],
    ctx: &MessageContext<'_>,
) -> Option<&'a AutoReplyRule> {
    let mut matching: Vec<&AutoReplyRule> = rules.iter().filter(|r| r.should_fire(ctx)).collect();
    matching.sort_by_key(|r| r.priority);
    matching.first().copied()
}

/// Apply variable substitution to a template string.
/// Supported variables: {{user}}, {{channel}}, {{message}}, {{timestamp}}, {{time}}, {{model}}
pub(crate) fn render_template(template: &str, ctx: &MessageContext<'_>) -> String {
    let timestamp = ctx.now.to_rfc3339();
    let time = ctx.now.format("%H:%M:%S").to_string();
    let model = ctx.model.unwrap_or("default");
    template
        .replace("{{user}}", ctx.user_id)
        .replace("{{channel}}", ctx.channel)
        .replace("{{message}}", ctx.text)
        .replace("{{timestamp}}", &timestamp)
        .replace("{{time}}", &time)
        .replace("{{model}}", model)
}

fn parse_hhmm(raw: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(raw.trim(), "%H:%M").ok()
}

fn time_in_window(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start == end {
        return true;
    }
    if start < end {
        now >= start && now < end
    } else {
        // Overnight window, e.g. 22:00-06:00
        now >= start || now < end
    }
}

fn weekday_matches(now: Weekday, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    let parsed: Vec<Weekday> = filters.iter().filter_map(|f| parse_weekday(f)).collect();
    if parsed.is_empty() {
        return false;
    }
    parsed.contains(&now)
}

fn parse_weekday(raw: &str) -> Option<Weekday> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "mon" | "monday" => Some(Weekday::Mon),
        "2" | "tue" | "tues" | "tuesday" => Some(Weekday::Tue),
        "3" | "wed" | "wednesday" => Some(Weekday::Wed),
        "4" | "thu" | "thur" | "thurs" | "thursday" => Some(Weekday::Thu),
        "5" | "fri" | "friday" => Some(Weekday::Fri),
        "6" | "sat" | "saturday" => Some(Weekday::Sat),
        "0" | "7" | "sun" | "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{
        AutoReplyRule, MessageContext, ReplyPermissions, ResponseTemplate, RuleCondition, Trigger,
        evaluate_rules, render_template,
    };

    fn rule(
        id: &str,
        priority: u32,
        triggers: Vec<Trigger>,
        conditions: Vec<RuleCondition>,
    ) -> AutoReplyRule {
        AutoReplyRule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            triggers,
            conditions,
            channels: Vec::new(),
            permissions: ReplyPermissions::default(),
            response: ResponseTemplate::Text {
                template: "ok".to_string(),
            },
            priority,
        }
    }

    #[test]
    fn condition_chaining_channel_and_group() {
        let groups = vec!["admin".to_string(), "ops".to_string()];
        let ctx = MessageContext {
            text: "hello",
            channel: "discord:general",
            user_id: "u1",
            is_mention: false,
            model: Some("openai/gpt-4o"),
            user_groups: &groups,
            now: Utc
                .with_ymd_and_hms(2026, 2, 15, 10, 0, 0)
                .single()
                .unwrap(),
        };

        let r = rule(
            "admin-rule",
            10,
            vec![Trigger::Always],
            vec![
                RuleCondition::ChannelEquals {
                    value: "discord:general".to_string(),
                },
                RuleCondition::UserInGroup {
                    groups: vec!["admin".to_string()],
                },
            ],
        );
        assert!(r.should_fire(&ctx));
    }

    #[test]
    fn time_window_supports_night_and_weekend() {
        let groups = Vec::new();
        let weekend_night = MessageContext {
            text: "night",
            channel: "discord:ops",
            user_id: "u2",
            is_mention: false,
            model: None,
            user_groups: &groups,
            now: Utc
                .with_ymd_and_hms(2026, 2, 14, 23, 15, 0)
                .single()
                .unwrap(), // Saturday
        };
        let weekday_night = MessageContext {
            text: "night",
            channel: "discord:ops",
            user_id: "u3",
            is_mention: false,
            model: None,
            user_groups: &groups,
            now: Utc
                .with_ymd_and_hms(2026, 2, 11, 23, 15, 0)
                .single()
                .unwrap(), // Wednesday
        };
        let r = rule(
            "weekend-night",
            20,
            vec![Trigger::Always],
            vec![RuleCondition::TimeWindow {
                start: "22:00".to_string(),
                end: "06:00".to_string(),
                weekdays: vec!["sat".to_string(), "sun".to_string()],
            }],
        );

        assert!(r.should_fire(&weekend_night));
        assert!(!r.should_fire(&weekday_night));
    }

    #[test]
    fn evaluate_rules_picks_highest_priority_match() {
        let ctx = MessageContext::new("hi", "discord:general", "u1");
        let slow = rule("slow", 100, vec![Trigger::Always], vec![]);
        let fast = rule("fast", 1, vec![Trigger::Always], vec![]);
        let rules = vec![slow, fast];
        let picked = evaluate_rules(&rules, &ctx).expect("rule should match");
        assert_eq!(picked.id, "fast");
    }

    #[test]
    fn render_template_supports_time_and_model() {
        let groups = Vec::new();
        let ctx = MessageContext {
            text: "hello",
            channel: "telegram:123",
            user_id: "alice",
            is_mention: false,
            model: Some("anthropic/claude-sonnet"),
            user_groups: &groups,
            now: Utc
                .with_ymd_and_hms(2026, 2, 15, 7, 30, 45)
                .single()
                .unwrap(),
        };
        let out = render_template("u={{user}} c={{channel}} t={{time}} m={{model}}", &ctx);
        assert_eq!(
            out,
            "u=alice c=telegram:123 t=07:30:45 m=anthropic/claude-sonnet"
        );
    }

    #[test]
    fn invalid_time_window_does_not_match() {
        let ctx = MessageContext::new("hello", "discord:general", "u1");
        let r = rule(
            "invalid-time",
            50,
            vec![Trigger::Always],
            vec![RuleCondition::TimeWindow {
                start: "99:00".to_string(),
                end: "06:00".to_string(),
                weekdays: Vec::new(),
            }],
        );
        assert!(!r.should_fire(&ctx));
    }
}
