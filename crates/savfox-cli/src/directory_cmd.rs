//! `savfox directory` — Channel-aware account, peer, and group lookups.

use clap::Parser;
use serde_json::{Value, json};

use crate::ws_rpc_client;

/// Directory lookups against a running gateway.
#[derive(Debug, Parser)]
pub struct DirectoryCommand {
    #[clap(subcommand)]
    pub action: DirectoryAction,

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
pub enum DirectoryAction {
    /// Show account/self information by channel.
    #[clap(name = "self")]
    SelfInfo {
        /// Restrict to one channel (discord, slack, telegram, whatsapp)
        #[clap(long)]
        channel: Option<String>,

        /// Output raw JSON.
        #[clap(long)]
        json: bool,
    },

    /// Contact/peer directory operations.
    Peers {
        #[clap(subcommand)]
        action: DirectoryPeersAction,
    },

    /// Group directory operations.
    Groups {
        #[clap(subcommand)]
        action: DirectoryGroupsAction,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum DirectoryPeersAction {
    /// Search peer contacts.
    List {
        /// Search query.
        #[clap(long)]
        query: Option<String>,

        /// Max number of rows to return.
        #[clap(long, default_value_t = 50)]
        limit: usize,

        /// Restrict to one channel.
        #[clap(long)]
        channel: Option<String>,

        /// Output raw JSON.
        #[clap(long)]
        json: bool,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum DirectoryGroupsAction {
    /// List groups.
    List {
        /// Search query.
        #[clap(long)]
        query: Option<String>,

        /// Max number of rows to return.
        #[clap(long, default_value_t = 50)]
        limit: usize,

        /// Restrict to one channel.
        #[clap(long)]
        channel: Option<String>,

        /// Output raw JSON.
        #[clap(long)]
        json: bool,
    },

    /// List members in a group.
    Members {
        /// Group identifier.
        #[clap(long = "group-id")]
        group_id: String,

        /// Restrict to one channel.
        #[clap(long)]
        channel: Option<String>,

        /// Max number of rows to return.
        #[clap(long, default_value_t = 200)]
        limit: usize,

        /// Output raw JSON.
        #[clap(long)]
        json: bool,
    },
}

pub async fn run(cmd: DirectoryCommand) -> Result<(), Box<dyn std::error::Error>> {
    let gateway_url = &cmd.gateway_url;
    let token = &cmd.token;

    match cmd.action {
        DirectoryAction::SelfInfo {
            channel,
            json: raw_json,
        } => {
            let mut params = json!({});
            if let Some(channel) = channel {
                params["channel"] = json!(channel);
            }

            let result = ws_rpc_client::rpc_call(gateway_url, token, "directory.self", params)
                .await
                .map_err(|e| format!("directory.self failed: {e}"))?;

            if raw_json {
                ws_rpc_client::print_json(&result);
            } else {
                print_self_table(&result);
            }
        }
        DirectoryAction::Peers { action } => match action {
            DirectoryPeersAction::List {
                query,
                limit,
                channel,
                json: raw_json,
            } => {
                let mut params = json!({ "limit": limit });
                if let Some(query) = query {
                    params["query"] = json!(query);
                }
                if let Some(channel) = channel {
                    params["channel"] = json!(channel);
                }

                let result =
                    ws_rpc_client::rpc_call(gateway_url, token, "directory.peers.list", params)
                        .await
                        .map_err(|e| format!("directory.peers.list failed: {e}"))?;

                if raw_json {
                    ws_rpc_client::print_json(&result);
                } else {
                    print_peers_table(&result);
                }
            }
        },
        DirectoryAction::Groups { action } => match action {
            DirectoryGroupsAction::List {
                query,
                limit,
                channel,
                json: raw_json,
            } => {
                let mut params = json!({ "limit": limit });
                if let Some(query) = query {
                    params["query"] = json!(query);
                }
                if let Some(channel) = channel {
                    params["channel"] = json!(channel);
                }

                let result =
                    ws_rpc_client::rpc_call(gateway_url, token, "directory.groups.list", params)
                        .await
                        .map_err(|e| format!("directory.groups.list failed: {e}"))?;

                if raw_json {
                    ws_rpc_client::print_json(&result);
                } else {
                    print_groups_table(&result);
                }
            }
            DirectoryGroupsAction::Members {
                group_id,
                channel,
                limit,
                json: raw_json,
            } => {
                let mut params = json!({ "group_id": group_id, "limit": limit });
                if let Some(channel) = channel {
                    params["channel"] = json!(channel);
                }

                let result =
                    ws_rpc_client::rpc_call(gateway_url, token, "directory.groups.members", params)
                        .await
                        .map_err(|e| format!("directory.groups.members failed: {e}"))?;

                if raw_json {
                    ws_rpc_client::print_json(&result);
                } else {
                    print_group_members_table(&result);
                }
            }
        },
    }

    Ok(())
}

fn print_self_table(result: &Value) {
    println!("Directory Accounts");
    println!();
    println!(
        "{:<10} {:<28} {:<11} {:<8} Source",
        "Channel", "Account", "Configured", "Enabled"
    );
    println!("{}", "-".repeat(88));

    let accounts = result
        .get("accounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for account in accounts {
        let channel = account
            .get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let account_id = account
            .get("account_id")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let configured = yes_no(account.get("configured").and_then(|v| v.as_bool()));
        let enabled = yes_no(account.get("enabled").and_then(|v| v.as_bool()));
        let source = account
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        println!("{channel:<10} {account_id:<28} {configured:<11} {enabled:<8} {source}");
    }
}

fn print_peers_table(result: &Value) {
    println!("Directory Peers");
    println!();
    println!(
        "{:<10} {:<22} {:<24} {:<18} Last Seen (ms)",
        "Channel", "Peer", "Display", "Identity"
    );
    println!("{}", "-".repeat(110));

    let peers = result
        .get("peers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for peer in peers {
        let channel = peer.get("channel").and_then(|v| v.as_str()).unwrap_or("-");
        let peer_id = peer.get("peer_id").and_then(|v| v.as_str()).unwrap_or("-");
        let name = peer.get("name").and_then(|v| v.as_str()).unwrap_or("-");
        let identity = peer.get("identity").and_then(|v| v.as_str()).unwrap_or("-");
        let last_seen = format_ms(peer.get("last_seen_ms"));
        println!("{channel:<10} {peer_id:<22} {name:<24} {identity:<18} {last_seen}");
    }
}

fn print_groups_table(result: &Value) {
    println!("Directory Groups");
    println!();
    println!(
        "{:<10} {:<22} {:<28} {:<8} Last Seen (ms)",
        "Channel", "Group ID", "Name", "Members"
    );
    println!("{}", "-".repeat(96));

    let groups = result
        .get("groups")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for group in groups {
        let channel = group.get("channel").and_then(|v| v.as_str()).unwrap_or("-");
        let group_id = group
            .get("group_id")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let name = group.get("name").and_then(|v| v.as_str()).unwrap_or("-");
        let members = group
            .get("members_estimate")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let last_seen = format_ms(group.get("last_seen_ms"));
        println!("{channel:<10} {group_id:<22} {name:<28} {members:<8} {last_seen}");
    }
}

fn print_group_members_table(result: &Value) {
    println!("Group Members");
    println!();
    println!(
        "{:<10} {:<22} {:<24} {:<8} Last Seen (ms)",
        "Channel", "User ID", "Display", "Sessions"
    );
    println!("{}", "-".repeat(96));

    let members = result
        .get("members")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for member in members {
        let channel = member
            .get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let user_id = member
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let name = member.get("name").and_then(|v| v.as_str()).unwrap_or("-");
        let sessions = member.get("sessions").and_then(|v| v.as_u64()).unwrap_or(0);
        let last_seen = format_ms(member.get("last_seen_ms"));
        println!("{channel:<10} {user_id:<22} {name:<24} {sessions:<8} {last_seen}");
    }
}

fn yes_no(value: Option<bool>) -> &'static str {
    if value.unwrap_or(false) { "yes" } else { "no" }
}

fn format_ms(value: Option<&Value>) -> String {
    value
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_owned())
}
