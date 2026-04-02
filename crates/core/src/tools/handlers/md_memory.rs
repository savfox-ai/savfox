use std::path::PathBuf;

use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::md_memory::{
    MemoryFrontmatter, MemoryLayer, discover_md_memories, is_valid_slug, layer_dirs, parse_md_file,
    render_md_file, sanitize_slug, search_memories,
};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

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
        .or_else(|e| model_err(format!("invalid layer: {e}")))
}

/// Resolve the directory for a given layer.
fn resolve_dir(layer: MemoryLayer) -> Result<PathBuf, FunctionCallError> {
    let home = savfox_home();
    let pr = project_root();
    let dirs = layer_dirs(&home, pr.as_deref(), "default");
    let Some(dir) = dirs.into_iter().find(|(l, _)| *l == layer).map(|(_, p)| p) else {
        return model_err(format!(
            "layer '{layer}' directory not available (no project root?)"
        ));
    };
    Ok(dir)
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
        if let Some(filter) = layer_filter
            && layer != filter
        {
            continue;
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            return Some((layer, path));
        }
    }
    None
}

/// Agent tool for managing Markdown memory entries across 4 layers.
///
/// Actions: `list`, `get`, `create`, `update`, `delete`, `search`, `promote`.
pub struct MdMemoryHandler;

#[async_trait::async_trait]
impl ToolHandler for MdMemoryHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("MdMemoryHandler received unsupported payload"),
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
            other => model_err(format!("unknown md_memory action: {other}")),
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

    Ok(ToolOutput::ok(
        serde_json::to_string(&summary).unwrap_or_else(|_| "[]".to_owned()),
    ))
}

async fn handle_get(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let Some(slug) = args.slug.as_deref() else {
        return model_err("'slug' is required for get");
    };

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((_layer, path)) = find_entry_path(slug, layer_filter) else {
        return Ok(ToolOutput::fail(format!("memory entry '{slug}' not found")));
    };

    let content = std::fs::read_to_string(&path)
        .or_else(|e| model_err(format!("failed to read {}: {e}", path.display())))?;

    Ok(ToolOutput::ok(content))
}

async fn handle_create(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let Some(layer_str) = args.layer.as_deref() else {
        return model_err("'layer' is required for create");
    };
    let layer = parse_layer(layer_str)?;

    if layer == MemoryLayer::Session {
        return model_err("session layer entries cannot be created via this tool");
    }

    let Some(slug_raw) = args.slug.as_deref() else {
        return model_err("'slug' is required for create");
    };
    let slug = sanitize_slug(slug_raw);
    if !is_valid_slug(&slug) {
        return model_err(format!(
            "invalid slug '{slug_raw}' (must be lowercase alphanumeric with dashes)"
        ));
    }

    let Some(body) = args.content.as_deref() else {
        return model_err("'content' is required for create");
    };

    let dir = resolve_dir(layer)?;
    let path = dir.join(format!("{slug}.md"));
    if path.exists() {
        return model_err(format!(
            "memory entry '{slug}' already exists in {layer} layer"
        ));
    }

    std::fs::create_dir_all(&dir)
        .or_else(|e| model_err(format!("failed to create directory: {e}")))?;

    let now = chrono::Utc::now();
    let fm = MemoryFrontmatter {
        tags: args.tags.clone().unwrap_or_default(),
        priority: args.priority.unwrap_or(5),
        author: "agent".to_owned(),
        created_at: Some(now),
        updated_at: Some(now),
        ..Default::default()
    };

    let rendered = render_md_file(&fm, body);
    std::fs::write(&path, &rendered)
        .or_else(|e| model_err(format!("failed to write {}: {e}", path.display())))?;

    Ok(ToolOutput::ok(format!(
        "created memory '{slug}' in {layer} layer"
    )))
}

async fn handle_update(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let Some(slug) = args.slug.as_deref() else {
        return model_err("'slug' is required for update");
    };

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((_layer, path)) = find_entry_path(slug, layer_filter) else {
        return model_err(format!("memory entry '{slug}' not found"));
    };

    let existing = std::fs::read_to_string(&path)
        .or_else(|e| model_err(format!("failed to read {}: {e}", path.display())))?;

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
        std::fs::write(&path, &rendered)
            .or_else(|e| model_err(format!("failed to write {}: {e}", path.display())))?;
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
            std::fs::write(&path, &rendered)
                .or_else(|e| model_err(format!("failed to write {}: {e}", path.display())))?;
        }
    }

    Ok(ToolOutput::ok(format!("updated memory '{slug}'")))
}

async fn handle_delete(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let Some(slug) = args.slug.as_deref() else {
        return model_err("'slug' is required for delete");
    };

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((_layer, path)) = find_entry_path(slug, layer_filter) else {
        return Ok(ToolOutput::fail(format!("memory entry '{slug}' not found")));
    };

    std::fs::remove_file(&path)
        .or_else(|e| model_err(format!("failed to delete {}: {e}", path.display())))?;

    Ok(ToolOutput::ok(format!("deleted memory '{slug}'")))
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

    Ok(ToolOutput::ok(
        serde_json::to_string(&summary).unwrap_or_else(|_| "[]".to_owned()),
    ))
}

async fn handle_promote(args: &MdMemoryArgs) -> Result<ToolOutput, FunctionCallError> {
    let Some(slug) = args.slug.as_deref() else {
        return model_err("'slug' is required for promote");
    };

    let Some(target_str) = args.target_layer.as_deref() else {
        return model_err("'target_layer' is required for promote");
    };
    let target_layer = parse_layer(target_str)?;

    if target_layer == MemoryLayer::Session {
        return model_err("cannot promote to session layer");
    }

    let layer_filter = args.layer.as_deref().map(parse_layer).transpose()?;
    let Some((source_layer, source_path)) = find_entry_path(slug, layer_filter) else {
        return model_err(format!("memory entry '{slug}' not found"));
    };

    if source_layer == target_layer {
        return Ok(ToolOutput::ok(format!(
            "'{slug}' is already in {target_layer} layer"
        )));
    }

    let content = std::fs::read_to_string(&source_path)
        .or_else(|e| model_err(format!("failed to read: {e}")))?;

    let target_dir = resolve_dir(target_layer)?;
    std::fs::create_dir_all(&target_dir)
        .or_else(|e| model_err(format!("failed to create target dir: {e}")))?;

    let target_path = target_dir.join(format!("{slug}.md"));
    std::fs::write(&target_path, &content)
        .or_else(|e| model_err(format!("failed to write: {e}")))?;

    // Remove the source file.
    let _ = std::fs::remove_file(&source_path);

    Ok(ToolOutput::ok(format!(
        "promoted '{slug}' from {source_layer} to {target_layer}"
    )))
}
