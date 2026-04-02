use http::{HeaderMap, HeaderValue};
use savfox_protocol::protocol::SessionSource;

#[must_use]
pub fn build_conversation_headers(conversation_id: Option<String>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(id) = conversation_id {
        insert_header(&mut headers, "session_id", &id);
    }
    headers
}

pub(crate) fn subagent_header(source: &Option<SessionSource>) -> Option<String> {
    let SessionSource::SubAgent(sub) = source.as_ref()? else {
        return None;
    };
    match sub {
        savfox_protocol::protocol::SubAgentSource::Review => Some("review".to_owned()),
        savfox_protocol::protocol::SubAgentSource::Compact => Some("compact".to_owned()),
        savfox_protocol::protocol::SubAgentSource::SessionSpawn { .. } => {
            Some("collab_spawn".to_owned())
        }
        savfox_protocol::protocol::SubAgentSource::Other(label) => Some(label.clone()),
    }
}

pub(crate) fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        name.parse::<http::HeaderName>(),
        HeaderValue::from_str(value),
    ) {
        headers.insert(header_name, header_value);
    }
}
