use std::collections::BTreeMap;

use savfox_protocol::models::VIEW_IMAGE_TOOL_NAME;

use super::JsonSchema;
use super::declarations::{FunctionToolDecl, function_tool};
use crate::client_common::tools::ToolSpec;

pub(super) fn create_view_image_tool() -> ToolSpec {
    // Support only local filesystem path.
    let properties = BTreeMap::from([(
        "path".to_owned(),
        JsonSchema::String {
            description: Some("Local filesystem path to an image file".to_owned()),
        },
    )]);

    function_tool(FunctionToolDecl {
        name: VIEW_IMAGE_TOOL_NAME,
        description: "View a local image from the filesystem (only use if given a full filepath by the user, and the image isn't already attached to the session context within <image ...> tags).",
        properties,
        required: &["path"],
    })
}

pub(super) fn create_grep_files_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "pattern".to_owned(),
            JsonSchema::String {
                description: Some("Regular expression pattern to search for.".to_owned()),
            },
        ),
        (
            "include".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional glob that limits which files are searched (e.g. \"*.rs\" or \
                     \"*.{ts,tsx}\").".to_owned(),
                ),
            },
        ),
        (
            "path".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Directory or file path to search. Defaults to the session's working directory.".to_owned(),
                ),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of file paths to return (defaults to 100).".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "grep_files",
        description: "Finds files whose contents match the pattern and lists them by modification time.",
        properties,
        required: &["pattern"],
    })
}

pub(super) fn create_read_file_tool() -> ToolSpec {
    let indentation_properties = BTreeMap::from([
        (
            "anchor_line".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Anchor line to center the indentation lookup on (defaults to offset)."
                        .to_owned(),
                ),
            },
        ),
        (
            "max_levels".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "How many parent indentation levels (smaller indents) to include.".to_owned(),
                ),
            },
        ),
        (
            "include_siblings".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "When true, include additional blocks that share the anchor indentation."
                        .to_owned(),
                ),
            },
        ),
        (
            "include_header".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "Include doc comments or attributes directly above the selected block."
                        .to_owned(),
                ),
            },
        ),
        (
            "max_lines".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Hard cap on the number of lines returned when using indentation mode."
                        .to_owned(),
                ),
            },
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "file_path".to_owned(),
            JsonSchema::String {
                description: Some("Absolute path to the file".to_owned()),
            },
        ),
        (
            "offset".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "The line number to start reading from. Must be 1 or greater.".to_owned(),
                ),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("The maximum number of lines to return.".to_owned()),
            },
        ),
        (
            "mode".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional mode selector: \"slice\" for simple ranges (default) or \"indentation\" \
                     to expand around an anchor line.".to_owned(),
                ),
            },
        ),
        (
            "indentation".to_owned(),
            JsonSchema::Object {
                properties: indentation_properties,
                required: None,
                additional_properties: Some(false.into()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "read_file",
        description: "Reads a local file with 1-indexed line numbers, supporting slice and indentation-aware block modes.",
        properties,
        required: &["file_path"],
    })
}

pub(super) fn create_list_dir_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "dir_path".to_owned(),
            JsonSchema::String {
                description: Some("Absolute path to the directory to list.".to_owned()),
            },
        ),
        (
            "offset".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "The entry number to start listing from. Must be 1 or greater.".to_owned(),
                ),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("The maximum number of entries to return.".to_owned()),
            },
        ),
        (
            "depth".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "The maximum directory depth to traverse. Must be 1 or greater.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "list_dir",
        description: "Lists entries in a local directory with 1-indexed entry numbers and simple type labels.",
        properties,
        required: &["dir_path"],
    })
}

pub(super) fn create_write_file_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "file_path".to_owned(),
            JsonSchema::String {
                description: Some("Absolute path of the file to write.".to_owned()),
            },
        ),
        (
            "content".to_owned(),
            JsonSchema::String {
                description: Some("The content to write to the file.".to_owned()),
            },
        ),
        (
            "create_dirs".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "When true, create intermediate directories if they don't exist. Defaults to false.".to_owned(),
                ),
            },
        ),
        (
            "overwrite".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "When true (default), overwrite an existing file. When false, fail if the file already exists.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "write_file",
        description: "Write content to a file at the given absolute path. Use this for creating new files or completely replacing file contents.",
        properties,
        required: &["file_path", "content"],
    })
}

pub(super) fn create_web_fetch_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "url".to_owned(),
            JsonSchema::String {
                description: Some("URL to fetch (http or https).".to_owned()),
            },
        ),
        (
            "extract_mode".to_owned(),
            JsonSchema::String {
                description: Some(
                    "How to process the response: \"markdown\" (default, converts HTML to markdown), \"text\" (plain text), or \"raw\" (unprocessed body).".to_owned(),
                ),
            },
        ),
        (
            "max_length".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Maximum character length of the returned content. Defaults to 50000.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "web_fetch",
        description: "Fetch the content of a URL and return it as text. Useful for reading web pages, APIs, or documentation. HTML is converted to readable markdown by default.",
        properties,
        required: &["url"],
    })
}

pub(super) fn create_web_search_provider_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "query".to_owned(),
            JsonSchema::String {
                description: Some("The search query.".to_owned()),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of results to return (default 5, max 20).".to_owned(),
                ),
            },
        ),
        (
            "site".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional domain filter to restrict results to a specific site.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "web_search_provider",
        description: "Search the web using a search engine API. Returns structured results with title, URL, and snippet. Requires SAVFOX_WEB_SEARCH_API_KEY environment variable.",
        properties,
        required: &["query"],
    })
}
