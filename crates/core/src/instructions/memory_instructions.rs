use savfox_protocol::models::{ContentItem, ResponseItem};
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
pub(crate) const MEMORY_INSTRUCTIONS_PREFIX: &str = "# Memory Context";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "memory_instructions", rename_all = "snake_case")]
pub(crate) struct MemoryInstructions {
    pub text: String,
}

impl MemoryInstructions {
    #[allow(dead_code)]
    pub fn is_memory_instructions(message: &[ContentItem]) -> bool {
        if let [ContentItem::InputText { text }] = message {
            text.starts_with(MEMORY_INSTRUCTIONS_PREFIX)
        } else {
            false
        }
    }
}

impl From<MemoryInstructions> for ResponseItem {
    fn from(mi: MemoryInstructions) -> Self {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: mi.text }],
            end_turn: None,
            phase: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_instructions_roundtrip() {
        let mi = MemoryInstructions {
            text: "# Memory Context\n\n## [global] my-note\nHello".to_string(),
        };
        let item: ResponseItem = mi.into();
        let ResponseItem::Message { content, .. } = item else {
            panic!("expected Message");
        };
        assert!(MemoryInstructions::is_memory_instructions(&content));
    }

    #[test]
    fn test_not_memory_instructions() {
        assert!(!MemoryInstructions::is_memory_instructions(&[
            ContentItem::InputText {
                text: "Some other text".to_string()
            }
        ]));
    }
}
