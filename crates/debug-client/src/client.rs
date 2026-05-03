use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use savfox_app_server_protocol::{
    AskForApproval, ClientInfo, ClientNotification, ClientRequest,
    CommandExecutionApprovalDecision, FileChangeApprovalDecision, InitializeCapabilities,
    JSONRPCRequest, JSONRPCResponse, JsonRpcMessage, RequestId, SessionListParams,
    SessionResumeParams, SessionResumeResponse, SessionStartParams, SessionStartResponse,
    TurnStartParams, UserInput,
};
use serde::Serialize;

use crate::output::Output;
use crate::reader::start_reader;
use crate::state::{PendingRequest, ReaderEvent, State};

pub struct AppServerClient {
    child: Child,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    stdout: Option<BufReader<ChildStdout>>,
    next_request_id: AtomicI64,
    state: Arc<Mutex<State>>,
    output: Output,
    filtered_output: bool,
}

impl AppServerClient {
    pub fn spawn(
        savfox_bin: &str,
        config_overrides: &[String],
        output: Output,
        filtered_output: bool,
    ) -> Result<Self> {
        let mut cmd = Command::new(savfox_bin);
        for override_kv in config_overrides {
            cmd.arg("--config").arg(override_kv);
        }

        let mut child = cmd
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to start `{savfox_bin}` app-server"))?;

        let stdin = child
            .stdin
            .take()
            .context("savfox app-server stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("savfox app-server stdout unavailable")?;

        Ok(Self {
            child,
            stdin: Arc::new(Mutex::new(Some(stdin))),
            stdout: Some(BufReader::new(stdout)),
            next_request_id: AtomicI64::new(1),
            state: Arc::new(Mutex::new(State::default())),
            output,
            filtered_output,
        })
    }

    pub fn initialize(&mut self) -> Result<()> {
        let request_id = self.next_request_id();
        let request = ClientRequest::Initialize {
            request_id: request_id.clone(),
            params: savfox_app_server_protocol::InitializeParams {
                client_info: ClientInfo {
                    name: "debug-client".to_string(),
                    title: Some("Debug Client".to_string()),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                }),
            },
        };

        self.send(&request)?;
        let response = self.read_until_response(&request_id)?;
        let _parsed: savfox_app_server_protocol::InitializeResponse =
            serde_json::from_value(response.result).context("decode initialize response")?;
        let initialized = ClientNotification::Initialized;
        self.send(&initialized)?;
        Ok(())
    }

    pub fn start_session(&mut self, params: SessionStartParams) -> Result<String> {
        let request_id = self.next_request_id();
        let request = ClientRequest::SessionStart {
            request_id: request_id.clone(),
            params,
        };
        self.send(&request)?;
        let response = self.read_until_response(&request_id)?;
        let parsed: SessionStartResponse =
            serde_json::from_value(response.result).context("decode session/start response")?;
        let session_id = parsed.session.id;
        self.set_session_id(session_id.clone());
        Ok(session_id)
    }

    pub fn resume_session(&mut self, params: SessionResumeParams) -> Result<String> {
        let request_id = self.next_request_id();
        let request = ClientRequest::SessionResume {
            request_id: request_id.clone(),
            params,
        };
        self.send(&request)?;
        let response = self.read_until_response(&request_id)?;
        let parsed: SessionResumeResponse =
            serde_json::from_value(response.result).context("decode session/resume response")?;
        let session_id = parsed.session.id;
        self.set_session_id(session_id.clone());
        Ok(session_id)
    }

    pub fn request_session_start(&self, params: SessionStartParams) -> Result<RequestId> {
        let request_id = self.next_request_id();
        self.track_pending(request_id.clone(), PendingRequest::Start);
        let request = ClientRequest::SessionStart {
            request_id: request_id.clone(),
            params,
        };
        self.send(&request)?;
        Ok(request_id)
    }

    pub fn request_session_resume(&self, params: SessionResumeParams) -> Result<RequestId> {
        let request_id = self.next_request_id();
        self.track_pending(request_id.clone(), PendingRequest::Resume);
        let request = ClientRequest::SessionResume {
            request_id: request_id.clone(),
            params,
        };
        self.send(&request)?;
        Ok(request_id)
    }

    pub fn request_session_list(&self, cursor: Option<String>) -> Result<RequestId> {
        let request_id = self.next_request_id();
        self.track_pending(request_id.clone(), PendingRequest::List);
        let request = ClientRequest::SessionList {
            request_id: request_id.clone(),
            params: SessionListParams {
                cursor,
                limit: None,
                sort_key: None,
                model_providers: None,
                source_kinds: None,
                archived: None,
            },
        };
        self.send(&request)?;
        Ok(request_id)
    }

    pub fn send_turn(&self, session_id: &str, text: String) -> Result<RequestId> {
        let request_id = self.next_request_id();
        let request = ClientRequest::TurnStart {
            request_id: request_id.clone(),
            params: TurnStartParams {
                session_id: session_id.to_string(),
                input: vec![UserInput::Text {
                    text,
                    // Debug client sends plain text with no UI markup spans.
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        };
        self.send(&request)?;
        Ok(request_id)
    }

    pub fn start_reader(
        &mut self,
        events: Sender<ReaderEvent>,
        auto_approve: bool,
        filtered_output: bool,
    ) -> Result<()> {
        let stdout = self.stdout.take().context("reader already started")?;
        start_reader(
            stdout,
            Arc::clone(&self.stdin),
            Arc::clone(&self.state),
            events,
            self.output.clone(),
            auto_approve,
            filtered_output,
        );
        Ok(())
    }

    pub fn session_id(&self) -> Option<String> {
        let state = self.state.lock().expect("state lock poisoned");
        state.session_id.clone()
    }

    pub fn set_session_id(&self, session_id: String) {
        let mut state = self.state.lock().expect("state lock poisoned");
        state.session_id = Some(session_id);
        self.remember_session_locked(&mut state);
    }

    pub fn use_session(&self, session_id: String) -> bool {
        let mut state = self.state.lock().expect("state lock poisoned");
        let known = state.known_sessions.iter().any(|id| id == &session_id);
        state.session_id = Some(session_id);
        self.remember_session_locked(&mut state);
        known
    }

    pub fn shutdown(&mut self) {
        if let Ok(mut stdin) = self.stdin.lock() {
            let _ = stdin.take();
        }
        let _ = self.child.wait();
    }

    fn track_pending(&self, request_id: RequestId, kind: PendingRequest) {
        let mut state = self.state.lock().expect("state lock poisoned");
        state.pending.insert(request_id, kind);
    }

    fn remember_session_locked(&self, state: &mut State) {
        if let Some(session_id) = state.session_id.as_ref()
            && !state.known_sessions.iter().any(|id| id == session_id)
        {
            state.known_sessions.push(session_id.clone());
        }
    }

    fn next_request_id(&self) -> RequestId {
        let id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        RequestId::Integer(id)
    }

    fn send<T: Serialize>(&self, value: &T) -> Result<()> {
        let json = serde_json::to_string(value).context("serialize message")?;
        let mut line = json;
        line.push('\n');
        let mut stdin = self.stdin.lock().expect("stdin lock poisoned");
        let Some(stdin) = stdin.as_mut() else {
            anyhow::bail!("stdin already closed");
        };
        stdin.write_all(line.as_bytes()).context("write message")?;
        stdin.flush().context("flush message")?;
        Ok(())
    }

    fn read_until_response(&mut self, request_id: &RequestId) -> Result<JSONRPCResponse> {
        let stdin = Arc::clone(&self.stdin);
        let output = self.output.clone();
        let reader = self.stdout.as_mut().context("stdout missing")?;
        let mut buffer = String::new();

        loop {
            buffer.clear();
            let bytes = reader
                .read_line(&mut buffer)
                .context("read server output")?;
            if bytes == 0 {
                anyhow::bail!("server closed stdout while awaiting response {request_id:?}");
            }

            let line = buffer.trim_end_matches(['\n', '\r']);
            if !line.is_empty() && !self.filtered_output {
                let _ = output.server_line(line);
            }

            let message = match serde_json::from_str::<JsonRpcMessage>(line) {
                Ok(message) => message,
                Err(_) => continue,
            };

            match message {
                JsonRpcMessage::Response(response) if &response.id == request_id => {
                    return Ok(response);
                }
                JsonRpcMessage::Response(_) => {}
                JsonRpcMessage::Request(request) => {
                    let _ = handle_server_request(request, &stdin);
                }
                _ => {}
            }
        }
    }
}

fn handle_server_request(
    request: JSONRPCRequest,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
) -> Result<()> {
    let Ok(server_request) = savfox_app_server_protocol::ServerRequest::try_from(request) else {
        return Ok(());
    };

    match server_request {
        savfox_app_server_protocol::ServerRequest::CommandExecutionRequestApproval {
            request_id,
            ..
        } => {
            let response = savfox_app_server_protocol::CommandExecutionRequestApprovalResponse {
                decision: CommandExecutionApprovalDecision::Decline,
            };
            send_jsonrpc_response(stdin, request_id, response)
        }
        savfox_app_server_protocol::ServerRequest::FileChangeRequestApproval {
            request_id, ..
        } => {
            let response = savfox_app_server_protocol::FileChangeRequestApprovalResponse {
                decision: FileChangeApprovalDecision::Decline,
            };
            send_jsonrpc_response(stdin, request_id, response)
        }
        _ => Ok(()),
    }
}

fn send_jsonrpc_response<T: Serialize>(
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    request_id: RequestId,
    response: T,
) -> Result<()> {
    let result = serde_json::to_value(response).context("serialize response")?;
    let message = JsonRpcMessage::Response(JSONRPCResponse {
        id: request_id,
        result,
    });
    send_with_stdin(stdin, &message)
}

fn send_with_stdin<T: Serialize>(stdin: &Arc<Mutex<Option<ChildStdin>>>, value: &T) -> Result<()> {
    let json = serde_json::to_string(value).context("serialize message")?;
    let mut line = json;
    line.push('\n');
    let mut stdin = stdin.lock().expect("stdin lock poisoned");
    let Some(stdin) = stdin.as_mut() else {
        anyhow::bail!("stdin already closed");
    };
    stdin.write_all(line.as_bytes()).context("write message")?;
    stdin.flush().context("flush message")?;
    Ok(())
}

pub fn build_session_start_params(
    approval_policy: AskForApproval,
    model: Option<String>,
    model_provider: Option<String>,
    cwd: Option<String>,
) -> SessionStartParams {
    SessionStartParams {
        model,
        model_provider,
        cwd,
        approval_policy: Some(approval_policy),
        experimental_raw_events: false,
        ..Default::default()
    }
}

pub fn build_session_resume_params(
    session_id: String,
    approval_policy: AskForApproval,
    model: Option<String>,
    model_provider: Option<String>,
    cwd: Option<String>,
) -> SessionResumeParams {
    SessionResumeParams {
        session_id,
        model,
        model_provider,
        cwd,
        approval_policy: Some(approval_policy),
        ..Default::default()
    }
}
