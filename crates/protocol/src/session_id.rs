use std::borrow::Cow;
use std::fmt::Display;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Unique identifier for a Savfox session.
///
/// Uses UUID v7 for time-ordered, globally unique identifiers.
/// This ensures good database index performance and natural sorting by creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, TS, Hash)]
#[ts(type = "string")]
pub struct SessionId {
    uuid: Uuid,
}

impl SessionId {
    /// Creates a new SessionId using UUID v7 (time-ordered).
    ///
    /// # Example
    /// ```rust
    /// use savfox_protocol::SessionId;
    /// let id = SessionId::new();
    /// ```
    #[must_use] 
    pub fn new() -> Self {
        Self {
            uuid: Uuid::now_v7(),
        }
    }

    /// Parses a SessionId from a string representation.
    ///
    /// # Errors
    /// Returns an error if the string is not a valid UUID.
    ///
    /// # Example
    /// ```rust
    /// use savfox_protocol::SessionId;
    /// let id = SessionId::from_string("018e0d46-5d1f-7d2e-8c3b-4a5b6c7d8e9f").unwrap();
    /// ```
    pub fn from_string(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self {
            uuid: Uuid::parse_str(s)?,
        })
    }

    /// Check if this SessionId is nil (all zeros).
    #[must_use] 
    pub fn is_empty(&self) -> bool {
        self.uuid == Uuid::nil()
    }
}

impl TryFrom<&str> for SessionId {
    type Error = uuid::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::from_string(value)
    }
}

impl TryFrom<String> for SessionId {
    type Error = uuid::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_string(value.as_str())
    }
}

impl From<SessionId> for String {
    fn from(value: SessionId) -> Self {
        value.to_string()
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.uuid, f)
    }
}

impl Serialize for SessionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&self.uuid)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let uuid = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
        Ok(Self { uuid })
    }
}

impl JsonSchema for SessionId {
    fn schema_name() -> Cow<'static, str> {
        "SessionId".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <String>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_session_id_default_is_not_zeroes() {
        let id = SessionId::default();
        assert_ne!(id.uuid, Uuid::nil());
    }
}
