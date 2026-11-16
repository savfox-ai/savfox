//! Canonical tool name registry for the unified permission system.
//!
//! All permission policies, presets, and tool filtering use these canonical
//! short names, which match the runtime tool handler names in `crates/core`.

/// Canonical tool identifiers.
pub mod canonical {
    pub const SHELL: &str = "shell";
    pub const READ_FILE: &str = "read_file";
    pub const WRITE_FILE: &str = "write_file";
    pub const LIST_DIR: &str = "list_dir";
    pub const GREP_FILES: &str = "grep_files";
    pub const BROWSER: &str = "browser";
    pub const WEB_FETCH: &str = "web_fetch";
    pub const WEB_SEARCH: &str = "web_search";
    pub const MD_MEMORY: &str = "md_memory";
    pub const IMAGE_GENERATE: &str = "image_generate";

    /// Returns all canonical tool names.
    #[must_use]
    pub fn all() -> &'static [&'static str] {
        &[
            SHELL,
            READ_FILE,
            WRITE_FILE,
            LIST_DIR,
            GREP_FILES,
            BROWSER,
            WEB_FETCH,
            WEB_SEARCH,
            MD_MEMORY,
            IMAGE_GENERATE,
        ]
    }

    /// Read-only tools that are safe with a `ReadOnly` sandbox.
    #[must_use]
    pub fn read_only_tools() -> &'static [&'static str] {
        &[READ_FILE, LIST_DIR, GREP_FILES, MD_MEMORY]
    }

    /// Tools that require write or exec permissions.
    #[must_use]
    pub fn write_tools() -> &'static [&'static str] {
        &[SHELL, WRITE_FILE]
    }
}
