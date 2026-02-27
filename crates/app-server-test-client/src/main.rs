use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;
use std::{fs, thread};

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use savfox_app_server_protocol::{
    AddConversationListenerParams, AddConversationSubscriptionResponse, AskForApproval, ClientInfo,
    ClientRequest, CommandExecutionApprovalDecision, CommandExecutionRequestApprovalParams,
    CommandExecutionRequestApprovalResponse, DynamicToolSpec, FileChangeApprovalDecision,
    FileChangeRequestApprovalParams, FileChangeRequestApprovalResponse,
    GetAccountRateLimitsResponse, InitializeCapabilities, InitializeParams, InitializeResponse,
    InputItem, JSONRPCNotification, JSONRPCRequest, JSONRPCResponse, JsonRpcMessage,
    LoginChatGptCompleteNotification, LoginChatGptResponse, ModelListParams, ModelListResponse,
    NewConversationParams, NewConversationResponse, RequestId, SandboxPolicy,
    SendUserMessageParams, SendUserMessageResponse, ServerNotification, ServerRequest,
    SessionStartParams, SessionStartResponse, TurnStartParams, TurnStartResponse, TurnStatus,
    UserInput as V2UserInput,
};
use savfox_protocol::SessionId;
use savfox_protocol::protocol::{Event, EventMsg};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

/// Minimal launcher that initializes the Savfox app-server and logs the handshake.
#[derive(Parser)]
#[command(author = "Savfox", version, about = "Bootstrap Savfox app-server", long_about = None)]
struct Cli {
    /// Path to the `savfox` CLI binary.
    #[arg(long, env = "SAVFOX_BIN", default_value = "savfox")]
    savfox_bin: String,

    /// Forwarded to the `savfox` CLI as `--config key=value`. Repeatable.
    ///
    /// Example:
    ///   `--config 'model_providers.mock.base_url="http://localhost:4010/v2"'`
    #[arg(
        short = 'c',
        long = "config",
        value_name = "key=value",
        action = ArgAction::Append,
        global = true
    )]
    config_overrides: Vec<String>,

    /// JSON array of dynamic tool specs or a single tool object.
    /// Prefix a filename with '@' to read from a file.
    ///
    /// Example:
    ///   --dynamic-tools '[{"name":"demo","description":"Demo","inputSchema":{"type":"object"}}]'
    ///   --dynamic-tools @/path/to/tools.json
    #[arg(long, value_name = "json-or-@file", global = true)]
    dynamic_tools: Option<String>,

    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Send a user message through the Savfox app-server.
    SendMessage {
        /// User message to send to Savfox.
        #[arg()]
        user_message: String,
    },
    /// Send a user message through the app-server V2 session/turn APIs.
    SendMessageV2 {
        /// User message to send to Savfox.
        #[arg()]
        user_message: String,
    },
    /// Start a V2 turn that elicits an ExecCommand approval.
    #[command(name = "trigger-cmd-approval")]
    TriggerCmdApproval {
        /// Optional prompt; defaults to a simple python command.
        #[arg()]
        user_message: Option<String>,
    },
    /// Start a V2 turn that elicits an ApplyPatch approval.
    #[command(name = "trigger-patch-approval")]
    TriggerPatchApproval {
        /// Optional prompt; defaults to creating a file via apply_patch.
        #[arg()]
        user_message: Option<String>,
    },
    /// Start a V2 turn that should not elicit an ExecCommand approval.
    #[command(name = "no-trigger-cmd-approval")]
    NoTriggerCmdApproval,
    /// Send two sequential V2 turns in the same session to test follow-up behavior.
    SendFollowUpV2 {
        /// Initial user message for the first turn.
        #[arg()]
        first_message: String,
        /// Follow-up user message for the second turn.
        #[arg()]
        follow_up_message: String,
    },
    /// Trigger the ChatGPT login flow and wait for completion.
    TestLogin,
    /// Fetch the current account rate limits from the Savfox app-server.
    GetAccountRateLimits,
    /// List the available models from the Savfox app-server.
    #[command(name = "model-list")]
    ModelList,
}

fn main() -> Result<()> {
    let Cli {
        savfox_bin,
        config_overrides,
        dynamic_tools,
        command,
    } = Cli::parse();

    let dynamic_tools = parse_dynamic_tools_arg(&dynamic_tools)?;

    match command {
        CliCommand::SendMessage { user_message } => {
            ensure_dynamic_tools_unused(&dynamic_tools, "send-message")?;
            send_message(&savfox_bin, &config_overrides, user_message)
        }
        CliCommand::SendMessageV2 { user_message } => {
            send_message_v2(&savfox_bin, &config_overrides, user_message, &dynamic_tools)
        }
        CliCommand::TriggerCmdApproval { user_message } => {
            trigger_cmd_approval(&savfox_bin, &config_overrides, user_message, &dynamic_tools)
        }
        CliCommand::TriggerPatchApproval { user_message } => {
            trigger_patch_approval(&savfox_bin, &config_overrides, user_message, &dynamic_tools)
        }
        CliCommand::NoTriggerCmdApproval => {
            no_trigger_cmd_approval(&savfox_bin, &config_overrides, &dynamic_tools)
        }
        CliCommand::SendFollowUpV2 {
            first_message,
            follow_up_message,
        } => send_follow_up_v2(
            &savfox_bin,
            &config_overrides,
            first_message,
            follow_up_message,
            &dynamic_tools,
        ),
        CliCommand::TestLogin => {
            ensure_dynamic_tools_unused(&dynamic_tools, "test-login")?;
            test_login(&savfox_bin, &config_overrides)
        }
        CliCommand::GetAccountRateLimits => {
            ensure_dynamic_tools_unused(&dynamic_tools, "get-account-rate-limits")?;
            get_account_rate_limits(&savfox_bin, &config_overrides)
        }
        CliCommand::ModelList => {
            ensure_dynamic_tools_unused(&dynamic_tools, "model-list")?;
            model_list(&savfox_bin, &config_overrides)
        }
    }
}

fn send_message(savfox_bin: &str, config_overrides: &[String], user_message: String) -> Result<()> {
    let mut client = SavfoxClient::spawn(savfox_bin, config_overrides)?;

    let initialize = client.initialize()?;
    println!("< initialize response: {initialize:?}");

    let conversation = client.start_session()?;
    println!("< newConversation response: {conversation:?}");

    let subscription = client.add_conversation_listener(&conversation.conversation_id)?;
    println!("< addConversationListener response: {subscription:?}");

    let send_response = client.send_user_message(&conversation.conversation_id, &user_message)?;
    println!("< sendUserMessage response: {send_response:?}");

    client.stream_conversation(&conversation.conversation_id)?;

    client.remove_session_listener(subscription.subscription_id)?;

    Ok(())
}

fn send_message_v2(
    savfox_bin: &str,
    config_overrides: &[String],
    user_message: String,
    dynamic_tools: &Option<Vec<DynamicToolSpec>>,
) -> Result<()> {
    send_message_v2_with_policies(
        savfox_bin,
        config_overrides,
        user_message,
        None,
        None,
        dynamic_tools,
    )
}

fn trigger_cmd_approval(
    savfox_bin: &str,
    config_overrides: &[String],
    user_message: Option<String>,
    dynamic_tools: &Option<Vec<DynamicToolSpec>>,
) -> Result<()> {
    let default_prompt =
        "Run `touch /tmp/should-trigger-approval` so I can confirm the file exists.";
    let message = user_message.unwrap_or_else(|| default_prompt.to_string());
    send_message_v2_with_policies(
        savfox_bin,
        config_overrides,
        message,
        Some(AskForApproval::OnRequest),
        Some(SandboxPolicy::ReadOnly),
        dynamic_tools,
    )
}

fn trigger_patch_approval(
    savfox_bin: &str,
    config_overrides: &[String],
    user_message: Option<String>,
    dynamic_tools: &Option<Vec<DynamicToolSpec>>,
) -> Result<()> {
    let default_prompt =
        "Create a file named APPROVAL_DEMO.txt containing a short hello message using apply_patch.";
    let message = user_message.unwrap_or_else(|| default_prompt.to_string());
    send_message_v2_with_policies(
        savfox_bin,
        config_overrides,
        message,
        Some(AskForApproval::OnRequest),
        Some(SandboxPolicy::ReadOnly),
        dynamic_tools,
    )
}

fn no_trigger_cmd_approval(
    savfox_bin: &str,
    config_overrides: &[String],
    dynamic_tools: &Option<Vec<DynamicToolSpec>>,
) -> Result<()> {
    let prompt = "Run `touch should_not_trigger_approval.txt`";
    send_message_v2_with_policies(
        savfox_bin,
        config_overrides,
        prompt.to_string(),
        None,
        None,
        dynamic_tools,
    )
}

fn send_message_v2_with_policies(
    savfox_bin: &str,
    config_overrides: &[String],
    user_message: String,
    approval_policy: Option<AskForApproval>,
    sandbox_policy: Option<SandboxPolicy>,
    dynamic_tools: &Option<Vec<DynamicToolSpec>>,
) -> Result<()> {
    let mut client = SavfoxClient::spawn(savfox_bin, config_overrides)?;

    let initialize = client.initialize()?;
    println!("< initialize response: {initialize:?}");

    let session_response = client.session_start(SessionStartParams {
        dynamic_tools: dynamic_tools.clone(),
        ..Default::default()
    })?;
    println!("< session/start response: {session_response:?}");
    let mut turn_params = TurnStartParams {
        session_id: session_response.session.id.clone(),
        input: vec![V2UserInput::Text {
            text: user_message,
            // Test client sends plain text without UI element ranges.
            text_elements: Vec::new(),
        }],
        ..Default::default()
    };
    turn_params.approval_policy = approval_policy;
    turn_params.sandbox_policy = sandbox_policy;

    let turn_response = client.turn_start(turn_params)?;
    println!("< turn/start response: {turn_response:?}");

    client.stream_turn(&session_response.session.id, &turn_response.turn.id)?;

    Ok(())
}

fn send_follow_up_v2(
    savfox_bin: &str,
    config_overrides: &[String],
    first_message: String,
    follow_up_message: String,
    dynamic_tools: &Option<Vec<DynamicToolSpec>>,
) -> Result<()> {
    let mut client = SavfoxClient::spawn(savfox_bin, config_overrides)?;

    let initialize = client.initialize()?;
    println!("< initialize response: {initialize:?}");

    let session_response = client.session_start(SessionStartParams {
        dynamic_tools: dynamic_tools.clone(),
        ..Default::default()
    })?;
    println!("< session/start response: {session_response:?}");

    let first_turn_params = TurnStartParams {
        session_id: session_response.session.id.clone(),
        input: vec![V2UserInput::Text {
            text: first_message,
            // Test client sends plain text without UI element ranges.
            text_elements: Vec::new(),
        }],
        ..Default::default()
    };
    let first_turn_response = client.turn_start(first_turn_params)?;
    println!("< turn/start response (initial): {first_turn_response:?}");
    client.stream_turn(&session_response.session.id, &first_turn_response.turn.id)?;

    let follow_up_params = TurnStartParams {
        session_id: session_response.session.id.clone(),
        input: vec![V2UserInput::Text {
            text: follow_up_message,
            // Test client sends plain text without UI element ranges.
            text_elements: Vec::new(),
        }],
        ..Default::default()
    };
    let follow_up_response = client.turn_start(follow_up_params)?;
    println!("< turn/start response (follow-up): {follow_up_response:?}");
    client.stream_turn(&session_response.session.id, &follow_up_response.turn.id)?;

    Ok(())
}

fn test_login(savfox_bin: &str, config_overrides: &[String]) -> Result<()> {
    let mut client = SavfoxClient::spawn(savfox_bin, config_overrides)?;

    let initialize = client.initialize()?;
    println!("< initialize response: {initialize:?}");

    let login_response = client.login_chat_gpt()?;
    println!("< loginChatGpt response: {login_response:?}");
    println!(
        "Open the following URL in your browser to continue:\n{}",
        login_response.auth_url
    );

    let completion = client.wait_for_login_completion(&login_response.login_id)?;
    println!("< loginChatGptComplete notification: {completion:?}");

    if completion.success {
        println!("Login succeeded.");
        Ok(())
    } else {
        bail!(
            "login failed: {}",
            completion
                .error
                .as_deref()
                .unwrap_or("unknown error from loginChatGptComplete")
        );
    }
}

fn get_account_rate_limits(savfox_bin: &str, config_overrides: &[String]) -> Result<()> {
    let mut client = SavfoxClient::spawn(savfox_bin, config_overrides)?;

    let initialize = client.initialize()?;
    println!("< initialize response: {initialize:?}");

    let response = client.get_account_rate_limits()?;
    println!("< account/rateLimits/read response: {response:?}");

    Ok(())
}

fn model_list(savfox_bin: &str, config_overrides: &[String]) -> Result<()> {
    let mut client = SavfoxClient::spawn(savfox_bin, config_overrides)?;

    let initialize = client.initialize()?;
    println!("< initialize response: {initialize:?}");

    let response = client.model_list(ModelListParams::default())?;
    println!("< model/list response: {response:?}");

    Ok(())
}

fn ensure_dynamic_tools_unused(
    dynamic_tools: &Option<Vec<DynamicToolSpec>>,
    command: &str,
) -> Result<()> {
    if dynamic_tools.is_some() {
        bail!(
            "dynamic tools are only supported for v2 session/start; remove --dynamic-tools for {command} or use send-message-v2"
        );
    }
    Ok(())
}

fn parse_dynamic_tools_arg(dynamic_tools: &Option<String>) -> Result<Option<Vec<DynamicToolSpec>>> {
    let Some(raw_arg) = dynamic_tools.as_deref() else {
        return Ok(None);
    };

    let raw_json = if let Some(path) = raw_arg.strip_prefix('@') {
        fs::read_to_string(Path::new(path))
            .with_context(|| format!("read dynamic tools file {path}"))?
    } else {
        raw_arg.to_string()
    };

    let value: Value = serde_json::from_str(&raw_json).context("parse dynamic tools JSON")?;
    let tools = match value {
        Value::Array(_) => serde_json::from_value(value).context("decode dynamic tools array")?,
        Value::Object(_) => vec![serde_json::from_value(value).context("decode dynamic tool")?],
        _ => bail!("dynamic tools JSON must be an object or array"),
    };

    Ok(Some(tools))
}

struct SavfoxClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    pending_notifications: VecDeque<JSONRPCNotification>,
}

impl SavfoxClient {
    fn spawn(savfox_bin: &str, config_overrides: &[String]) -> Result<Self> {
        let mut cmd = Command::new(savfox_bin);
        for override_kv in config_overrides {
            cmd.arg("--config").arg(override_kv);
        }
        let mut savfox_app_server = cmd
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to start `{savfox_bin}` app-server"))?;

        let stdin = savfox_app_server
            .stdin
            .take()
            .context("savfox app-server stdin unavailable")?;
        let stdout = savfox_app_server
            .stdout
            .take()
            .context("savfox app-server stdout unavailable")?;

        Ok(Self {
            child: savfox_app_server,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            pending_notifications: VecDeque::new(),
        })
    }

    fn initialize(&mut self) -> Result<InitializeResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::Initialize {
            request_id: request_id.clone(),
            params: InitializeParams {
                client_info: ClientInfo {
                    name: "savfox-toy-app-server".to_string(),
                    title: Some("Savfox Toy App Server".to_string()),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                }),
            },
        };

        self.send_request(request, request_id, "initialize")
    }

    fn start_session(&mut self) -> Result<NewConversationResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::NewConversation {
            request_id: request_id.clone(),
            params: NewConversationParams::default(),
        };

        self.send_request(request, request_id, "newConversation")
    }

    fn add_conversation_listener(
        &mut self,
        conversation_id: &SessionId,
    ) -> Result<AddConversationSubscriptionResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::AddConversationListener {
            request_id: request_id.clone(),
            params: AddConversationListenerParams {
                conversation_id: *conversation_id,
                experimental_raw_events: false,
            },
        };

        self.send_request(request, request_id, "addConversationListener")
    }

    fn remove_session_listener(&mut self, subscription_id: Uuid) -> Result<()> {
        let request_id = self.request_id();
        let request = ClientRequest::RemoveConversationListener {
            request_id: request_id.clone(),
            params: savfox_app_server_protocol::RemoveConversationListenerParams {
                subscription_id,
            },
        };

        self.send_request::<savfox_app_server_protocol::RemoveConversationSubscriptionResponse>(
            request,
            request_id,
            "removeConversationListener",
        )?;

        Ok(())
    }

    fn send_user_message(
        &mut self,
        conversation_id: &SessionId,
        message: &str,
    ) -> Result<SendUserMessageResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::SendUserMessage {
            request_id: request_id.clone(),
            params: SendUserMessageParams {
                conversation_id: *conversation_id,
                items: vec![InputItem::Text {
                    text: message.to_string(),
                    // Test client sends plain text without UI element ranges.
                    text_elements: Vec::new(),
                }],
            },
        };

        self.send_request(request, request_id, "sendUserMessage")
    }

    fn session_start(&mut self, params: SessionStartParams) -> Result<SessionStartResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::SessionStart {
            request_id: request_id.clone(),
            params,
        };

        self.send_request(request, request_id, "session/start")
    }

    fn turn_start(&mut self, params: TurnStartParams) -> Result<TurnStartResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::TurnStart {
            request_id: request_id.clone(),
            params,
        };

        self.send_request(request, request_id, "turn/start")
    }

    fn login_chat_gpt(&mut self) -> Result<LoginChatGptResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::LoginChatGpt {
            request_id: request_id.clone(),
            params: None,
        };

        self.send_request(request, request_id, "loginChatGpt")
    }

    fn get_account_rate_limits(&mut self) -> Result<GetAccountRateLimitsResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::GetAccountRateLimits {
            request_id: request_id.clone(),
            params: None,
        };

        self.send_request(request, request_id, "account/rateLimits/read")
    }

    fn model_list(&mut self, params: ModelListParams) -> Result<ModelListResponse> {
        let request_id = self.request_id();
        let request = ClientRequest::ModelList {
            request_id: request_id.clone(),
            params,
        };

        self.send_request(request, request_id, "model/list")
    }

    fn stream_conversation(&mut self, conversation_id: &SessionId) -> Result<()> {
        loop {
            let notification = self.next_notification()?;

            if !notification.method.starts_with("savfox/event/") {
                continue;
            }

            if let Some(event) = self.extract_event(notification, conversation_id)? {
                match &event.msg {
                    EventMsg::AgentMessage(event) => {
                        println!("{}", event.message);
                    }
                    EventMsg::AgentMessageDelta(event) => {
                        print!("{}", event.delta);
                        std::io::stdout().flush().ok();
                    }
                    EventMsg::TurnComplete(event) => {
                        println!("\n[task complete: {event:?}]");
                        break;
                    }
                    EventMsg::TurnAborted(event) => {
                        println!("\n[turn aborted: {:?}]", event.reason);
                        break;
                    }
                    EventMsg::Error(event) => {
                        println!("[error] {event:?}");
                    }
                    _ => {
                        println!("[UNKNOWN EVENT] {:?}", event.msg);
                    }
                }
            }
        }

        Ok(())
    }

    fn wait_for_login_completion(
        &mut self,
        expected_login_id: &Uuid,
    ) -> Result<LoginChatGptCompleteNotification> {
        loop {
            let notification = self.next_notification()?;

            if let Ok(server_notification) = ServerNotification::try_from(notification) {
                match server_notification {
                    ServerNotification::LoginChatGptComplete(completion) => {
                        if &completion.login_id == expected_login_id {
                            return Ok(completion);
                        }

                        println!(
                            "[ignoring loginChatGptComplete for unexpected login_id: {}]",
                            completion.login_id
                        );
                    }
                    ServerNotification::AuthStatusChange(status) => {
                        println!("< authStatusChange notification: {status:?}");
                    }
                    ServerNotification::AccountRateLimitsUpdated(snapshot) => {
                        println!("< accountRateLimitsUpdated notification: {snapshot:?}");
                    }
                    ServerNotification::SessionConfigured(_) => {
                        // SessionConfigured notifications are unrelated to login; skip.
                    }
                    _ => {}
                }
            }

            // Not a server notification (likely a conversation event); keep waiting.
        }
    }

    fn stream_turn(&mut self, session_id: &str, turn_id: &str) -> Result<()> {
        loop {
            let notification = self.next_notification()?;

            let Ok(server_notification) = ServerNotification::try_from(notification) else {
                continue;
            };

            match server_notification {
                ServerNotification::SessionStarted(payload) => {
                    if payload.session.id == session_id {
                        println!("< session/started notification: {:?}", payload.session);
                    }
                }
                ServerNotification::TurnStarted(payload) => {
                    if payload.turn.id == turn_id {
                        println!("< turn/started notification: {:?}", payload.turn.status);
                    }
                }
                ServerNotification::AgentMessageDelta(delta) => {
                    print!("{}", delta.delta);
                    std::io::stdout().flush().ok();
                }
                ServerNotification::CommandExecutionOutputDelta(delta) => {
                    print!("{}", delta.delta);
                    std::io::stdout().flush().ok();
                }
                ServerNotification::TerminalInteraction(delta) => {
                    println!("[stdin sent: {}]", delta.stdin);
                    std::io::stdout().flush().ok();
                }
                ServerNotification::ItemStarted(payload) => {
                    println!("\n< item started: {:?}", payload.item);
                }
                ServerNotification::ItemCompleted(payload) => {
                    println!("< item completed: {:?}", payload.item);
                }
                ServerNotification::TurnCompleted(payload) => {
                    if payload.turn.id == turn_id {
                        println!("\n< turn/completed notification: {:?}", payload.turn.status);
                        if payload.turn.status == TurnStatus::Failed
                            && let Some(error) = payload.turn.error
                        {
                            println!("[turn error] {}", error.message);
                        }
                        break;
                    }
                }
                ServerNotification::McpToolCallProgress(payload) => {
                    println!("< MCP tool progress: {}", payload.message);
                }
                _ => {
                    println!("[UNKNOWN SERVER NOTIFICATION] {server_notification:?}");
                }
            }
        }

        Ok(())
    }

    fn extract_event(
        &self,
        notification: JSONRPCNotification,
        conversation_id: &SessionId,
    ) -> Result<Option<Event>> {
        let params = notification
            .params
            .context("event notification missing params")?;

        let mut map = match params {
            Value::Object(map) => map,
            other => bail!("unexpected params shape: {other:?}"),
        };

        let conversation_value = map
            .remove("conversationId")
            .context("event missing conversationId")?;
        let notification_conversation: SessionId = serde_json::from_value(conversation_value)
            .context("conversationId was not a valid UUID")?;

        if &notification_conversation != conversation_id {
            return Ok(None);
        }

        let event_value = Value::Object(map);
        let event: Event =
            serde_json::from_value(event_value).context("failed to decode event payload")?;
        Ok(Some(event))
    }

    fn send_request<T>(
        &mut self,
        request: ClientRequest,
        request_id: RequestId,
        method: &str,
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self.write_request(&request)?;
        self.wait_for_response(request_id, method)
    }

    fn write_request(&mut self, request: &ClientRequest) -> Result<()> {
        let request_json = serde_json::to_string(request)?;
        let request_pretty = serde_json::to_string_pretty(request)?;
        print_multiline_with_prefix("> ", &request_pretty);

        if let Some(stdin) = self.stdin.as_mut() {
            writeln!(stdin, "{request_json}")?;
            stdin
                .flush()
                .context("failed to flush request to savfox app-server")?;
        } else {
            bail!("savfox app-server stdin closed");
        }

        Ok(())
    }

    fn wait_for_response<T>(&mut self, request_id: RequestId, method: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        loop {
            let message = self.read_jsonrpc_message()?;

            match message {
                JsonRpcMessage::Response(JSONRPCResponse { id, result }) => {
                    if id == request_id {
                        return serde_json::from_value(result)
                            .with_context(|| format!("{method} response missing payload"));
                    }
                }
                JsonRpcMessage::Error(err) => {
                    if err.id == request_id {
                        bail!("{method} failed: {err:?}");
                    }
                }
                JsonRpcMessage::Notification(notification) => {
                    self.pending_notifications.push_back(notification);
                }
                JsonRpcMessage::Request(request) => {
                    self.handle_server_request(request)?;
                }
            }
        }
    }

    fn next_notification(&mut self) -> Result<JSONRPCNotification> {
        if let Some(notification) = self.pending_notifications.pop_front() {
            return Ok(notification);
        }

        loop {
            let message = self.read_jsonrpc_message()?;

            match message {
                JsonRpcMessage::Notification(notification) => return Ok(notification),
                JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => {
                    // No outstanding requests, so ignore stray responses/errors for now.
                    continue;
                }
                JsonRpcMessage::Request(request) => {
                    self.handle_server_request(request)?;
                }
            }
        }
    }

    fn read_jsonrpc_message(&mut self) -> Result<JsonRpcMessage> {
        loop {
            let mut response_line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut response_line)
                .context("failed to read from savfox app-server")?;

            if bytes == 0 {
                bail!("savfox app-server closed stdout");
            }

            let trimmed = response_line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parsed: Value =
                serde_json::from_str(trimmed).context("response was not valid JSON-RPC")?;
            let pretty = serde_json::to_string_pretty(&parsed)?;
            print_multiline_with_prefix("< ", &pretty);
            let message: JsonRpcMessage = serde_json::from_value(parsed)
                .context("response was not a valid JSON-RPC message")?;
            return Ok(message);
        }
    }

    fn request_id(&self) -> RequestId {
        RequestId::String(Uuid::new_v4().to_string())
    }

    fn handle_server_request(&mut self, request: JSONRPCRequest) -> Result<()> {
        let server_request = ServerRequest::try_from(request)
            .context("failed to deserialize ServerRequest from JSONRPCRequest")?;

        match server_request {
            ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
                self.handle_command_execution_request_approval(request_id, params)?;
            }
            ServerRequest::FileChangeRequestApproval { request_id, params } => {
                self.approve_file_change_request(request_id, params)?;
            }
            other => {
                bail!("received unsupported server request: {other:?}");
            }
        }

        Ok(())
    }

    fn handle_command_execution_request_approval(
        &mut self,
        request_id: RequestId,
        params: CommandExecutionRequestApprovalParams,
    ) -> Result<()> {
        let CommandExecutionRequestApprovalParams {
            session_id,
            turn_id,
            item_id,
            reason,
            command,
            cwd,
            command_actions,
            proposed_execpolicy_amendment,
        } = params;

        println!(
            "\n< commandExecution approval requested for session {session_id}, turn {turn_id}, item {item_id}"
        );
        if let Some(reason) = reason.as_deref() {
            println!("< reason: {reason}");
        }
        if let Some(command) = command.as_deref() {
            println!("< command: {command}");
        }
        if let Some(cwd) = cwd.as_ref() {
            println!("< cwd: {}", cwd.display());
        }
        if let Some(command_actions) = command_actions.as_ref()
            && !command_actions.is_empty()
        {
            println!("< command actions: {command_actions:?}");
        }
        if let Some(execpolicy_amendment) = proposed_execpolicy_amendment.as_ref() {
            println!("< proposed execpolicy amendment: {execpolicy_amendment:?}");
        }

        let response = CommandExecutionRequestApprovalResponse {
            decision: CommandExecutionApprovalDecision::Accept,
        };
        self.send_server_request_response(request_id, &response)?;
        println!("< approved commandExecution request for item {item_id}");
        Ok(())
    }

    fn approve_file_change_request(
        &mut self,
        request_id: RequestId,
        params: FileChangeRequestApprovalParams,
    ) -> Result<()> {
        let FileChangeRequestApprovalParams {
            session_id,
            turn_id,
            item_id,
            reason,
            grant_root,
        } = params;

        println!(
            "\n< fileChange approval requested for session {session_id}, turn {turn_id}, item {item_id}"
        );
        if let Some(reason) = reason.as_deref() {
            println!("< reason: {reason}");
        }
        if let Some(grant_root) = grant_root.as_deref() {
            println!("< grant root: {}", grant_root.display());
        }

        let response = FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::Accept,
        };
        self.send_server_request_response(request_id, &response)?;
        println!("< approved fileChange request for item {item_id}");
        Ok(())
    }

    fn send_server_request_response<T>(&mut self, request_id: RequestId, response: &T) -> Result<()>
    where
        T: Serialize,
    {
        let message = JsonRpcMessage::Response(JSONRPCResponse {
            id: request_id,
            result: serde_json::to_value(response)?,
        });
        self.write_jsonrpc_message(message)
    }

    fn write_jsonrpc_message(&mut self, message: JsonRpcMessage) -> Result<()> {
        let payload = serde_json::to_string(&message)?;
        let pretty = serde_json::to_string_pretty(&message)?;
        print_multiline_with_prefix("> ", &pretty);

        if let Some(stdin) = self.stdin.as_mut() {
            writeln!(stdin, "{payload}")?;
            stdin
                .flush()
                .context("failed to flush response to savfox app-server")?;
            return Ok(());
        }

        bail!("savfox app-server stdin closed")
    }
}

fn print_multiline_with_prefix(prefix: &str, payload: &str) {
    for line in payload.lines() {
        println!("{prefix}{line}");
    }
}

impl Drop for SavfoxClient {
    fn drop(&mut self) {
        let _ = self.stdin.take();

        if let Ok(Some(status)) = self.child.try_wait() {
            println!("[savfox app-server exited: {status}]");
            return;
        }

        thread::sleep(Duration::from_millis(100));

        if let Ok(Some(status)) = self.child.try_wait() {
            println!("[savfox app-server exited: {status}]");
            return;
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
