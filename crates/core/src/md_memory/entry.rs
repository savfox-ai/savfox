use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The four layers of the Markdown memory system, ordered by scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryLayer {
    Global = 0,
    Project = 1,
    Agent = 2,
    Session = 3,
}

impl MemoryLayer {
    #[must_use] 
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Agent => "agent",
            Self::Session => "session",
        }
    }
}

impl std::fmt::Display for MemoryLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MemoryLayer {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project),
            "agent" => Ok(Self::Agent),
            "session" => Ok(Self::Session),
            other => Err(format!("unknown memory layer: {other}")),
        }
    }
}

/// YAML frontmatter parsed from the top of a `.md` memory file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFrontmatter {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

fn default_priority() -> u32 {
    5
}

fn default_author() -> String {
    "user".to_owned()
}

impl Default for MemoryFrontmatter {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            priority: 5,
            pinned: false,
            author: "user".to_owned(),
            created_at: None,
            updated_at: None,
            expires_at: None,
        }
    }
}

/// A single Markdown memory entry, loaded from disk or held in-memory (session layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MdMemoryEntry {
    /// File stem (without `.md`) used as a unique identifier within a layer.
    pub slug: String,
    /// Which layer this entry belongs to.
    pub layer: MemoryLayer,
    /// Parsed YAML frontmatter.
    pub frontmatter: MemoryFrontmatter,
    /// Markdown body (everything after the frontmatter).
    pub body: String,
    /// Path to the `.md` file on disk (`None` for session-layer entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<PathBuf>,
    /// Byte length of the body (for budget calculations).
    pub body_bytes: usize,
}

/// Maximum allowed size for a single `.md` memory file (64 KB).
pub const MAX_MEMORY_FILE_BYTES: usize = 64 * 1024;
