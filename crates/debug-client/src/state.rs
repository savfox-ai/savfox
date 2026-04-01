use std::collections::HashMap;

use savfox_app_server_protocol::RequestId;

#[derive(Debug, Default)]
pub struct State {
    pub pending: HashMap<RequestId, PendingRequest>,
    pub session_id: Option<String>,
    pub known_sessions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingRequest {
    Start,
    Resume,
    List,
}

#[derive(Debug, Clone)]
pub enum ReaderEvent {
    SessionReady {
        session_id: String,
    },
    SessionList {
        session_ids: Vec<String>,
        next_cursor: Option<String>,
    },
}
