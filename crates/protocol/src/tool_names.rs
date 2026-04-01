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
    pub fn read_only_tools() -> &'static [&'static str] {
        &[READ_FILE, LIST_DIR, GREP_FILES, MD_MEMORY]
    }

    /// Tools that require write or exec permissions.
    pub fn write_tools() -> &'static [&'static str] {
        &[SHELL, WRITE_FILE]
    }
}

/// Map dotted tool names to canonical short names.
pub fn normalize_tool_name(name: &str) -> &str {
    match name {
        "shell.execute" | "shell.run" | "local_shell" | "shell_command" => "shell",
        "files.read" => "read_file",
        "files.write" | "files.delete" => "write_file",
        "files.list" => "list_dir",
        "web.fetch" => "web_fetch",
        "web.search" => "web_search",
        "browser.navigate" | "browser.click" | "browser.screenshot" => "browser",
        "memory.get" | "memory.list" | "memory.set" => "md_memory",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_legacy_names() {
        assert_eq!(normalize_tool_name("shell.execute"), "shell");
        assert_eq!(normalize_tool_name("files.read"), "read_file");
        assert_eq!(normalize_tool_name("files.write"), "write_file");
        assert_eq!(normalize_tool_name("web.fetch"), "web_fetch");
        assert_eq!(normalize_tool_name("browser.navigate"), "browser");
        assert_eq!(normalize_tool_name("memory.get"), "md_memory");
    }

    #[test]
    fn test_normalize_already_canonical() {
        assert_eq!(normalize_tool_name("shell"), "shell");
        assert_eq!(normalize_tool_name("read_file"), "read_file");
        assert_eq!(normalize_tool_name("unknown_tool"), "unknown_tool");
    }
}
