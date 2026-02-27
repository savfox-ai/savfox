//! 4-layer Markdown memory system.
//!
//! Provides user-editable `.md` files organized in four layers:
//! - **Global**: `~/.savfox/memory/global/*.md` — user preferences, coding conventions
//! - **Project**: `<git-root>/.savfox/memory/*.md` — project-specific knowledge
//! - **Agent**: `~/.savfox/memory/agents/<name>/*.md` — agent personality/knowledge
//! - **Session**: in-memory only — temporary working notes (not persisted)

pub mod batch_embed;
pub mod budget;
pub mod discovery;
pub mod entry;
pub mod frontmatter;
pub mod hybrid_search;
pub mod search;
pub mod vector_store;

pub use budget::{DEFAULT_MD_MEMORY_MAX_BYTES, assemble_memory_prompt};
pub use discovery::{discover_md_memories, is_valid_slug, layer_dirs, sanitize_slug};
pub use entry::{MAX_MEMORY_FILE_BYTES, MdMemoryEntry, MemoryFrontmatter, MemoryLayer};
pub use frontmatter::{parse_md_file, render_md_file};
pub use hybrid_search::{HybridResult, HybridSearchConfig, hybrid_search};
pub use search::search_memories;
pub use vector_store::{SimilarityResult, VectorStore};
