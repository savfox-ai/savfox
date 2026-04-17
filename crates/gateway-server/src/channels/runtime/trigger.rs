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
    Command,
    RoomDefaultAgent,
    ExternalBotIgnored,
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
            Self::Command => "command",
            Self::RoomDefaultAgent => "room_default_agent",
            Self::ExternalBotIgnored => "external_bot_ignored",
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

#[cfg(test)]
mod tests {
    use super::{
        SenderKind, TriggerDecision, TriggerReason, decide_trigger, effective_conversation_kind,
    };

    #[test]
    fn pair_room_is_treated_as_replyable() {
        let decision = decide_trigger(
            SenderKind::Human,
            Some("group"),
            Some(2),
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
}
