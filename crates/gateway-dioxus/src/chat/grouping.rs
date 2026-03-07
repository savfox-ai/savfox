use crate::chat::message::{MessageRole, NormalizedMessage};

#[derive(Clone, Debug, PartialEq)]
pub struct MessageGroup {
    pub role: MessageRole,
    pub messages: Vec<NormalizedMessage>,
    pub is_streaming: bool,
    pub started_at: i64,
}

impl MessageGroup {
    pub fn new(role: MessageRole) -> Self {
        Self {
            role,
            messages: Vec::new(),
            is_streaming: false,
            started_at: chrono::Utc::now().timestamp(),
        }
    }

    pub fn add_message(&mut self, message: NormalizedMessage) {
        self.messages.push(message);
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn total_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }

    pub fn first_message(&self) -> Option<&NormalizedMessage> {
        self.messages.first()
    }

    pub fn last_message(&self) -> Option<&NormalizedMessage> {
        self.messages.last()
    }
}

pub fn group_messages(messages: Vec<NormalizedMessage>) -> Vec<MessageGroup> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut groups: Vec<MessageGroup> = Vec::new();
    let mut current_group: Option<MessageGroup> = None;

    for message in messages {
        if let Some(ref mut group) = current_group {
            if group.role == message.role {
                group.add_message(message);
            } else {
                groups.push(group.clone());
                let mut new_group = MessageGroup::new(message.role.clone());
                new_group.started_at = message.timestamp;
                new_group.add_message(message);
                current_group = Some(new_group);
            }
        } else {
            let mut new_group = MessageGroup::new(message.role.clone());
            new_group.started_at = message.timestamp;
            new_group.add_message(message);
            current_group = Some(new_group);
        }
    }

    if let Some(group) = current_group {
        if !group.is_empty() {
            groups.push(group);
        }
    }

    groups
}

pub fn should_start_new_group(current_role: &MessageRole, new_role: &MessageRole) -> bool {
    current_role != new_role
}
