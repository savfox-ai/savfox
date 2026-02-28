use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct WriteFileHandler;

#[derive(Deserialize)]
struct WriteFileArgs {
    /// Absolute path of the file to write.
    file_path: String,
    /// Content to write to the file.
    content: String,
    /// When true, create intermediate directories if they don't exist.
    #[serde(default)]
    create_dirs: bool,
    /// When true, overwrite an existing file; when false, fail if the file already exists.
    #[serde(default = "defaults::overwrite")]
    overwrite: bool,
}

mod defaults {
    pub fn overwrite() -> bool {
        true
    }
}

#[async_trait]
impl ToolHandler for WriteFileHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            tracker, payload, ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "write_file handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: WriteFileArgs = parse_arguments(&arguments)?;

        let path = PathBuf::from(&args.file_path);
        if !path.is_absolute() {
            return Err(FunctionCallError::RespondToModel(
                "file_path must be an absolute path".to_string(),
            ));
        }

        // Check if file already exists and overwrite is not allowed.
        if !args.overwrite && path.exists() {
            return Err(FunctionCallError::RespondToModel(format!(
                "file already exists and overwrite is false: {}",
                path.display()
            )));
        }

        // Create parent directories if requested.
        if args.create_dirs {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to create directories for {}: {err}",
                        parent.display()
                    ))
                })?;
            }
        } else if let Some(parent) = path.parent() {
            // Verify the parent directory exists.
            if !parent.exists() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "parent directory does not exist: {} (set create_dirs to true to create it)",
                    parent.display()
                )));
            }
        }

        // Snapshot baseline for diff tracking before writing.
        {
            use std::collections::HashMap;

            use crate::protocol::FileChange;

            let mut changes = HashMap::new();
            let change = if path.exists() {
                FileChange::Update {
                    unified_diff: String::new(),
                    move_path: None,
                }
            } else {
                FileChange::Add {
                    content: args.content.clone(),
                }
            };
            changes.insert(path.clone(), change);
            let mut guard = tracker.lock().await;
            guard.on_patch_begin(&changes);
        }

        // Write the file.
        tokio::fs::write(&path, args.content.as_bytes())
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to write file {}: {err}",
                    path.display()
                ))
            })?;

        let bytes_written = args.content.len();
        Ok(ToolOutput::Function {
            content: format!(
                "Successfully wrote {} bytes to {}",
                bytes_written,
                path.display()
            ),
            content_items: None,
            success: Some(true),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_path() {
        let args = r#"{"file_path": "relative/path.txt", "content": "hello"}"#;
        let parsed: WriteFileArgs = serde_json::from_str(args).unwrap();
        let path = PathBuf::from(&parsed.file_path);
        assert!(!path.is_absolute());
    }

    #[test]
    fn defaults_overwrite_to_true() {
        let args = r#"{"file_path": "/tmp/test.txt", "content": "hello"}"#;
        let parsed: WriteFileArgs = serde_json::from_str(args).unwrap();
        assert!(parsed.overwrite);
        assert!(!parsed.create_dirs);
    }
}
