//! `savfox agents` — Manage agents from CLI.

use clap::Parser;
use serde_json::json;

use crate::ws_rpc_client;

/// Manage agents
#[derive(Debug, Parser)]
pub struct AgentsCommand {
    #[clap(subcommand)]
    pub action: AgentsAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum AgentsAction {
    /// List all agents
    List {
        /// Output format
        #[clap(long, default_value = "table")]
        format: String,
    },
    /// Create a new agent
    Create {
        /// Agent name
        name: String,
        /// Model to use (e.g., openai/gpt-4o)
        #[clap(long)]
        model: Option<String>,
        /// System prompt
        #[clap(long)]
        prompt: Option<String>,
    },
    /// Delete an agent
    Delete {
        /// Agent name
        name: String,
        /// Skip confirmation
        #[clap(long)]
        force: bool,
    },
    /// Show or set agent identity
    Identity {
        /// Agent name
        name: String,
        /// New identity text (if setting)
        #[clap(long)]
        set: Option<String>,
    },
    /// Manage agent-channel bindings (routing rules)
    Bindings {
        #[clap(subcommand)]
        action: BindingsAction,
    },
    /// Manage tool policy for an agent
    Tools {
        #[clap(subcommand)]
        action: ToolsAction,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum ToolsAction {
    /// List available tools and their status for an agent
    List {
        /// Agent name or ID
        #[clap(long)]
        agent: String,
        /// Output format
        #[clap(long, default_value = "table")]
        format: String,
    },
    /// Allow a tool for an agent
    Allow {
        /// Agent name or ID
        #[clap(long)]
        agent: String,
        /// Tool name (e.g., "shell.execute", "files.read")
        tool: String,
    },
    /// Deny a tool for an agent
    Deny {
        /// Agent name or ID
        #[clap(long)]
        agent: String,
        /// Tool name (e.g., "shell.execute", "files.read")
        tool: String,
    },
    /// Set tool policy profile for an agent
    SetProfile {
        /// Agent name or ID
        #[clap(long)]
        agent: String,
        /// Profile name: default, restricted, full
        profile: String,
    },
    /// Reset tool policy to default
    Reset {
        /// Agent name or ID
        #[clap(long)]
        agent: String,
    },
    /// Show current tool policy for an agent
    Get {
        /// Agent name or ID
        #[clap(long)]
        agent: String,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum BindingsAction {
    /// List current agent-channel bindings
    List,

    /// Bind an agent to a channel
    Add {
        /// Agent name or ID
        agent: String,
        /// Channel identifier (e.g. "discord:12345", "telegram:67890")
        channel: String,
        /// Priority (lower = higher priority)
        #[clap(long, default_value_t = 0)]
        priority: u32,
    },

    /// Remove a binding by index
    Remove {
        /// Binding index (0-based, from the list output)
        id: usize,
    },
}

pub async fn run(
    cmd: AgentsCommand,
    gateway_url: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd.action {
        AgentsAction::List { format: _ } => {
            let result = ws_rpc_client::rpc_call(gateway_url, token, "agents.list", json!({}))
                .await
                .map_err(|e| format!("agents.list failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        AgentsAction::Create {
            name,
            model,
            prompt,
        } => {
            let mut params = json!({ "name": name });
            if let Some(m) = model {
                params["model"] = json!(m);
            }
            if let Some(p) = prompt {
                params["prompt"] = json!(p);
            }
            let result = ws_rpc_client::rpc_call(gateway_url, token, "agents.create", params)
                .await
                .map_err(|e| format!("agents.create failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        AgentsAction::Delete { name, force: _ } => {
            let params = json!({ "name": name });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "agents.delete", params)
                .await
                .map_err(|e| format!("agents.delete failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        AgentsAction::Identity { name, set } => {
            if let Some(identity) = set {
                // Set identity — use agents.update with identity field.
                let params = json!({
                    "name": name,
                    "identity": identity,
                });
                let result = ws_rpc_client::rpc_call(gateway_url, token, "agents.update", params)
                    .await
                    .map_err(|e| format!("agents.update failed: {e}"))?;
                ws_rpc_client::print_json(&result);
            } else {
                // Get identity.
                let result = ws_rpc_client::rpc_call(
                    gateway_url,
                    token,
                    "agent.identity.get",
                    json!({ "name": name }),
                )
                .await
                .map_err(|e| format!("agent.identity.get failed: {e}"))?;
                ws_rpc_client::print_json(&result);
            }
        }
        AgentsAction::Bindings { action } => {
            run_bindings(action, gateway_url, token).await?;
        }
        AgentsAction::Tools { action } => {
            run_tools(action, gateway_url, token).await?;
        }
    }
    Ok(())
}

async fn run_bindings(
    action: BindingsAction,
    gateway_url: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        BindingsAction::List => {
            let result =
                ws_rpc_client::rpc_call(gateway_url, token, "routing.rules.list", json!({}))
                    .await
                    .map_err(|e| format!("routing.rules.list failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        BindingsAction::Add {
            agent,
            channel,
            priority,
        } => {
            // Get current rules, append the new one, and set them back.
            let current =
                ws_rpc_client::rpc_call(gateway_url, token, "routing.rules.list", json!({}))
                    .await
                    .map_err(|e| format!("routing.rules.list failed: {e}"))?;

            let mut rules = current
                .get("rules")
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();

            rules.push(json!({
                "agent": agent,
                "channel": channel,
                "priority": priority,
            }));

            let params = json!({ "rules": rules });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "routing.rules.set", params)
                .await
                .map_err(|e| format!("routing.rules.set failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        BindingsAction::Remove { id } => {
            // Get current rules, remove by index, and set them back.
            let current =
                ws_rpc_client::rpc_call(gateway_url, token, "routing.rules.list", json!({}))
                    .await
                    .map_err(|e| format!("routing.rules.list failed: {e}"))?;

            let mut rules = current
                .get("rules")
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();

            if id >= rules.len() {
                return Err(format!(
                    "binding index {id} out of range (have {} bindings)",
                    rules.len()
                )
                .into());
            }
            rules.remove(id);

            let params = json!({ "rules": rules });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "routing.rules.set", params)
                .await
                .map_err(|e| format!("routing.rules.set failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
    }
    Ok(())
}

async fn run_tools(
    action: ToolsAction,
    gateway_url: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ToolsAction::List { agent, format } => {
            let params = json!({ "agentId": agent });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "tools.list", params)
                .await
                .map_err(|e| format!("tools.list failed: {e}"))?;

            if format == "json" {
                ws_rpc_client::print_json(&result);
            } else {
                print_tools_table(&result);
            }
        }
        ToolsAction::Allow { agent, tool } => {
            let params = json!({ "agentId": agent, "tool": tool });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "tools.policy.allow", params)
                .await
                .map_err(|e| format!("tools.policy.allow failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        ToolsAction::Deny { agent, tool } => {
            let params = json!({ "agentId": agent, "tool": tool });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "tools.policy.deny", params)
                .await
                .map_err(|e| format!("tools.policy.deny failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        ToolsAction::SetProfile { agent, profile } => {
            let params = json!({ "agentId": agent, "profile": profile });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "tools.policy.set", params)
                .await
                .map_err(|e| format!("tools.policy.set failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        ToolsAction::Reset { agent } => {
            let params = json!({ "agentId": agent });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "tools.policy.reset", params)
                .await
                .map_err(|e| format!("tools.policy.reset failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        ToolsAction::Get { agent } => {
            let params = json!({ "agentId": agent });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "tools.policy.get", params)
                .await
                .map_err(|e| format!("tools.policy.get failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
    }
    Ok(())
}

fn print_tools_table(result: &serde_json::Value) {
    let agent_id = result
        .get("agentId")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let count = result.get("count").and_then(|v| v.as_u64()).unwrap_or(0);

    println!("Tools for agent: {} ({} tools)\n", agent_id, count);

    if let Some(tools) = result.get("tools").and_then(|v| v.as_array()) {
        println!(
            "{:<25} {:<12} {:<10} {:<10}",
            "Tool", "Category", "Allowed", "Approval"
        );
        println!("{:-<25} {:-<12} {:-<10} {:-<10}", "", "", "", "");

        for tool in tools {
            let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let category = tool.get("category").and_then(|v| v.as_str()).unwrap_or("?");
            let allowed = tool
                .get("allowed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let approval = tool
                .get("requiresApproval")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let allowed_str = if allowed { "Yes" } else { "No" };
            let approval_str = if approval { "Required" } else { "Auto" };

            println!(
                "{:<25} {:<12} {:<10} {:<10}",
                name, category, allowed_str, approval_str
            );
        }
    }
}
