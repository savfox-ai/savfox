#![allow(unreachable_pub)]

use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{Args, Parser};
use clap_complete::Shell;
use savfox_arg0::arg0_dispatch_or_else;
use savfox_chatgpt::apply_command::{ApplyCommand, run_apply_command};
use savfox_cli::login::{
    read_api_key_from_stdin, run_login_status, run_login_with_api_key, run_login_with_chatgpt,
    run_login_with_device_code, run_logout,
};
use savfox_cli::{LandlockCommand, SeatbeltCommand, WindowsCommand};
use savfox_cloud_tasks::Cli as CloudTasksCli;
use savfox_common::CliConfigOverrides;
use savfox_exec::{Cli as ExecCli, Command as ExecCommand, ReviewArgs};
use savfox_exec_policy::ExecPolicyCheckCommand;
use savfox_gateway_server::GatewayCommand;
use savfox_responses_api_proxy::Args as ResponsesApiProxyArgs;
use savfox_tui::update_action::UpdateAction;
use savfox_tui::{AppExitInfo, Cli as TuiCli, ExitReason};
use supports_color::Stream;

mod acp_bridge;
mod agents_cmd;
#[cfg(target_os = "macos")]
mod app_cmd;
mod completion_support;
mod config_cmd;
mod cron_cmd;
mod daemon_cmd;
mod dashboard;
#[cfg(target_os = "macos")]
mod desktop_app;
mod directory_cmd;
mod dns_cmd;
mod docker_cmd;
mod doctor;
mod mcp_cmd;
mod memory_cmd;
mod migrate;
mod plugins_cmd;
mod security_cmd;
mod send;
mod sessions_cmd;
mod skills_cmd;
mod status_cmd;
mod uninstall;
mod update_cmd;
mod wizard;
mod ws_rpc_client;
#[cfg(not(windows))]
mod wsl_paths;

use savfox_core::config::edit::ConfigEditsBuilder;
use savfox_core::config::{Config, ConfigOverrides, find_savfox_home};
use savfox_core::features::{Stage, is_known_feature_key};
use savfox_core::terminal::TerminalName;

use crate::doctor::DoctorCommand;
use crate::mcp_cmd::McpCli;
use crate::migrate::MigrateCommand;
use crate::send::SendCommand;
use crate::wizard::WizardCommand;

/// Savfox CLI
///
/// If no subcommand is specified, options will be forwarded to the interactive CLI.
#[derive(Debug, Parser)]
#[clap(
    author,
    version,
    // If a sub‑command is given, ignore requirements of the default args.
    subcommand_negates_reqs = true,
    // The executable is sometimes invoked via a platform‑specific name like
    // `savfox-x86_64-unknown-linux-musl`, but the help output should always use
    // the generic `savfox` command name that users run.
    bin_name = "savfox",
    override_usage = "savfox [OPTIONS] [PROMPT]\n       savfox [OPTIONS] <COMMAND> [ARGS]"
)]
struct MultitoolCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    pub feature_toggles: FeatureToggles,

    #[clap(flatten)]
    interactive: TuiCli,

    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    /// Run Savfox non-interactively.
    #[clap(visible_alias = "e")]
    Exec(ExecCli),

    /// Run a code review non-interactively.
    Review(ReviewArgs),

    /// Manage login.
    Login(LoginCommand),

    /// Remove stored authentication credentials.
    Logout(LogoutCommand),

    /// [experimental] Run Savfox as an MCP server and manage MCP servers.
    Mcp(McpCli),

    /// [experimental] Run the Savfox MCP server (stdio transport).
    McpServer,

    /// [experimental] Run the app server or related tooling.
    AppServer(AppServerCommand),

    /// [experimental] Run the gateway server for remote WebSocket/HTTP access.
    Gateway(GatewayCommand),

    /// [experimental] Run ACP bridge over stdio.
    Acp(acp_bridge::AcpCommand),

    /// Launch the Savfox desktop app (downloads the macOS installer if missing).
    #[cfg(target_os = "macos")]
    App(app_cmd::AppCommand),

    /// Generate shell completion scripts.
    Completion(CompletionCommand),

    /// Run commands within a Savfox-provided sandbox.
    #[clap(visible_alias = "debug")]
    Sandbox(SandboxArgs),

    /// Execpolicy tooling.
    #[clap(hide = true)]
    Execpolicy(ExecpolicyCommand),

    /// Apply the latest diff produced by Savfox agent as a `git apply` to your local working tree.
    #[clap(visible_alias = "a")]
    Apply(ApplyCommand),

    /// Resume a previous interactive session (picker by default; use --last to continue the most
    /// recent).
    Resume(ResumeCommand),

    /// Fork a previous interactive session (picker by default; use --last to fork the most recent).
    Fork(ForkCommand),

    /// [EXPERIMENTAL] Browse tasks from Savfox Cloud and apply changes locally.
    #[clap(name = "cloud", alias = "cloud-tasks")]
    Cloud(CloudTasksCli),

    /// Internal: run the responses API proxy.
    #[clap(hide = true)]
    ResponsesApiProxy(ResponsesApiProxyArgs),

    /// Internal: relay stdio to a Unix domain socket.
    #[clap(hide = true, name = "stdio-to-uds")]
    StdioToUds(StdioToUdsCommand),

    /// Inspect feature flags.
    Features(FeaturesCli),

    /// Diagnose system health and configuration.
    Doctor(DoctorCommand),

    /// Send a message to a chat channel via the gateway.
    Send(SendCommand),

    /// Interactive setup wizard.
    Wizard(WizardCommand),

    /// Migrate configuration from an OpenClaw (TypeScript) installation.
    Migrate(MigrateCommand),

    /// List, inspect, and export gateway sessions.
    Sessions(sessions_cmd::SessionsCommand),

    /// List, create, and manage agents.
    Agents(agents_cmd::AgentsCommand),

    /// Manage markdown memory entries on the gateway.
    Memory(memory_cmd::MemoryCommand),

    /// Manage installed skill toggles and state.
    Skills(skills_cmd::SkillsCommand),

    /// Manage plugins: list/install/update/uninstall and version pinning.
    Plugins(plugins_cmd::PluginsCommand),

    /// Manage gateway configuration.
    #[clap(name = "config")]
    GatewayConfig(config_cmd::ConfigCommand),

    /// Manage cron jobs on the gateway.
    Cron(cron_cmd::CronCommand),

    /// Manage gateway daemon/service lifecycle.
    Daemon(daemon_cmd::DaemonCommand),

    /// Generate Docker deployment templates.
    Docker(docker_cmd::DockerCommand),

    /// Configure DNS-SD/CoreDNS/Tailscale split DNS helpers.
    Dns(dns_cmd::DnsCommand),

    /// Open the gateway web dashboard in the browser.
    Dashboard(dashboard::DashboardCommand),

    /// Query directory service for peers, groups, and identity info.
    Directory(directory_cmd::DirectoryCommand),

    /// Run security audit and secret rotation commands.
    Security(security_cmd::SecurityCommand),

    /// Check the status of a running gateway instance.
    Status(status_cmd::StatusCommand),

    /// Remove Savfox data, configuration, and installed skills.
    Uninstall(uninstall::UninstallCommand),

    /// Check for and install CLI updates from GitHub releases.
    Update(update_cmd::UpdateCommand),
}

#[derive(Debug, Parser)]
struct CompletionCommand {
    /// Shell to generate completions for.
    #[clap(value_enum)]
    shell: Option<Shell>,
    /// Install completion file to the default location for the selected shell.
    #[arg(long, default_value_t = false)]
    install: bool,
    /// Print dynamic values for completion scripts (internal use).
    #[arg(long, value_enum, hide = true)]
    dynamic_kind: Option<completion_support::DynamicCompletionKind>,
    /// Force refresh of completion cache before printing dynamic values.
    #[arg(long, default_value_t = false, hide = true)]
    refresh_cache: bool,
}

#[derive(Debug, Parser)]
struct ResumeCommand {
    /// Conversation/session id (UUID) or session name. UUIDs take precedence if it parses.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Continue the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false)]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[clap(flatten)]
    config_overrides: TuiCli,
}

#[derive(Debug, Parser)]
struct ForkCommand {
    /// Conversation/session id (UUID). When provided, forks this session.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Fork the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false, conflicts_with = "session_id")]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[clap(flatten)]
    config_overrides: TuiCli,
}

#[derive(Debug, Parser)]
struct SandboxArgs {
    #[command(subcommand)]
    cmd: SandboxCommand,
}

#[derive(Debug, clap::Subcommand)]
enum SandboxCommand {
    /// Run a command under Seatbelt (macOS only).
    #[clap(visible_alias = "seatbelt")]
    Macos(SeatbeltCommand),

    /// Run a command under Landlock+seccomp (Linux only).
    #[clap(visible_alias = "landlock")]
    Linux(LandlockCommand),

    /// Run a command under Windows restricted token (Windows only).
    Windows(WindowsCommand),
}

#[derive(Debug, Parser)]
struct ExecpolicyCommand {
    #[command(subcommand)]
    sub: ExecpolicySubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ExecpolicySubcommand {
    /// Check execpolicy files against a command.
    #[clap(name = "check")]
    Check(ExecPolicyCheckCommand),
}

#[derive(Debug, Parser)]
struct LoginCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[arg(
        long = "with-api-key",
        help = "Read the API key from stdin (e.g. `printenv OPENAI_API_KEY | savfox login --with-api-key`)"
    )]
    with_api_key: bool,

    #[arg(
        long = "api-key",
        value_name = "API_KEY",
        help = "(deprecated) Previously accepted the API key directly; now exits with guidance to use --with-api-key",
        hide = true
    )]
    api_key: Option<String>,

    #[arg(long = "device-auth")]
    use_device_code: bool,

    /// EXPERIMENTAL: Use custom OAuth issuer base URL (advanced)
    /// Override the OAuth issuer base URL (advanced)
    #[arg(long = "experimental_issuer", value_name = "URL", hide = true)]
    issuer_base_url: Option<String>,

    /// EXPERIMENTAL: Use custom OAuth client ID (advanced)
    #[arg(long = "experimental_client-id", value_name = "CLIENT_ID", hide = true)]
    client_id: Option<String>,

    #[command(subcommand)]
    action: Option<LoginSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum LoginSubcommand {
    /// Show login status.
    Status,
}

#[derive(Debug, Parser)]
struct LogoutCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,
}

#[derive(Debug, Parser)]
struct AppServerCommand {
    /// Omit to run the app server; specify a subcommand for tooling.
    #[command(subcommand)]
    subcommand: Option<AppServerSubcommand>,

    /// Controls whether analytics are enabled by default.
    ///
    /// Analytics are disabled by default for app-server. Users have to explicitly opt in
    /// via the `analytics` section in the config.toml file.
    ///
    /// However, for first-party use cases like the VSCode IDE extension, we default analytics
    /// to be enabled by default by setting this flag. Users can still opt out by setting this
    /// in their config.toml:
    ///
    /// ```toml
    /// [analytics]
    /// enabled = false
    /// ```
    ///
    /// See https://developers.openai.com/savfox/config-advanced/#metrics for more details.
    #[arg(long = "analytics-default-enabled")]
    analytics_default_enabled: bool,
}

#[derive(Debug, clap::Subcommand)]
enum AppServerSubcommand {
    /// [experimental] Generate TypeScript bindings for the app server protocol.
    GenerateTs(GenerateTsCommand),

    /// [experimental] Generate JSON Schema for the app server protocol.
    GenerateJsonSchema(GenerateJsonSchemaCommand),
}

#[derive(Debug, Args)]
struct GenerateTsCommand {
    /// Output directory where .ts files will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Optional path to the Prettier executable to format generated files
    #[arg(short = 'p', long = "prettier", value_name = "PRETTIER_BIN")]
    prettier: Option<PathBuf>,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateJsonSchemaCommand {
    /// Output directory where the schema bundle will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Parser)]
struct StdioToUdsCommand {
    /// Path to the Unix domain socket to connect to.
    #[arg(value_name = "SOCKET_PATH")]
    socket_path: PathBuf,
}

fn boxed_summary_lines(lines: &[String]) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }
    let inner_width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let border = format!("╭{}╮", "─".repeat(inner_width + 2));
    let mut out = Vec::with_capacity(lines.len() + 2);
    out.push(border);
    for line in lines {
        let padding = inner_width.saturating_sub(line.chars().count());
        out.push(format!("│ {line}{} │", " ".repeat(padding)));
    }
    out.push(format!("╰{}╯", "─".repeat(inner_width + 2)));
    out
}

fn format_exit_messages(exit_info: AppExitInfo, _color_enabled: bool) -> Vec<String> {
    let AppExitInfo {
        token_usage,
        session_id: conversation_id,
        session_name,
        model_display,
        directory,
        ..
    } = exit_info;

    let mut lines = Vec::new();
    if !token_usage.is_zero() {
        lines.push(format!(
            "{}",
            savfox_core::protocol::FinalOutput::from(token_usage)
        ));
    }

    let mut summary_rows = Vec::new();
    if let Some(model) = model_display {
        summary_rows.push(format!("{:<11}{model}", "model:"));
    }
    if let Some(dir) = directory {
        summary_rows.push(format!("{:<11}{}", "directory:", dir.display()));
    }
    if let Some(resume_cmd) = resume_command_for_exit(session_name.as_deref(), conversation_id) {
        summary_rows.push(format!("{:<11}{resume_cmd}", "resume:"));
    }
    if !summary_rows.is_empty() {
        let mut boxed_rows = Vec::with_capacity(summary_rows.len() + 2);
        boxed_rows.push(format!(">_ Savfox  (v{})", env!("CARGO_PKG_VERSION")));
        boxed_rows.push(String::new());
        boxed_rows.extend(summary_rows);
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(boxed_summary_lines(&boxed_rows));
    }

    lines
}

fn resume_command_for_exit(
    session_name: Option<&str>,
    session_id: Option<savfox_protocol::SessionId>,
) -> Option<String> {
    let session_name = session_name.map(str::trim).filter(|name| !name.is_empty());
    if let Some(id) = session_id {
        savfox_core::util::resume_command(None, Some(id))
    } else {
        savfox_core::util::resume_command(session_name, None)
    }
}

/// Handle the app exit and print the results. Optionally run the update action.
fn handle_app_exit(exit_info: AppExitInfo) -> anyhow::Result<()> {
    match exit_info.exit_reason {
        ExitReason::Fatal(message) => {
            eprintln!("ERROR: {message}");
            std::process::exit(1);
        }
        ExitReason::UserRequested => { /* normal exit */ }
    }

    let update_action = exit_info.update_action;
    let color_enabled = supports_color::on(Stream::Stdout).is_some();
    for line in format_exit_messages(exit_info, color_enabled) {
        println!("{line}");
    }
    if let Some(action) = update_action {
        run_update_action(action)?;
    }
    Ok(())
}

/// Run the update action and print the result.
fn run_update_action(action: UpdateAction) -> anyhow::Result<()> {
    println!();
    let cmd_str = action.command_str();
    println!("Updating Savfox via `{cmd_str}`...");

    let status = {
        #[cfg(windows)]
        {
            // On Windows, run via cmd.exe so .CMD/.BAT are correctly resolved (PATHEXT semantics).
            std::process::Command::new("cmd")
                .args(["/C", &cmd_str])
                .status()?
        }
        #[cfg(not(windows))]
        {
            let (cmd, args) = action.command_args();
            let command_path = crate::wsl_paths::normalize_for_wsl(cmd);
            let normalized_args: Vec<String> = args
                .iter()
                .map(crate::wsl_paths::normalize_for_wsl)
                .collect();
            std::process::Command::new(&command_path)
                .args(&normalized_args)
                .status()?
        }
    };
    if !status.success() {
        anyhow::bail!("`{cmd_str}` failed with status {status}");
    }
    println!("\n🎉 Update ran successfully! Please restart Savfox.");
    Ok(())
}

fn run_execpolicycheck(cmd: ExecPolicyCheckCommand) -> anyhow::Result<()> {
    cmd.run()
}

#[derive(Debug, Default, Parser, Clone)]
struct FeatureToggles {
    /// Enable a feature (repeatable). Equivalent to `-c features.<name>=true`.
    #[arg(long = "enable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    enable: Vec<String>,

    /// Disable a feature (repeatable). Equivalent to `-c features.<name>=false`.
    #[arg(long = "disable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    disable: Vec<String>,
}

impl FeatureToggles {
    fn to_overrides(&self) -> anyhow::Result<Vec<String>> {
        let mut v = Vec::new();
        for feature in &self.enable {
            Self::validate_feature(feature)?;
            v.push(format!("features.{feature}=true"));
        }
        for feature in &self.disable {
            Self::validate_feature(feature)?;
            v.push(format!("features.{feature}=false"));
        }
        Ok(v)
    }

    fn validate_feature(feature: &str) -> anyhow::Result<()> {
        if is_known_feature_key(feature) {
            Ok(())
        } else {
            anyhow::bail!("Unknown feature flag: {feature}")
        }
    }
}

#[derive(Debug, Parser)]
struct FeaturesCli {
    #[command(subcommand)]
    sub: FeaturesSubcommand,
}

#[derive(Debug, Parser)]
enum FeaturesSubcommand {
    /// List known features with their stage and effective state.
    List,
    /// Enable a feature in config.toml.
    Enable(FeatureSetArgs),
    /// Disable a feature in config.toml.
    Disable(FeatureSetArgs),
}

#[derive(Debug, Parser)]
struct FeatureSetArgs {
    /// Feature key to update (for example: unified_exec).
    feature: String,
}

fn stage_str(stage: savfox_core::features::Stage) -> &'static str {
    use savfox_core::features::Stage;
    match stage {
        Stage::UnderDevelopment => "under development",
        Stage::Experimental { .. } => "experimental",
        Stage::Stable => "stable",
        Stage::Deprecated => "deprecated",
        Stage::Removed => "removed",
    }
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|savfox_linux_sandbox_exe| async move {
        cli_main(savfox_linux_sandbox_exe).await?;
        Ok(())
    })
}

async fn cli_main(savfox_linux_sandbox_exe: Option<PathBuf>) -> anyhow::Result<()> {
    let MultitoolCli {
        config_overrides: mut root_config_overrides,
        feature_toggles,
        mut interactive,
        subcommand,
    } = MultitoolCli::parse();

    // Fold --enable/--disable into config overrides so they flow to all subcommands.
    let toggle_overrides = feature_toggles.to_overrides()?;
    root_config_overrides.raw_overrides.extend(toggle_overrides);

    match subcommand {
        None => {
            // Interactive startup uses the TUI onboarding flow. Keep the CLI wizard
            // as an explicit command (`savfox wizard`) for full setup workflows.
            prepend_config_flags(
                &mut interactive.config_overrides,
                root_config_overrides.clone(),
            );
            let exit_info = run_interactive_tui(interactive, savfox_linux_sandbox_exe).await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Exec(mut exec_cli)) => {
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            savfox_exec::run_main(exec_cli, savfox_linux_sandbox_exe).await?;
        }
        Some(Subcommand::Review(review_args)) => {
            let mut exec_cli = ExecCli::try_parse_from(["savfox", "exec"])?;
            exec_cli.command = Some(ExecCommand::Review(review_args));
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            savfox_exec::run_main(exec_cli, savfox_linux_sandbox_exe).await?;
        }
        Some(Subcommand::McpServer) => {
            savfox_mcp_server::run_main(savfox_linux_sandbox_exe, root_config_overrides).await?;
        }
        Some(Subcommand::Mcp(mut mcp_cli)) => {
            // Propagate any root-level config overrides (e.g. `-c key=value`).
            prepend_config_flags(&mut mcp_cli.config_overrides, root_config_overrides.clone());
            mcp_cli.run().await?;
        }
        Some(Subcommand::AppServer(app_server_cli)) => match app_server_cli.subcommand {
            None => {
                savfox_app_server::run_main(
                    savfox_linux_sandbox_exe,
                    root_config_overrides,
                    savfox_core::config_loader::LoaderOverrides::default(),
                    app_server_cli.analytics_default_enabled,
                )
                .await?;
            }
            Some(AppServerSubcommand::GenerateTs(gen_cli)) => {
                let options = savfox_app_server_protocol::GenerateTsOptions {
                    experimental_api: gen_cli.experimental,
                    ..Default::default()
                };
                savfox_app_server_protocol::generate_ts_with_options(
                    &gen_cli.out_dir,
                    gen_cli.prettier.as_deref(),
                    options,
                )?;
            }
            Some(AppServerSubcommand::GenerateJsonSchema(gen_cli)) => {
                savfox_app_server_protocol::generate_json_with_experimental(
                    &gen_cli.out_dir,
                    gen_cli.experimental,
                )?;
            }
        },
        Some(Subcommand::Gateway(gateway_cli)) => {
            if let Some(subcmd) = gateway_cli.subcommand {
                // Run a CLI management subcommand against a running gateway.
                savfox_gateway_server::gateway_cli::run_subcommand(subcmd).await?;
            } else {
                // Start the gateway server.
                let gateway_config = gateway_cli.into_config();
                savfox_gateway_server::run_main(
                    gateway_config,
                    savfox_linux_sandbox_exe,
                    root_config_overrides,
                )
                .await?;
            }
        }
        Some(Subcommand::Acp(cmd)) => {
            acp_bridge::run(cmd).await?;
        }
        #[cfg(target_os = "macos")]
        Some(Subcommand::App(app_cli)) => {
            app_cmd::run_app(app_cli).await?;
        }
        Some(Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            config_overrides,
        })) => {
            interactive = finalize_resume_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                config_overrides,
            );
            let exit_info = run_interactive_tui(interactive, savfox_linux_sandbox_exe).await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            config_overrides,
        })) => {
            interactive = finalize_fork_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                config_overrides,
            );
            let exit_info = run_interactive_tui(interactive, savfox_linux_sandbox_exe).await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Login(mut login_cli)) => {
            prepend_config_flags(
                &mut login_cli.config_overrides,
                root_config_overrides.clone(),
            );
            match login_cli.action {
                Some(LoginSubcommand::Status) => {
                    run_login_status(login_cli.config_overrides).await;
                }
                None => {
                    if login_cli.use_device_code {
                        run_login_with_device_code(
                            login_cli.config_overrides,
                            login_cli.issuer_base_url,
                            login_cli.client_id,
                        )
                        .await;
                    } else if login_cli.api_key.is_some() {
                        eprintln!(
                            "The --api-key flag is no longer supported. Pipe the key instead, e.g. `printenv OPENAI_API_KEY | savfox login --with-api-key`."
                        );
                        std::process::exit(1);
                    } else if login_cli.with_api_key {
                        let api_key = read_api_key_from_stdin();
                        run_login_with_api_key(login_cli.config_overrides, api_key).await;
                    } else {
                        run_login_with_chatgpt(login_cli.config_overrides).await;
                    }
                }
            }
        }
        Some(Subcommand::Logout(mut logout_cli)) => {
            prepend_config_flags(
                &mut logout_cli.config_overrides,
                root_config_overrides.clone(),
            );
            run_logout(logout_cli.config_overrides).await;
        }
        Some(Subcommand::Completion(completion_cli)) => {
            if let Some(kind) = completion_cli.dynamic_kind {
                completion_support::print_dynamic_values(kind, completion_cli.refresh_cache)?;
            } else {
                let shell = completion_cli
                    .shell
                    .unwrap_or_else(completion_support::detect_shell_from_env);
                completion_support::output_completion(shell, completion_cli.install)?;
            }
        }
        Some(Subcommand::Cloud(mut cloud_cli)) => {
            prepend_config_flags(
                &mut cloud_cli.config_overrides,
                root_config_overrides.clone(),
            );
            savfox_cloud_tasks::run_main(cloud_cli, savfox_linux_sandbox_exe).await?;
        }
        Some(Subcommand::Sandbox(sandbox_args)) => match sandbox_args.cmd {
            SandboxCommand::Macos(mut seatbelt_cli) => {
                prepend_config_flags(
                    &mut seatbelt_cli.config_overrides,
                    root_config_overrides.clone(),
                );
                savfox_cli::debug_sandbox::run_command_under_seatbelt(
                    seatbelt_cli,
                    savfox_linux_sandbox_exe,
                )
                .await?;
            }
            SandboxCommand::Linux(mut landlock_cli) => {
                prepend_config_flags(
                    &mut landlock_cli.config_overrides,
                    root_config_overrides.clone(),
                );
                savfox_cli::debug_sandbox::run_command_under_landlock(
                    landlock_cli,
                    savfox_linux_sandbox_exe,
                )
                .await?;
            }
            SandboxCommand::Windows(mut windows_cli) => {
                prepend_config_flags(
                    &mut windows_cli.config_overrides,
                    root_config_overrides.clone(),
                );
                savfox_cli::debug_sandbox::run_command_under_windows(
                    windows_cli,
                    savfox_linux_sandbox_exe,
                )
                .await?;
            }
        },
        Some(Subcommand::Execpolicy(ExecpolicyCommand { sub })) => match sub {
            ExecpolicySubcommand::Check(cmd) => run_execpolicycheck(cmd)?,
        },
        Some(Subcommand::Apply(mut apply_cli)) => {
            prepend_config_flags(
                &mut apply_cli.config_overrides,
                root_config_overrides.clone(),
            );
            run_apply_command(apply_cli, None).await?;
        }
        Some(Subcommand::ResponsesApiProxy(args)) => {
            tokio::task::spawn_blocking(move || savfox_responses_api_proxy::run_main(args))
                .await??;
        }
        Some(Subcommand::StdioToUds(cmd)) => {
            let socket_path = cmd.socket_path;
            tokio::task::spawn_blocking(move || savfox_stdio_to_uds::run(socket_path.as_path()))
                .await??;
        }
        Some(Subcommand::Doctor(cmd)) => {
            doctor::run_doctor(cmd).await?;
        }
        Some(Subcommand::Send(cmd)) => {
            send::run_send(cmd).await?;
        }
        Some(Subcommand::Wizard(cmd)) => {
            wizard::run_wizard(cmd).await?;
        }
        Some(Subcommand::Migrate(cmd)) => {
            migrate::run_migrate(cmd).await?;
        }
        Some(Subcommand::Sessions(cmd)) => {
            let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:18881".to_string());
            let token = std::env::var("SAVFOX_GATEWAY_TOKEN").unwrap_or_default();
            sessions_cmd::run(cmd, &gateway_url, &token)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Agents(cmd)) => {
            let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:18881".to_string());
            let token = std::env::var("SAVFOX_GATEWAY_TOKEN").unwrap_or_default();
            agents_cmd::run(cmd, &gateway_url, &token)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Memory(cmd)) => {
            memory_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Skills(cmd)) => {
            skills_cmd::run(cmd)?;
        }
        Some(Subcommand::Plugins(cmd)) => {
            plugins_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::GatewayConfig(cmd)) => {
            config_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Cron(cmd)) => {
            cron_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Daemon(cmd)) => {
            daemon_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Docker(cmd)) => {
            docker_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Dns(cmd)) => {
            dns_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Dashboard(cmd)) => {
            dashboard::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Directory(cmd)) => {
            directory_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Security(cmd)) => {
            security_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Status(cmd)) => {
            status_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Uninstall(cmd)) => {
            uninstall::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Update(cmd)) => {
            update_cmd::run(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Some(Subcommand::Features(FeaturesCli { sub })) => match sub {
            FeaturesSubcommand::List => {
                // Respect root-level `-c` overrides plus top-level flags like `--profile`.
                let mut cli_kv_overrides = root_config_overrides
                    .parse_overrides()
                    .map_err(anyhow::Error::msg)?;

                // Honor `--search` via the canonical web_search mode.
                if interactive.web_search {
                    cli_kv_overrides.push((
                        "web_search".to_string(),
                        toml::Value::String("live".to_string()),
                    ));
                }

                // Session through relevant top-level flags (at minimum, `--profile`).
                let overrides = ConfigOverrides {
                    config_profile: interactive.config_profile.clone(),
                    ..Default::default()
                };

                let config = Config::load_with_cli_overrides_and_harness_overrides(
                    cli_kv_overrides,
                    overrides,
                )
                .await?;
                let mut rows = Vec::with_capacity(savfox_core::features::FEATURES.len());
                let mut name_width = 0;
                let mut stage_width = 0;
                for def in savfox_core::features::FEATURES.iter() {
                    let name = def.key;
                    let stage = stage_str(def.stage);
                    let enabled = config.features.enabled(def.id);
                    name_width = name_width.max(name.len());
                    stage_width = stage_width.max(stage.len());
                    rows.push((name, stage, enabled));
                }

                for (name, stage, enabled) in rows {
                    println!("{name:<name_width$}  {stage:<stage_width$}  {enabled}");
                }
            }
            FeaturesSubcommand::Enable(FeatureSetArgs { feature }) => {
                enable_feature_in_config(&interactive, &feature).await?;
            }
            FeaturesSubcommand::Disable(FeatureSetArgs { feature }) => {
                disable_feature_in_config(&interactive, &feature).await?;
            }
        },
    }

    Ok(())
}

async fn enable_feature_in_config(interactive: &TuiCli, feature: &str) -> anyhow::Result<()> {
    FeatureToggles::validate_feature(feature)?;
    let savfox_home = find_savfox_home()?;
    ConfigEditsBuilder::new(&savfox_home)
        .with_profile(interactive.config_profile.as_deref())
        .set_feature_enabled(feature, true)
        .apply()
        .await?;
    println!("Enabled feature `{feature}` in config.toml.");
    maybe_print_under_development_feature_warning(&savfox_home, interactive, feature);
    Ok(())
}

async fn disable_feature_in_config(interactive: &TuiCli, feature: &str) -> anyhow::Result<()> {
    FeatureToggles::validate_feature(feature)?;
    let savfox_home = find_savfox_home()?;
    ConfigEditsBuilder::new(&savfox_home)
        .set_feature_enabled(feature, false)
        .apply()
        .await?;
    println!("Disabled feature `{feature}` in config.toml.");
    Ok(())
}

fn maybe_print_under_development_feature_warning(
    savfox_home: &std::path::Path,
    interactive: &TuiCli,
    feature: &str,
) {
    let Some(spec) = savfox_core::features::FEATURES
        .iter()
        .find(|spec| spec.key == feature)
    else {
        return;
    };
    if !matches!(spec.stage, Stage::UnderDevelopment) {
        return;
    }

    let config_path = savfox_home.join(savfox_core::config::CONFIG_TOML_FILE);
    eprintln!(
        "Under-development features enabled: {feature}. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in {}.",
        config_path.display()
    );
}

/// Prepend root-level overrides so they have lower precedence than
/// CLI-specific ones specified after the subcommand (if any).
fn prepend_config_flags(
    subcommand_config_overrides: &mut CliConfigOverrides,
    cli_config_overrides: CliConfigOverrides,
) {
    subcommand_config_overrides
        .raw_overrides
        .splice(0..0, cli_config_overrides.raw_overrides);
}

async fn run_interactive_tui(
    mut interactive: TuiCli,
    savfox_linux_sandbox_exe: Option<PathBuf>,
) -> std::io::Result<AppExitInfo> {
    if let Some(prompt) = interactive.prompt.take() {
        // Normalize CRLF/CR to LF so CLI-provided text can't leak `\r` into TUI state.
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    let terminal_info = savfox_core::terminal::terminal_info();
    if terminal_info.name == TerminalName::Dumb {
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return Ok(AppExitInfo::fatal(
                "TERM is set to \"dumb\". Refusing to start the interactive TUI because no terminal is available for a confirmation prompt (stdin/stderr is not a TTY). Run in a supported terminal or unset TERM.",
            ));
        }

        eprintln!(
            "WARNING: TERM is set to \"dumb\". Savfox's interactive TUI may not work in this terminal."
        );
        if !confirm("Continue anyway? [y/N]: ")? {
            return Ok(AppExitInfo::fatal(
                "Refusing to start the interactive TUI because TERM is set to \"dumb\". Run in a supported terminal or unset TERM.",
            ));
        }
    }

    savfox_tui::run_main(interactive, savfox_linux_sandbox_exe).await
}

fn confirm(prompt: &str) -> std::io::Result<bool> {
    eprintln!("{prompt}");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Build the final `TuiCli` for a `savfox resume` invocation.
fn finalize_resume_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    resume_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so resume shares the same
    // configuration surface area as `savfox` without additional flags.
    let resume_session_id = session_id;
    interactive.resume_picker = resume_session_id.is_none() && !last;
    interactive.resume_last = last;
    interactive.resume_session_id = resume_session_id;
    interactive.resume_show_all = show_all;

    // Merge resume-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, resume_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

/// Build the final `TuiCli` for a `savfox fork` invocation.
fn finalize_fork_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    fork_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so fork shares the same
    // configuration surface area as `savfox` without additional flags.
    let fork_session_id = session_id;
    interactive.fork_picker = fork_session_id.is_none() && !last;
    interactive.fork_last = last;
    interactive.fork_session_id = fork_session_id;
    interactive.fork_show_all = show_all;

    // Merge fork-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, fork_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

/// Merge flags provided to `savfox resume`/`savfox fork` so they take precedence over any
/// root-level flags. Only overrides fields explicitly set on the subcommand-scoped
/// CLI. Also appends `-c key=value` overrides with highest precedence.
fn merge_interactive_cli_flags(interactive: &mut TuiCli, subcommand_cli: TuiCli) {
    if let Some(model) = subcommand_cli.model {
        interactive.model = Some(model);
    }
    if subcommand_cli.oss {
        interactive.oss = true;
    }
    if let Some(sandbox) = subcommand_cli.sandbox_mode {
        interactive.sandbox_mode = Some(sandbox);
    }
    if let Some(approval) = subcommand_cli.approval_policy {
        interactive.approval_policy = Some(approval);
    }
    if subcommand_cli.full_auto {
        interactive.full_auto = true;
    }
    if subcommand_cli.dangerously_bypass_approvals_and_sandbox {
        interactive.dangerously_bypass_approvals_and_sandbox = true;
    }
    if let Some(cwd) = subcommand_cli.cwd {
        interactive.cwd = Some(cwd);
    }
    if subcommand_cli.web_search {
        interactive.web_search = true;
    }
    if !subcommand_cli.images.is_empty() {
        interactive.images = subcommand_cli.images;
    }
    if !subcommand_cli.add_dir.is_empty() {
        interactive.add_dir.extend(subcommand_cli.add_dir);
    }
    if let Some(prompt) = subcommand_cli.prompt {
        // Normalize CRLF/CR to LF so CLI-provided text can't leak `\r` into TUI state.
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    interactive
        .config_overrides
        .raw_overrides
        .extend(subcommand_cli.config_overrides.raw_overrides);
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use pretty_assertions::assert_eq;
    use savfox_core::protocol::TokenUsage;
    use savfox_protocol::SessionId;

    use super::*;

    fn finalize_resume_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            interactive,
            config_overrides: root_overrides,
            subcommand,
            feature_toggles: _,
        } = cli;

        let Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            config_overrides: resume_cli,
        }) = subcommand.expect("resume present")
        else {
            unreachable!()
        };

        finalize_resume_interactive(
            interactive,
            root_overrides,
            session_id,
            last,
            all,
            resume_cli,
        )
    }

    fn finalize_fork_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            interactive,
            config_overrides: root_overrides,
            subcommand,
            feature_toggles: _,
        } = cli;

        let Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            config_overrides: fork_cli,
        }) = subcommand.expect("fork present")
        else {
            unreachable!()
        };

        finalize_fork_interactive(interactive, root_overrides, session_id, last, all, fork_cli)
    }

    #[test]
    fn exec_resume_last_accepts_prompt_positional() {
        let cli =
            MultitoolCli::try_parse_from(["savfox", "exec", "--json", "resume", "--last", "2+2"])
                .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(savfox_exec::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert!(args.last);
        assert_eq!(args.session_id, None);
        assert_eq!(args.prompt.as_deref(), Some("2+2"));
    }

    fn app_server_from_args(args: &[&str]) -> AppServerCommand {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let Subcommand::AppServer(app_server) = cli.subcommand.expect("app-server present") else {
            unreachable!()
        };
        app_server
    }

    fn sample_exit_info(conversation_id: Option<&str>, session_name: Option<&str>) -> AppExitInfo {
        let token_usage = TokenUsage {
            output_tokens: 2,
            total_tokens: 2,
            ..Default::default()
        };
        AppExitInfo {
            token_usage,
            session_id: conversation_id
                .map(SessionId::from_string)
                .map(Result::unwrap),
            session_name: session_name.map(str::to_string),
            model_display: Some("gpt-5.3-codex xhigh".to_string()),
            directory: Some(PathBuf::from("workspace")),
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        }
    }

    #[test]
    fn format_exit_messages_skips_zero_usage() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            session_id: None,
            session_name: None,
            model_display: None,
            directory: None,
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        };
        let lines = format_exit_messages(exit_info, false);
        assert!(lines.is_empty());
    }

    #[test]
    fn format_exit_messages_includes_resume_hint_without_color() {
        let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        let lines = format_exit_messages(exit_info, false);
        let mut expected = vec![
            "Token usage: total=2 input=0 output=2".to_string(),
            String::new(),
        ];
        expected.extend(boxed_summary_lines(&[
            format!(">_ Savfox  (v{})", env!("CARGO_PKG_VERSION")),
            String::new(),
            "model:     gpt-5.3-codex xhigh".to_string(),
            "directory: workspace".to_string(),
            "resume:    savfox resume 123e4567-e89b-12d3-a456-426614174000".to_string(),
        ]));
        assert_eq!(lines, expected);
    }

    #[test]
    fn format_exit_messages_is_stable_when_color_enabled() {
        let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        let no_color = format_exit_messages(exit_info.clone(), false);
        let with_color = format_exit_messages(exit_info, true);
        assert_eq!(with_color, no_color);
    }

    #[test]
    fn format_exit_messages_continues_with_id_when_name_present() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            Some("my-session"),
        );
        let lines = format_exit_messages(exit_info, false);
        let mut expected = vec![
            "Token usage: total=2 input=0 output=2".to_string(),
            String::new(),
        ];
        expected.extend(boxed_summary_lines(&[
            format!(">_ Savfox  (v{})", env!("CARGO_PKG_VERSION")),
            String::new(),
            "model:     gpt-5.3-codex xhigh".to_string(),
            "directory: workspace".to_string(),
            "resume:    savfox resume 123e4567-e89b-12d3-a456-426614174000".to_string(),
        ]));
        assert_eq!(lines, expected);
    }

    #[test]
    fn format_exit_messages_includes_restore_hint_with_zero_usage() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            session_id: Some(
                SessionId::from_string("123e4567-e89b-12d3-a456-426614174000")
                    .expect("valid session id"),
            ),
            session_name: Some("my-session".to_string()),
            model_display: None,
            directory: None,
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        };
        let lines = format_exit_messages(exit_info, false);
        let expected = boxed_summary_lines(&[
            format!(">_ Savfox  (v{})", env!("CARGO_PKG_VERSION")),
            String::new(),
            "resume:    savfox resume 123e4567-e89b-12d3-a456-426614174000".to_string(),
        ]);
        assert_eq!(lines, expected);
    }

    #[test]
    fn resume_model_flag_applies_when_no_root_flags() {
        let interactive =
            finalize_resume_from_args(["savfox", "resume", "-m", "gpt-5.1-test"].as_ref());

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    #[test]
    fn resume_picker_logic_none_and_not_last() {
        let interactive = finalize_resume_from_args(["savfox", "resume"].as_ref());
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_picker_logic_last() {
        let interactive = finalize_resume_from_args(["savfox", "resume", "--last"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_picker_logic_with_session_id() {
        let interactive = finalize_resume_from_args(["savfox", "resume", "1234"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_all_flag_sets_show_all() {
        let interactive = finalize_resume_from_args(["savfox", "resume", "--all"].as_ref());
        assert!(interactive.resume_picker);
        assert!(interactive.resume_show_all);
    }

    #[test]
    fn resume_merges_option_flags_and_full_auto() {
        let interactive = finalize_resume_from_args(
            [
                "savfox",
                "resume",
                "sid",
                "--oss",
                "--full-auto",
                "--search",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
                "-m",
                "gpt-5.1-test",
                "-p",
                "my-profile",
                "-C",
                "/tmp",
                "-i",
                "/tmp/a.png,/tmp/b.png",
            ]
            .as_ref(),
        );

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.oss);
        assert_eq!(interactive.config_profile.as_deref(), Some("my-profile"));
        assert_matches!(
            interactive.sandbox_mode,
            Some(savfox_common::SandboxModeCliArg::WorkspaceWrite)
        );
        assert_matches!(
            interactive.approval_policy,
            Some(savfox_common::ApprovalModeCliArg::OnRequest)
        );
        assert!(interactive.full_auto);
        assert_eq!(
            interactive.cwd.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert!(interactive.web_search);
        let has_a = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/a.png"));
        let has_b = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/b.png"));
        assert!(has_a && has_b);
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("sid"));
    }

    #[test]
    fn resume_merges_dangerously_bypass_flag() {
        let interactive = finalize_resume_from_args(
            [
                "savfox",
                "resume",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
            .as_ref(),
        );
        assert!(interactive.dangerously_bypass_approvals_and_sandbox);
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    #[test]
    fn fork_picker_logic_none_and_not_last() {
        let interactive = finalize_fork_from_args(["savfox", "fork"].as_ref());
        assert!(interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_picker_logic_last() {
        let interactive = finalize_fork_from_args(["savfox", "fork", "--last"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_picker_logic_with_session_id() {
        let interactive = finalize_fork_from_args(["savfox", "fork", "1234"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_all_flag_sets_show_all() {
        let interactive = finalize_fork_from_args(["savfox", "fork", "--all"].as_ref());
        assert!(interactive.fork_picker);
        assert!(interactive.fork_show_all);
    }

    #[test]
    fn app_server_analytics_default_disabled_without_flag() {
        let app_server = app_server_from_args(["savfox", "app-server"].as_ref());
        assert!(!app_server.analytics_default_enabled);
    }

    #[test]
    fn app_server_analytics_default_enabled_with_flag() {
        let app_server =
            app_server_from_args(["savfox", "app-server", "--analytics-default-enabled"].as_ref());
        assert!(app_server.analytics_default_enabled);
    }

    #[test]
    fn features_enable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["savfox", "features", "enable", "unified_exec"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Enable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features enable");
        };
        assert_eq!(feature, "unified_exec");
    }

    #[test]
    fn features_disable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["savfox", "features", "disable", "shell_tool"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Disable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features disable");
        };
        assert_eq!(feature, "shell_tool");
    }

    #[test]
    fn feature_toggles_known_features_generate_overrides() {
        let toggles = FeatureToggles {
            enable: vec!["web_search_request".to_string()],
            disable: vec!["unified_exec".to_string()],
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec![
                "features.web_search_request=true".to_string(),
                "features.unified_exec=false".to_string(),
            ]
        );
    }

    #[test]
    fn feature_toggles_unknown_feature_errors() {
        let toggles = FeatureToggles {
            enable: vec!["does_not_exist".to_string()],
            disable: Vec::new(),
        };
        let err = toggles
            .to_overrides()
            .expect_err("feature should be rejected");
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }
}
