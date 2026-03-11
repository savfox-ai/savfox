use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct MemoryEntry {
    pub slug: String,
    pub layer: String,
    pub tags: Vec<String>,
    pub priority: i32,
    pub pinned: bool,
    pub author: String,
    pub body_bytes: u64,
    pub body: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MemoryLayer {
    pub layer: String,
    pub path: String,
    pub exists: bool,
}

#[derive(Debug, Deserialize)]
pub struct MemoryListResponse {
    pub entries: Vec<MemoryEntry>,
}

#[derive(Debug, Deserialize)]
pub struct MemoryLayersResponse {
    pub layers: Vec<MemoryLayer>,
}
