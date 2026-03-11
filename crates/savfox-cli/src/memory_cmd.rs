//! `savfox memory` — Manage gateway markdown memories from the CLI.

use clap::Parser;
use serde_json::json;

use crate::ws_rpc_client;

/// Manage markdown memory entries via the gateway
#[derive(Debug, Parser)]
pub struct MemoryCommand {
    #[clap(subcommand)]
    pub action: MemoryAction,

    /// Gateway URL
    #[clap(
        long,
        env = "SAVFOX_GATEWAY_URL",
        default_value = "http://127.0.0.1:18881"
    )]
    pub gateway_url: String,

    /// Authentication token
    #[clap(long, env = "SAVFOX_GATEWAY_TOKEN", default_value = "")]
    pub token: String,
}

#[derive(Debug, clap::Subcommand)]
pub enum MemoryAction {
    /// List memory entries
    List {
        /// Filter by layer (global, project, agent, session)
        #[clap(long)]
        layer: Option<String>,

        /// Include entry content in output
        #[clap(long)]
        include_content: bool,
    },

    /// Search memory entries by keyword
    Search {
        /// Search query string
        query: String,

        /// Maximum results to return
        #[clap(long, default_value_t = 10)]
        limit: usize,

        /// Filter by layer
        #[clap(long)]
        layer: Option<String>,
    },

    /// Create a new memory entry
    Add {
        /// Content for the memory entry (markdown)
        content: String,

        /// Entry slug (filename without .md)
        #[clap(long)]
        slug: String,

        /// Target layer (global, project, agent)
        #[clap(long, default_value = "global")]
        layer: String,

        /// Comma-separated tags
        #[clap(long)]
        tags: Option<String>,

        /// Priority (0-100)
        #[clap(long)]
        priority: Option<u32>,
    },

    /// Delete a memory entry
    Delete {
        /// Entry slug to delete
        id: String,

        /// Layer to delete from (if not specified, deletes from all layers)
        #[clap(long)]
        layer: Option<String>,
    },
}

pub async fn run(cmd: MemoryCommand) -> Result<(), Box<dyn std::error::Error>> {
    let gateway_url = &cmd.gateway_url;
    let token = &cmd.token;

    match cmd.action {
        MemoryAction::List {
            layer,
            include_content,
        } => {
            let mut params = json!({ "include_content": include_content });
            if let Some(l) = layer {
                params["layer"] = json!(l);
            }
            let result = ws_rpc_client::rpc_call(gateway_url, token, "memory.list", params)
                .await
                .map_err(|e| format!("memory.list failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        MemoryAction::Search {
            query,
            limit,
            layer,
        } => {
            let mut params = json!({
                "query": query,
                "limit": limit,
            });
            if let Some(l) = layer {
                params["layer"] = json!(l);
            }
            let result = ws_rpc_client::rpc_call(gateway_url, token, "memory.search", params)
                .await
                .map_err(|e| format!("memory.search failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        MemoryAction::Add {
            content,
            slug,
            layer,
            tags,
            priority,
        } => {
            let mut params = json!({
                "slug": slug,
                "layer": layer,
                "content": content,
            });
            if let Some(t) = tags {
                let tag_list: Vec<&str> = t.split(',').map(|s| s.trim()).collect();
                params["tags"] = json!(tag_list);
            }
            if let Some(p) = priority {
                params["priority"] = json!(p);
            }
            let result = ws_rpc_client::rpc_call(gateway_url, token, "memory.create", params)
                .await
                .map_err(|e| format!("memory.create failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        MemoryAction::Delete { id, layer } => {
            let mut params = json!({ "slug": id });
            if let Some(l) = layer {
                params["layer"] = json!(l);
            }
            let result = ws_rpc_client::rpc_call(gateway_url, token, "memory.delete", params)
                .await
                .map_err(|e| format!("memory.delete failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
    }

    Ok(())
}
