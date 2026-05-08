use savfox_protocol::models::{ContentItem, ResponseItem};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "memory_instructions", rename_all = "snake_case")]
pub(crate) struct MemoryInstructions {
    pub text: String,
}

impl From<MemoryInstructions> for ResponseItem {
    fn from(mi: MemoryInstructions) -> Self {
        Self::Message {
            id: None,
            role: "user".to_owned(),
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
        let text = "# Memory Context\n\n## [global] my-note\nHello".to_owned();
        let mi = MemoryInstructions { text: text.clone() };
        let item: ResponseItem = mi.into();
        let ResponseItem::Message { content, role, .. } = item else {
            panic!("expected Message");
        };
        assert_eq!(role, "user");
        assert!(matches!(
            content.as_slice(),
            [ContentItem::InputText { text: t }] if t == &text
        ));
    }
}
