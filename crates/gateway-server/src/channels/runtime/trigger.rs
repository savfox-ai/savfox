use crate::auto_reply::GroupActivation;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SenderKind {
    #[default]
    Human,
    SelfBot,
    OwnAgentGhost,
    ExternalBot,
    BridgeGhost,
    Unknown,
}

impl SenderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::SelfBot => "self_bot",
            Self::OwnAgentGhost => "own_agent_ghost",
            Self::ExternalBot => "external_bot",
            Self::BridgeGhost => "bridge_ghost",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Direct,
    PairRoom,
    Group,
    Broadcast,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerReason {
    DirectMessage,
    PairRoom,
    MentionedMe,
    MentionedOtherAgent,
    ReplyToMyMessage,
    Command,
    KeywordMatched,
    RoomDefaultAgent,
    ExternalBotIgnored,
    ExternalBotIngestOnly,
    SelfMessageIgnored,
    NoTrigger,
}

impl TriggerReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DirectMessage => "direct_message",
            Self::PairRoom => "pair_room",
            Self::MentionedMe => "mentioned_me",
            Self::MentionedOtherAgent => "mentioned_other_agent",
            Self::ReplyToMyMessage => "reply_to_my_message",
            Self::Command => "command",
            Self::KeywordMatched => "keyword_matched",
            Self::RoomDefaultAgent => "room_default_agent",
            Self::ExternalBotIgnored => "external_bot_ignored",
            Self::ExternalBotIngestOnly => "external_bot_ingest_only",
            Self::SelfMessageIgnored => "self_message_ignored",
            Self::NoTrigger => "no_trigger",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerDecision {
    Ignore { reason: TriggerReason },
    IngestOnly { reason: TriggerReason },
    Reply { reason: TriggerReason },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IngestPolicy {
    #[default]
    PreserveBase,
    None,
    ReplyOnly,
    TargetedOnly,
    AllHumanMessages,
    AllNonBotMessages,
    AllMessages,
}

impl IngestPolicy {
    pub fn from_str(value: Option<&str>) -> Self {
        match value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            Some("none") => Self::None,
            Some("reply_only") => Self::ReplyOnly,
            Some("targeted_only") => Self::TargetedOnly,
            Some("all_human_messages") => Self::AllHumanMessages,
            Some("all_non_bot_messages") => Self::AllNonBotMessages,
            Some("all_messages") => Self::AllMessages,
            _ => Self::PreserveBase,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExternalBotPolicy {
    #[default]
    Ignore,
    IngestOnly,
    ReplyAllowed,
}

impl ExternalBotPolicy {
    pub fn from_str(value: Option<&str>) -> Self {
        match value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            Some("ingest_only") => Self::IngestOnly,
            Some("reply_allowed") => Self::ReplyAllowed,
            _ => Self::Ignore,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleReplyConfig {
    pub enabled: bool,
    pub delay_secs: u64,
    pub prompt: Option<String>,
}

impl Default for IdleReplyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            delay_secs: 180,
            prompt: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTriggerConfig {
    pub group_activation: GroupActivation,
    pub group_keywords: Vec<String>,
    pub ingest_policy: IngestPolicy,
    pub external_bot_policy: ExternalBotPolicy,
    pub agent_aliases: Vec<String>,
    pub idle_reply: IdleReplyConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerContext<'a> {
    pub sender_kind: SenderKind,
    pub conversation_kind: ConversationKind,
    pub is_mentioned: bool,
    pub reply_to_self: bool,
    pub is_command: bool,
    pub explicitly_targets_other_agent: bool,
    pub text: &'a str,
}

pub fn effective_conversation_kind(
    chat_type: Option<&str>,
    participant_count: Option<u32>,
) -> ConversationKind {
    if participant_count == Some(2) {
        return ConversationKind::PairRoom;
    }

    match chat_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
    {
        Some(value) if matches!(value.as_str(), "dm" | "private" | "direct" | "single") => {
            ConversationKind::Direct
        }
        Some(value) if matches!(value.as_str(), "channel" | "broadcast") => {
            ConversationKind::Broadcast
        }
        Some(value) if matches!(value.as_str(), "group" | "supergroup" | "chat") => {
            ConversationKind::Group
        }
        Some(_) => ConversationKind::Unknown,
        None => ConversationKind::Unknown,
    }
}

pub fn decide_trigger(
    sender_kind: SenderKind,
    chat_type: Option<&str>,
    participant_count: Option<u32>,
    is_mentioned: bool,
    reply_to_self: bool,
    is_command: bool,
    used_plain_text_fallback: bool,
    explicitly_targets_other_agent: bool,
) -> TriggerDecision {
    match sender_kind {
        SenderKind::SelfBot | SenderKind::OwnAgentGhost | SenderKind::BridgeGhost => {
            return TriggerDecision::Ignore {
                reason: TriggerReason::SelfMessageIgnored,
            };
        }
        SenderKind::ExternalBot => {
            return TriggerDecision::Ignore {
                reason: TriggerReason::ExternalBotIgnored,
            };
        }
        SenderKind::Human | SenderKind::Unknown => {}
    }

    if explicitly_targets_other_agent && !is_mentioned && !is_command {
        return TriggerDecision::IngestOnly {
            reason: TriggerReason::MentionedOtherAgent,
        };
    }

    if reply_to_self {
        return TriggerDecision::Reply {
            reason: TriggerReason::ReplyToMyMessage,
        };
    }

    if is_command {
        return TriggerDecision::Reply {
            reason: TriggerReason::Command,
        };
    }

    if is_mentioned {
        return TriggerDecision::Reply {
            reason: TriggerReason::MentionedMe,
        };
    }

    match effective_conversation_kind(chat_type, participant_count) {
        ConversationKind::Direct => TriggerDecision::Reply {
            reason: TriggerReason::DirectMessage,
        },
        ConversationKind::PairRoom => TriggerDecision::Reply {
            reason: TriggerReason::PairRoom,
        },
        ConversationKind::Group | ConversationKind::Broadcast | ConversationKind::Unknown => {
            if used_plain_text_fallback {
                TriggerDecision::IngestOnly {
                    reason: TriggerReason::RoomDefaultAgent,
                }
            } else {
                TriggerDecision::Reply {
                    reason: TriggerReason::RoomDefaultAgent,
                }
            }
        }
    }
}

fn has_group_keyword(text: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    keywords.iter().any(|keyword| {
        let needle = keyword.trim().to_ascii_lowercase();
        !needle.is_empty() && lower.contains(&needle)
    })
}

pub fn apply_agent_trigger_policy(
    base_decision: TriggerDecision,
    ctx: TriggerContext<'_>,
    config: &AgentTriggerConfig,
) -> TriggerDecision {
    let TriggerContext {
        sender_kind,
        conversation_kind,
        is_mentioned,
        reply_to_self,
        is_command,
        explicitly_targets_other_agent,
        text,
    } = ctx;

    let mut decision = base_decision;

    if matches!(
        conversation_kind,
        ConversationKind::Group | ConversationKind::Broadcast | ConversationKind::Unknown
    ) && !explicitly_targets_other_agent
        && !reply_to_self
    {
        decision = match config.group_activation {
            GroupActivation::Always => match decision {
                TriggerDecision::IngestOnly {
                    reason: TriggerReason::RoomDefaultAgent,
                } => TriggerDecision::Reply {
                    reason: TriggerReason::RoomDefaultAgent,
                },
                other => other,
            },
            GroupActivation::Off => match decision {
                TriggerDecision::Reply { reason } | TriggerDecision::IngestOnly { reason } => {
                    TriggerDecision::IngestOnly { reason }
                }
                other => other,
            },
            GroupActivation::Command => {
                if is_command {
                    TriggerDecision::Reply {
                        reason: TriggerReason::Command,
                    }
                } else {
                    match decision {
                        TriggerDecision::Reply { reason }
                        | TriggerDecision::IngestOnly { reason } => {
                            TriggerDecision::IngestOnly { reason }
                        }
                        other => other,
                    }
                }
            }
            GroupActivation::Mention => {
                if is_mentioned {
                    decision
                } else {
                    match decision {
                        TriggerDecision::Reply { reason }
                        | TriggerDecision::IngestOnly { reason } => {
                            TriggerDecision::IngestOnly { reason }
                        }
                        other => other,
                    }
                }
            }
            GroupActivation::Keyword => {
                if is_mentioned {
                    decision
                } else if has_group_keyword(text, &config.group_keywords) {
                    TriggerDecision::Reply {
                        reason: TriggerReason::KeywordMatched,
                    }
                } else {
                    match decision {
                        TriggerDecision::Reply { reason }
                        | TriggerDecision::IngestOnly { reason } => {
                            TriggerDecision::IngestOnly { reason }
                        }
                        other => other,
                    }
                }
            }
        };
    }

    match config.ingest_policy {
        IngestPolicy::PreserveBase => decision,
        IngestPolicy::None | IngestPolicy::ReplyOnly => match decision {
            TriggerDecision::IngestOnly { reason } => TriggerDecision::Ignore { reason },
            other => other,
        },
        IngestPolicy::TargetedOnly => match decision {
            TriggerDecision::IngestOnly {
                reason: TriggerReason::MentionedOtherAgent,
            } => decision,
            TriggerDecision::IngestOnly { reason } => TriggerDecision::Ignore { reason },
            other => other,
        },
        IngestPolicy::AllHumanMessages => match decision {
            TriggerDecision::Ignore {
                reason: TriggerReason::NoTrigger,
            } if sender_kind == SenderKind::Human => TriggerDecision::IngestOnly {
                reason: TriggerReason::NoTrigger,
            },
            other => other,
        },
        IngestPolicy::AllNonBotMessages => match decision {
            TriggerDecision::Ignore {
                reason: TriggerReason::NoTrigger,
            } if !matches!(
                sender_kind,
                SenderKind::SelfBot
                    | SenderKind::OwnAgentGhost
                    | SenderKind::BridgeGhost
                    | SenderKind::ExternalBot
            ) =>
            {
                TriggerDecision::IngestOnly {
                    reason: TriggerReason::NoTrigger,
                }
            }
            other => other,
        },
        IngestPolicy::AllMessages => match decision {
            TriggerDecision::Ignore {
                reason: TriggerReason::NoTrigger,
            } if !matches!(
                sender_kind,
                SenderKind::SelfBot | SenderKind::OwnAgentGhost | SenderKind::BridgeGhost
            ) =>
            {
                TriggerDecision::IngestOnly {
                    reason: TriggerReason::NoTrigger,
                }
            }
            other => other,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentTriggerConfig, ConversationKind, ExternalBotPolicy, IngestPolicy, SenderKind,
        TriggerContext, TriggerDecision, TriggerReason, apply_agent_trigger_policy, decide_trigger,
        effective_conversation_kind,
    };
    use crate::auto_reply::GroupActivation;

    #[test]
    fn pair_room_is_treated_as_replyable() {
        let decision = decide_trigger(
            SenderKind::Human,
            Some("group"),
            Some(2),
            false,
            false,
            false,
            true,
            false,
        );
        assert_eq!(
            decision,
            TriggerDecision::Reply {
                reason: TriggerReason::PairRoom
            }
        );
        assert!(matches!(
            effective_conversation_kind(Some("group"), Some(2)),
            super::ConversationKind::PairRoom
        ));
    }

    #[test]
    fn plain_group_fallback_is_ingest_only() {
        let decision = decide_trigger(
            SenderKind::Human,
            Some("group"),
            None,
            false,
            false,
            false,
            true,
            false,
        );
        assert_eq!(
            decision,
            TriggerDecision::IngestOnly {
                reason: TriggerReason::RoomDefaultAgent
            }
        );
    }

    #[test]
    fn external_bots_are_ignored() {
        let decision = decide_trigger(
            SenderKind::ExternalBot,
            Some("group"),
            None,
            true,
            false,
            false,
            false,
            false,
        );
        assert_eq!(
            decision,
            TriggerDecision::Ignore {
                reason: TriggerReason::ExternalBotIgnored
            }
        );
    }

    #[test]
    fn mentioning_other_agent_becomes_ingest_only() {
        let decision = decide_trigger(
            SenderKind::Human,
            Some("group"),
            None,
            false,
            false,
            false,
            true,
            true,
        );
        assert_eq!(
            decision,
            TriggerDecision::IngestOnly {
                reason: TriggerReason::MentionedOtherAgent
            }
        );
    }

    #[test]
    fn reply_to_self_is_replyable() {
        let decision = decide_trigger(
            SenderKind::Human,
            Some("group"),
            None,
            false,
            true,
            false,
            true,
            false,
        );
        assert_eq!(
            decision,
            TriggerDecision::Reply {
                reason: TriggerReason::ReplyToMyMessage
            }
        );
    }

    #[test]
    fn keyword_policy_promotes_group_fallback_to_reply() {
        let base = TriggerDecision::IngestOnly {
            reason: TriggerReason::RoomDefaultAgent,
        };
        let decision = apply_agent_trigger_policy(
            base,
            TriggerContext {
                sender_kind: SenderKind::Human,
                conversation_kind: ConversationKind::Group,
                is_mentioned: false,
                reply_to_self: false,
                is_command: false,
                explicitly_targets_other_agent: false,
                text: "please ask savfox reviewer to inspect this",
            },
            &AgentTriggerConfig {
                group_activation: GroupActivation::Keyword,
                group_keywords: vec!["reviewer".to_owned()],
                ..AgentTriggerConfig::default()
            },
        );
        assert_eq!(
            decision,
            TriggerDecision::Reply {
                reason: TriggerReason::KeywordMatched
            }
        );
    }

    #[test]
    fn off_policy_suppresses_group_reply() {
        let base = TriggerDecision::Reply {
            reason: TriggerReason::MentionedMe,
        };
        let decision = apply_agent_trigger_policy(
            base,
            TriggerContext {
                sender_kind: SenderKind::Human,
                conversation_kind: ConversationKind::Group,
                is_mentioned: true,
                reply_to_self: false,
                is_command: false,
                explicitly_targets_other_agent: false,
                text: "hi",
            },
            &AgentTriggerConfig {
                group_activation: GroupActivation::Off,
                ..AgentTriggerConfig::default()
            },
        );
        assert_eq!(
            decision,
            TriggerDecision::IngestOnly {
                reason: TriggerReason::MentionedMe
            }
        );
    }

    #[test]
    fn reply_only_ingest_policy_drops_buffered_messages() {
        let decision = apply_agent_trigger_policy(
            TriggerDecision::IngestOnly {
                reason: TriggerReason::RoomDefaultAgent,
            },
            TriggerContext {
                sender_kind: SenderKind::Human,
                conversation_kind: ConversationKind::Group,
                is_mentioned: false,
                reply_to_self: false,
                is_command: false,
                explicitly_targets_other_agent: false,
                text: "hello",
            },
            &AgentTriggerConfig {
                ingest_policy: IngestPolicy::ReplyOnly,
                ..AgentTriggerConfig::default()
            },
        );
        assert_eq!(
            decision,
            TriggerDecision::Ignore {
                reason: TriggerReason::RoomDefaultAgent
            }
        );
    }

    #[test]
    fn external_bot_policy_parser_works() {
        assert_eq!(
            ExternalBotPolicy::from_str(Some("ingest_only")),
            ExternalBotPolicy::IngestOnly
        );
        assert_eq!(
            IngestPolicy::from_str(Some("targeted_only")),
            IngestPolicy::TargetedOnly
        );
    }
}
