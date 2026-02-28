use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::md_memory::{
    MemoryFrontmatter, MemoryLayer, discover_md_memories, is_valid_slug, layer_dirs, parse_md_file,
    render_md_file, sanitize_slug, search_memories,
};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Agent tool for managing Markdown memory entries across 4 layers.
///
/// Actions: `list`, `get`, `create`, `update`, `delete`, `search`, `promote`.
pub struct MdMemoryHandler;

#[derive(Deserialize)]
struct MdMemoryArgs {
    action: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    target_layer: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    priority: Option<u32>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default = "defaults::limit")]
    limit: usize,
}

mod defaults {
    pub fn limit() -> usize {
        10
    }
}

fn savfox_home() -> PathBuf {
    let home = std::env::var("SAVFOX_HOME").unwrap_or_else(|_| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".savfox")
            .to_string_lossy()
            .into_owned()
    });
    PathBuf::from(home)
}

fn project_root() -> Option<PathBuf> {
    crate::git_info::get_git_repo_root(&std::env::current_dir().unwrap_or_default())
}

fn parse_layer(s: &str) -> Result<MemoryLayer, FunctionCallError> {
    s.parse::<MemoryLayer>()
        .map_err(|e| FunctionCallError::RespondToModel(format!("invalid layer: {e}")))
}

/// Resolve the directory for a given layer.
fn resolve_dir(layer: MemoryLayer) -> Result<PathBuf, FunctionCallError> {
    let home = savfox_home();
    let pr = project_root();
    let dirs = layer_dirs(&home, pr.as_deref(), "default");
    dirs.into_iter()
        .find(|(l, _)| *l == layer)
        .map(|(_, p)| p)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!(
                "layer '{}' directory not available (no project root?)",
                layer
            ))
        })
}

/// Find an entry by slug across all layers, optionally filtered by layer.
fn find_entry_path(
    slug: &str,
    layer_filter: Option<MemoryLayer>,
) -> Option<(MemoryLayer, PathBuf)> {
    let home = savfox_home();
    let pr = project_root();
    let dirs = layer_dirs(&home, pr.as_deref(), "default");

    for (layer, dir) in dirs {
        if let Some(filter) = layer_filter {
            if layer != filter {
                continue;
            }
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            return Some((layer, path));
        }
    }
    None
}

#[async_trait]
impl ToolHandler for MdMemoryHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "md_memory requires function payload".to_string(),
                ));
            }
        };

        let args: MdMemoryArgs = parse_arguments(&arguments)?;

        match args.action.as_str() {
            "list" => handle_list(&args).await,
            "get" => handle_get(&args).await,
            "create" => handle_create(&args).await,
            "update" => handle_update(&args).await,
            "delete" => handle_delete(&args).await,
            "search" => handle_search(&args).await,
            "promote" => handle_promote(&args).await,
            other => Err(FunctionCallError::RespondToModel(format!(
                "unknown md_memory action: {other}"
            ))),
        }
    }
}

async fn handle_list(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let home = savfox_home();
    let pr = project_root();
    let entries = discover_md_memories(&home, pr.as_deref(), "default").await;

    let filtered: Vec<_> = if let Some(ref layer_str) = args.layer {
        let layer = parse_layer(layer_str)?;
        entries.into_iter().filter(|e| e.layer == layer).collect()
    } else {
        entries
    };

    let summary: Vec<serde_json::Value> = filtered
        .iter()
        .take(args.limit)
        .map(|e| {
            serde_json::json!({
                "slug": e.slug,
                "layer": e.layer,
                "tags": e.frontmatter.tags,
                "priority": e.frontmatter.priority,
                "pinned": e.frontmatter.pinned,
                "body_bytes": e.body_bytes,
            })
        })
        .collect();

    Ok(ToolOutput::Function {
        content: serde_json::to_string(&summary).unwrap_or_else(|_| "[]".to_string()),
        content_items: None,
        success: Some(true),
    })
}

async fn handle_get(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let slug = args.slug.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'slug' is required for get".to_string())
    })?;

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((_layer, path)) = find_entry_path(slug, layer_filter) else {
        return Ok(ToolOutput::Function {
            content: format!("memory entry '{slug}' not found"),
            content_items: None,
            success: Some(false),
        });
    };

    let content = std::fs::read_to_string(&path).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to read {}: {e}", path.display()))
    })?;

    Ok(ToolOutput::Function {
        content,
        content_items: None,
        success: Some(true),
    })
}

async fn handle_create(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let layer_str = args.layer.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'layer' is required for create".to_string())
    })?;
    let layer = parse_layer(layer_str)?;

    if layer == MemoryLayer::Session {
        return Err(FunctionCallError::RespondToModel(
            "session layer entries cannot be created via this tool".to_string(),
        ));
    }

    let slug_raw = args.slug.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'slug' is required for create".to_string())
    })?;
    let slug = sanitize_slug(slug_raw);
    if !is_valid_slug(&slug) {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid slug '{slug_raw}' (must be lowercase alphanumeric with dashes)"
        )));
    }

    let body = args.content.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'content' is required for create".to_string())
    })?;

    let dir = resolve_dir(layer)?;
    let path = dir.join(format!("{slug}.md"));
    if path.exists() {
        return Err(FunctionCallError::RespondToModel(format!(
            "memory entry '{slug}' already exists in {layer} layer"
        )));
    }

    std::fs::create_dir_all(&dir).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to create directory: {e}"))
    })?;

    let now = chrono::Utc::now();
    let fm = MemoryFrontmatter {
        tags: args.tags.clone().unwrap_or_default(),
        priority: args.priority.unwrap_or(5),
        author: "agent".to_string(),
        created_at: Some(now),
        updated_at: Some(now),
        ..Default::default()
    };

    let rendered = render_md_file(&fm, body);
    std::fs::write(&path, &rendered).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to write {}: {e}", path.display()))
    })?;

    Ok(ToolOutput::Function {
        content: format!("created memory '{slug}' in {layer} layer"),
        content_items: None,
        success: Some(true),
    })
}

async fn handle_update(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let slug = args.slug.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'slug' is required for update".to_string())
    })?;

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((_layer, path)) = find_entry_path(slug, layer_filter) else {
        return Err(FunctionCallError::RespondToModel(format!(
            "memory entry '{slug}' not found"
        )));
    };

    let existing = std::fs::read_to_string(&path).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to read {}: {e}", path.display()))
    })?;

    let (mut fm, old_body) = parse_md_file(&existing);

    if let Some(ref content) = args.content {
        // Replace body with new content.
        fm.updated_at = Some(chrono::Utc::now());
        if let Some(ref tags) = args.tags {
            fm.tags = tags.clone();
        }
        if let Some(priority) = args.priority {
            fm.priority = priority;
        }

        let rendered = render_md_file(&fm, content);
        std::fs::write(&path, &rendered).map_err(|e| {
            FunctionCallError::RespondToModel(format!("failed to write {}: {e}", path.display()))
        })?;
    } else {
        // Only update metadata.
        let mut changed = false;
        if let Some(ref tags) = args.tags {
            fm.tags = tags.clone();
            changed = true;
        }
        if let Some(priority) = args.priority {
            fm.priority = priority;
            changed = true;
        }
        if changed {
            fm.updated_at = Some(chrono::Utc::now());
            let rendered = render_md_file(&fm, &old_body);
            std::fs::write(&path, &rendered).map_err(|e| {
                FunctionCallError::RespondToModel(format!(
                    "failed to write {}: {e}",
                    path.display()
                ))
            })?;
        }
    }

    Ok(ToolOutput::Function {
        content: format!("updated memory '{slug}'"),
        content_items: None,
        success: Some(true),
    })
}

async fn handle_delete(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let slug = args.slug.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'slug' is required for delete".to_string())
    })?;

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((_layer, path)) = find_entry_path(slug, layer_filter) else {
        return Ok(ToolOutput::Function {
            content: format!("memory entry '{slug}' not found"),
            content_items: None,
            success: Some(false),
        });
    };

    std::fs::remove_file(&path).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to delete {}: {e}", path.display()))
    })?;

    Ok(ToolOutput::Function {
        content: format!("deleted memory '{slug}'"),
        content_items: None,
        success: Some(true),
    })
}

async fn handle_search(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let query = args.query.as_deref().unwrap_or("");
    let home = savfox_home();
    let pr = project_root();
    let entries = discover_md_memories(&home, pr.as_deref(), "default").await;

    let results = search_memories(&entries, query, args.limit);
    let summary: Vec<serde_json::Value> = results
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "slug": e.slug,
                "layer": e.layer,
                "tags": e.frontmatter.tags,
                "priority": e.frontmatter.priority,
                "body_preview": if e.body.len() > 200 {
                    format!("{}...", &e.body[..200])
                } else {
                    e.body.clone()
                },
            })
        })
        .collect();

    Ok(ToolOutput::Function {
        content: serde_json::to_string(&summary).unwrap_or_else(|_| "[]".to_string()),
        content_items: None,
        success: Some(true),
    })
}

async fn handle_promote(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let slug = args.slug.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'slug' is required for promote".to_string())
    })?;

    let target_str = args.target_layer.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel("'target_layer' is required for promote".to_string())
    })?;
    let target_layer = parse_layer(target_str)?;

    if target_layer == MemoryLayer::Session {
        return Err(FunctionCallError::RespondToModel(
            "cannot promote to session layer".to_string(),
        ));
    }

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((source_layer, source_path)) = find_entry_path(slug, layer_filter) else {
        return Err(FunctionCallError::RespondToModel(format!(
            "memory entry '{slug}' not found"
        )));
    };

    if source_layer == target_layer {
        return Ok(ToolOutput::Function {
            content: format!("'{slug}' is already in {target_layer} layer"),
            content_items: None,
            success: Some(true),
        });
    }

    let content = std::fs::read_to_string(&source_path)
        .map_err(|e| FunctionCallError::RespondToModel(format!("failed to read: {e}")))?;

    let target_dir = resolve_dir(target_layer)?;
    std::fs::create_dir_all(&target_dir).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to create target dir: {e}"))
    })?;

    let target_path = target_dir.join(format!("{slug}.md"));
    std::fs::write(&target_path, &content)
        .map_err(|e| FunctionCallError::RespondToModel(format!("failed to write: {e}")))?;

    // Remove the source file.
    let _ = std::fs::remove_file(&source_path);

    Ok(ToolOutput::Function {
        content: format!("promoted '{slug}' from {source_layer} to {target_layer}"),
        content_items: None,
        success: Some(true),
    })
}
