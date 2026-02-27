//! `savfox daemon` — OS-native gateway daemon/service management.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;
use savfox_gateway_server::{GatewaySubcommand, gateway_cli};

use crate::completion_support;

/// Manage the gateway as an OS-native daemon/service.
#[derive(Debug, Parser)]
pub struct DaemonCommand {
    #[clap(subcommand)]
    pub action: DaemonAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum DaemonAction {
    /// Install the gateway as a user service.
    Install {
        /// Service/task name.
        #[clap(long, default_value = "savfox-gateway")]
        name: String,
        /// Runtime override (`auto`, `systemd`, `launchd`, `windows-task`).
        #[clap(long, default_value = "auto")]
        runtime: String,
    },
    /// Uninstall the gateway user service.
    Uninstall {
        /// Service/task name.
        #[clap(long, default_value = "savfox-gateway")]
        name: String,
        /// Runtime override (`auto`, `systemd`, `launchd`, `windows-task`).
        #[clap(long, default_value = "auto")]
        runtime: String,
    },
    /// Start the background gateway daemon.
    Start {
        /// Host to bind.
        #[clap(long, default_value = "127.0.0.1")]
        host: IpAddr,
        /// Port to bind.
        #[clap(long, default_value_t = savfox_gateway_server::config::DEFAULT_PORT)]
        port: u16,
        /// PID file path.
        #[clap(long)]
        pid_file: Option<PathBuf>,
    },
    /// Stop the background gateway daemon.
    Stop {
        /// PID file path.
        #[clap(long)]
        pid_file: Option<PathBuf>,
    },
    /// Restart the background gateway daemon.
    Restart {
        /// Host to bind.
        #[clap(long, default_value = "127.0.0.1")]
        host: IpAddr,
        /// Port to bind.
        #[clap(long, default_value_t = savfox_gateway_server::config::DEFAULT_PORT)]
        port: u16,
        /// PID file path.
        #[clap(long)]
        pid_file: Option<PathBuf>,
    },
    /// Show gateway daemon status.
    Status {
        /// Gateway URL (for health/status probing).
        #[clap(long, default_value = "http://127.0.0.1:18881")]
        url: String,
    },
    /// Show gateway daemon logs.
    Logs {
        /// Gateway URL.
        #[clap(long, default_value = "http://127.0.0.1:18881")]
        url: String,
        /// Number of lines.
        #[clap(long, default_value_t = 100)]
        lines: usize,
        /// Follow logs in real-time.
        #[clap(long)]
        follow: bool,
    },
}

pub async fn run(cmd: DaemonCommand) -> Result<(), Box<dyn std::error::Error>> {
    let mut auto_install_completion = false;
    let subcommand = match cmd.action {
        DaemonAction::Install { name, runtime } => {
            auto_install_completion = true;
            if runtime != "auto" {
                eprintln!("Using runtime override: {runtime}");
            }
            GatewaySubcommand::Install { name, runtime }
        }
        DaemonAction::Uninstall { name, runtime } => GatewaySubcommand::Uninstall { name, runtime },
        DaemonAction::Start {
            host,
            port,
            pid_file,
        } => GatewaySubcommand::Start {
            host,
            port,
            pid_file,
        },
        DaemonAction::Stop { pid_file } => GatewaySubcommand::Stop { pid_file },
        DaemonAction::Restart {
            host,
            port,
            pid_file,
        } => GatewaySubcommand::Restart {
            host,
            port,
            pid_file,
        },
        DaemonAction::Status { url } => GatewaySubcommand::Status { url },
        DaemonAction::Logs { url, lines, follow } => GatewaySubcommand::Logs { url, lines, follow },
    };

    gateway_cli::run_subcommand(subcommand).await?;

    if auto_install_completion {
        match completion_support::install_for_current_shell() {
            Ok(path) => {
                eprintln!("Installed shell completion to {}", path.display());
            }
            Err(err) => {
                eprintln!("warning: failed to install shell completion automatically: {err}");
            }
        }
    }
    Ok(())
}
