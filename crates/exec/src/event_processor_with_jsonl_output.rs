use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

use savfox_core::config::Config;
use savfox_core::protocol;
use savfox_core::protocol::{
    AgentStatus as CoreAgentStatus, CollabAgentInteractionBeginEvent,
    CollabAgentInteractionEndEvent, CollabAgentSpawnBeginEvent, CollabAgentSpawnEndEvent,
    CollabCloseBeginEvent, CollabCloseEndEvent, CollabWaitingBeginEvent, CollabWaitingEndEvent,
};
use savfox_protocol::models::WebSearchAction;
use savfox_protocol::plan_tool::{StepStatus, UpdatePlanArgs};
use serde_json::Value as JsonValue;
use tracing::{error, warn};

use crate::event_processor::{EventProcessor, SavfoxStatus, handle_last_message};
use crate::exec_events::{
    AgentMessageItem, CollabAgentState, CollabAgentStatus, CollabTool, CollabToolCallItem,
    CollabToolCallStatus, CommandExecutionItem, CommandExecutionStatus, ErrorItem, FileChangeItem,
    FileUpdateChange, ItemCompletedEvent, ItemStartedEvent, ItemUpdatedEvent, McpToolCallItem,
    McpToolCallItemError, McpToolCallItemResult, McpToolCallStatus, PatchApplyStatus,
    PatchChangeKind, ReasoningItem, SessionErrorEvent, SessionEvent, SessionItem,
    SessionItemDetails, SessionStartedEvent, TodoItem, TodoListItem, TurnCompletedEvent,
    TurnFailedEvent, TurnStartedEvent, Usage, WebSearchItem,
};

pub struct EventProcessorWithJsonOutput {
    last_message_path: Option<PathBuf>,
    last_proposed_plan: Option<String>,
    next_event_id: AtomicU64,
    // Tracks running commands by call_id, including the associated item id.
    running_commands: HashMap<String, RunningCommand>,
    running_patch_applies: HashMap<String, protocol::PatchApplyBeginEvent>,
    // Tracks the todo list for the current turn (at most one per turn).
    running_todo_list: Option<RunningTodoList>,
    last_total_token_usage: Option<savfox_core::protocol::TokenUsage>,
    running_mcp_tool_calls: HashMap<String, RunningMcpToolCall>,
    running_collab_tool_calls: HashMap<String, RunningCollabToolCall>,
    running_web_search_calls: HashMap<String, String>,
    last_critical_error: Option<SessionErrorEvent>,
}

#[derive(Debug, Clone)]
struct RunningCommand {
    command: String,
    item_id: String,
    aggregated_output: String,
}

#[derive(Debug, Clone)]
struct RunningTodoList {
    item_id: String,
    items: Vec<TodoItem>,
}

#[derive(Debug, Clone)]
struct RunningMcpToolCall {
    server: String,
    tool: String,
    item_id: String,
    arguments: JsonValue,
}

#[derive(Debug, Clone)]
struct RunningCollabToolCall {
    tool: CollabTool,
    item_id: String,
}

impl EventProcessorWithJsonOutput {
    #[must_use] 
    pub fn new(last_message_path: Option<PathBuf>) -> Self {
        Self {
            last_message_path,
            last_proposed_plan: None,
            next_event_id: AtomicU64::new(0),
            running_commands: HashMap::new(),
            running_patch_applies: HashMap::new(),
            running_todo_list: None,
            last_total_token_usage: None,
            running_mcp_tool_calls: HashMap::new(),
            running_collab_tool_calls: HashMap::new(),
            running_web_search_calls: HashMap::new(),
            last_critical_error: None,
        }
    }

    pub fn collect_session_events(&mut self, event: &protocol::Event) -> Vec<SessionEvent> {
        match &event.msg {
            protocol::EventMsg::SessionConfigured(ev) => self.handle_session_configured(ev),
            protocol::EventMsg::SessionNameUpdated(_) => Vec::new(),
            protocol::EventMsg::AgentMessage(ev) => self.handle_agent_message(ev),
            protocol::EventMsg::ItemCompleted(protocol::ItemCompletedEvent {
                item: savfox_protocol::items::TurnItem::Plan(item),
                ..
            }) => {
                self.last_proposed_plan = Some(item.text.clone());
                Vec::new()
            }
            protocol::EventMsg::AgentReasoning(ev) => self.handle_reasoning_event(ev),
            protocol::EventMsg::ExecCommandBegin(ev) => self.handle_exec_command_begin(ev),
            protocol::EventMsg::ExecCommandEnd(ev) => self.handle_exec_command_end(ev),
            protocol::EventMsg::TerminalInteraction(ev) => self.handle_terminal_interaction(ev),
            protocol::EventMsg::ExecCommandOutputDelta(ev) => {
                self.handle_output_chunk(&ev.call_id, &ev.chunk)
            }
            protocol::EventMsg::McpToolCallBegin(ev) => self.handle_mcp_tool_call_begin(ev),
            protocol::EventMsg::McpToolCallEnd(ev) => self.handle_mcp_tool_call_end(ev),
            protocol::EventMsg::CollabAgentSpawnBegin(ev) => self.handle_collab_spawn_begin(ev),
            protocol::EventMsg::CollabAgentSpawnEnd(ev) => self.handle_collab_spawn_end(ev),
            protocol::EventMsg::CollabAgentInteractionBegin(ev) => {
                self.handle_collab_interaction_begin(ev)
            }
            protocol::EventMsg::CollabAgentInteractionEnd(ev) => {
                self.handle_collab_interaction_end(ev)
            }
            protocol::EventMsg::CollabWaitingBegin(ev) => self.handle_collab_wait_begin(ev),
            protocol::EventMsg::CollabWaitingEnd(ev) => self.handle_collab_wait_end(ev),
            protocol::EventMsg::CollabCloseBegin(ev) => self.handle_collab_close_begin(ev),
            protocol::EventMsg::CollabCloseEnd(ev) => self.handle_collab_close_end(ev),
            protocol::EventMsg::PatchApplyBegin(ev) => self.handle_patch_apply_begin(ev),
            protocol::EventMsg::PatchApplyEnd(ev) => self.handle_patch_apply_end(ev),
            protocol::EventMsg::WebSearchBegin(ev) => self.handle_web_search_begin(ev),
            protocol::EventMsg::WebSearchEnd(ev) => self.handle_web_search_end(ev),
            protocol::EventMsg::TokenCount(ev) => {
                if let Some(info) = &ev.info {
                    self.last_total_token_usage = Some(info.total_token_usage.clone());
                }
                Vec::new()
            }
            protocol::EventMsg::TurnStarted(ev) => self.handle_task_started(ev),
            protocol::EventMsg::TurnComplete(_) => self.handle_task_complete(),
            protocol::EventMsg::Error(ev) => {
                let error = SessionErrorEvent {
                    message: ev.message.clone(),
                };
                self.last_critical_error = Some(error.clone());
                vec![SessionEvent::Error(error)]
            }
            protocol::EventMsg::Warning(ev) => {
                let item = SessionItem {
                    id: self.get_next_item_id(),
                    details: SessionItemDetails::Error(ErrorItem {
                        message: ev.message.clone(),
                    }),
                };
                vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })]
            }
            protocol::EventMsg::StreamError(ev) => {
                let message = match &ev.additional_details {
                    Some(details) if !details.trim().is_empty() => {
                        format!("{} ({})", ev.message, details)
                    }
                    _ => ev.message.clone(),
                };
                vec![SessionEvent::Error(SessionErrorEvent { message })]
            }
            protocol::EventMsg::PlanUpdate(ev) => self.handle_plan_update(ev),
            _ => Vec::new(),
        }
    }

    fn get_next_item_id(&self) -> String {
        format!(
            "item_{}",
            self.next_event_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    }

    fn handle_session_configured(
        &self,
        payload: &protocol::SessionConfiguredEvent,
    ) -> Vec<SessionEvent> {
        vec![SessionEvent::SessionStarted(SessionStartedEvent {
            session_id: payload.session_id.to_string(),
        })]
    }

    fn handle_web_search_begin(&mut self, ev: &protocol::WebSearchBeginEvent) -> Vec<SessionEvent> {
        if self.running_web_search_calls.contains_key(&ev.call_id) {
            return Vec::new();
        }
        let item_id = self.get_next_item_id();
        self.running_web_search_calls
            .insert(ev.call_id.clone(), item_id.clone());
        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::WebSearch(WebSearchItem {
                id: ev.call_id.clone(),
                query: String::new(),
                action: WebSearchAction::Other,
            }),
        };

        vec![SessionEvent::ItemStarted(ItemStartedEvent { item })]
    }

    fn handle_web_search_end(&mut self, ev: &protocol::WebSearchEndEvent) -> Vec<SessionEvent> {
        let item_id = self
            .running_web_search_calls
            .remove(&ev.call_id)
            .unwrap_or_else(|| self.get_next_item_id());
        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::WebSearch(WebSearchItem {
                id: ev.call_id.clone(),
                query: ev.query.clone(),
                action: ev.action.clone(),
            }),
        };

        vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })]
    }

    fn handle_output_chunk(&mut self, _call_id: &str, _chunk: &[u8]) -> Vec<SessionEvent> {
        // TODO see how we want to process them
        vec![]
    }

    fn handle_terminal_interaction(
        &mut self,
        _ev: &protocol::TerminalInteractionEvent,
    ) -> Vec<SessionEvent> {
        // TODO see how we want to process them
        vec![]
    }

    fn handle_agent_message(&self, payload: &protocol::AgentMessageEvent) -> Vec<SessionEvent> {
        let item = SessionItem {
            id: self.get_next_item_id(),

            details: SessionItemDetails::AgentMessage(AgentMessageItem {
                text: payload.message.clone(),
            }),
        };

        vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })]
    }

    fn handle_reasoning_event(&self, ev: &protocol::AgentReasoningEvent) -> Vec<SessionEvent> {
        let item = SessionItem {
            id: self.get_next_item_id(),

            details: SessionItemDetails::Reasoning(ReasoningItem {
                text: ev.text.clone(),
            }),
        };

        vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })]
    }
    fn handle_exec_command_begin(
        &mut self,
        ev: &protocol::ExecCommandBeginEvent,
    ) -> Vec<SessionEvent> {
        let item_id = self.get_next_item_id();

        let command_string = match shlex::try_join(ev.command.iter().map(String::as_str)) {
            Ok(command_string) => command_string,
            Err(e) => {
                warn!(
                    call_id = ev.call_id,
                    "Failed to stringify command: {e:?}; skipping item.started"
                );
                ev.command.join(" ")
            }
        };

        self.running_commands.insert(
            ev.call_id.clone(),
            RunningCommand {
                command: command_string.clone(),
                item_id: item_id.clone(),
                aggregated_output: String::new(),
            },
        );

        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::CommandExecution(CommandExecutionItem {
                command: command_string,
                aggregated_output: String::new(),
                exit_code: None,
                status: CommandExecutionStatus::InProgress,
            }),
        };

        vec![SessionEvent::ItemStarted(ItemStartedEvent { item })]
    }

    fn handle_mcp_tool_call_begin(
        &mut self,
        ev: &protocol::McpToolCallBeginEvent,
    ) -> Vec<SessionEvent> {
        let item_id = self.get_next_item_id();
        let server = ev.invocation.server.clone();
        let tool = ev.invocation.tool.clone();
        let arguments = ev.invocation.arguments.clone().unwrap_or(JsonValue::Null);

        self.running_mcp_tool_calls.insert(
            ev.call_id.clone(),
            RunningMcpToolCall {
                server: server.clone(),
                tool: tool.clone(),
                item_id: item_id.clone(),
                arguments: arguments.clone(),
            },
        );

        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::McpToolCall(McpToolCallItem {
                server,
                tool,
                arguments,
                result: None,
                error: None,
                status: McpToolCallStatus::InProgress,
            }),
        };

        vec![SessionEvent::ItemStarted(ItemStartedEvent { item })]
    }

    fn handle_mcp_tool_call_end(
        &mut self,
        ev: &protocol::McpToolCallEndEvent,
    ) -> Vec<SessionEvent> {
        let status = if ev.is_success() {
            McpToolCallStatus::Completed
        } else {
            McpToolCallStatus::Failed
        };

        let (server, tool, item_id, arguments) =
            if let Some(running) = self.running_mcp_tool_calls.remove(&ev.call_id) { (
                running.server,
                running.tool,
                running.item_id,
                running.arguments,
            ) } else {
                warn!(
                    call_id = ev.call_id,
                    "Received McpToolCallEnd without begin; synthesizing new item"
                );
                (
                    ev.invocation.server.clone(),
                    ev.invocation.tool.clone(),
                    self.get_next_item_id(),
                    ev.invocation.arguments.clone().unwrap_or(JsonValue::Null),
                )
            };

        let (result, error) = match &ev.result {
            Ok(value) => {
                let result = McpToolCallItemResult {
                    content: value.content.clone(),
                    structured_content: value.structured_content.clone(),
                };
                (Some(result), None)
            }
            Err(message) => (
                None,
                Some(McpToolCallItemError {
                    message: message.clone(),
                }),
            ),
        };

        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::McpToolCall(McpToolCallItem {
                server,
                tool,
                arguments,
                result,
                error,
                status,
            }),
        };

        vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })]
    }

    fn handle_collab_spawn_begin(&mut self, ev: &CollabAgentSpawnBeginEvent) -> Vec<SessionEvent> {
        self.start_collab_tool_call(
            &ev.call_id,
            CollabTool::SpawnAgent,
            ev.sender_session_id.to_string(),
            Vec::new(),
            Some(ev.prompt.clone()),
        )
    }

    fn handle_collab_spawn_end(&mut self, ev: &CollabAgentSpawnEndEvent) -> Vec<SessionEvent> {
        let (receiver_session_ids, agents_states) = match ev.new_session_id {
            Some(id) => {
                let receiver_id = id.to_string();
                let agent_state = CollabAgentState::from(ev.status.clone());
                (
                    vec![receiver_id.clone()],
                    [(receiver_id, agent_state)].into_iter().collect(),
                )
            }
            None => (Vec::new(), HashMap::new()),
        };
        let status = if ev.new_session_id.is_some() && !is_collab_failure(&ev.status) {
            CollabToolCallStatus::Completed
        } else {
            CollabToolCallStatus::Failed
        };
        self.finish_collab_tool_call(
            &ev.call_id,
            CollabTool::SpawnAgent,
            ev.sender_session_id.to_string(),
            receiver_session_ids,
            Some(ev.prompt.clone()),
            agents_states,
            status,
        )
    }

    fn handle_collab_interaction_begin(
        &mut self,
        ev: &CollabAgentInteractionBeginEvent,
    ) -> Vec<SessionEvent> {
        self.start_collab_tool_call(
            &ev.call_id,
            CollabTool::SendInput,
            ev.sender_session_id.to_string(),
            vec![ev.receiver_session_id.to_string()],
            Some(ev.prompt.clone()),
        )
    }

    fn handle_collab_interaction_end(
        &mut self,
        ev: &CollabAgentInteractionEndEvent,
    ) -> Vec<SessionEvent> {
        let receiver_id = ev.receiver_session_id.to_string();
        let agent_state = CollabAgentState::from(ev.status.clone());
        let status = if is_collab_failure(&ev.status) {
            CollabToolCallStatus::Failed
        } else {
            CollabToolCallStatus::Completed
        };
        self.finish_collab_tool_call(
            &ev.call_id,
            CollabTool::SendInput,
            ev.sender_session_id.to_string(),
            vec![receiver_id.clone()],
            Some(ev.prompt.clone()),
            [(receiver_id, agent_state)].into_iter().collect(),
            status,
        )
    }

    fn handle_collab_wait_begin(&mut self, ev: &CollabWaitingBeginEvent) -> Vec<SessionEvent> {
        self.start_collab_tool_call(
            &ev.call_id,
            CollabTool::Wait,
            ev.sender_session_id.to_string(),
            ev.receiver_session_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            None,
        )
    }

    fn handle_collab_wait_end(&mut self, ev: &CollabWaitingEndEvent) -> Vec<SessionEvent> {
        let status = if ev.statuses.values().any(is_collab_failure) {
            CollabToolCallStatus::Failed
        } else {
            CollabToolCallStatus::Completed
        };
        let mut receiver_session_ids = ev
            .statuses
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        receiver_session_ids.sort();
        let agents_states = ev
            .statuses
            .iter()
            .map(|(session_id, status)| {
                (
                    session_id.to_string(),
                    CollabAgentState::from(status.clone()),
                )
            })
            .collect();
        self.finish_collab_tool_call(
            &ev.call_id,
            CollabTool::Wait,
            ev.sender_session_id.to_string(),
            receiver_session_ids,
            None,
            agents_states,
            status,
        )
    }

    fn handle_collab_close_begin(&mut self, ev: &CollabCloseBeginEvent) -> Vec<SessionEvent> {
        self.start_collab_tool_call(
            &ev.call_id,
            CollabTool::CloseAgent,
            ev.sender_session_id.to_string(),
            vec![ev.receiver_session_id.to_string()],
            None,
        )
    }

    fn handle_collab_close_end(&mut self, ev: &CollabCloseEndEvent) -> Vec<SessionEvent> {
        let receiver_id = ev.receiver_session_id.to_string();
        let agent_state = CollabAgentState::from(ev.status.clone());
        let status = if is_collab_failure(&ev.status) {
            CollabToolCallStatus::Failed
        } else {
            CollabToolCallStatus::Completed
        };
        self.finish_collab_tool_call(
            &ev.call_id,
            CollabTool::CloseAgent,
            ev.sender_session_id.to_string(),
            vec![receiver_id.clone()],
            None,
            [(receiver_id, agent_state)].into_iter().collect(),
            status,
        )
    }

    fn start_collab_tool_call(
        &mut self,
        call_id: &str,
        tool: CollabTool,
        sender_session_id: String,
        receiver_session_ids: Vec<String>,
        prompt: Option<String>,
    ) -> Vec<SessionEvent> {
        let item_id = self.get_next_item_id();
        self.running_collab_tool_calls.insert(
            call_id.to_owned(),
            RunningCollabToolCall {
                tool: tool.clone(),
                item_id: item_id.clone(),
            },
        );
        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::CollabToolCall(CollabToolCallItem {
                tool,
                sender_session_id,
                receiver_session_ids,
                prompt,
                agents_states: HashMap::new(),
                status: CollabToolCallStatus::InProgress,
            }),
        };
        vec![SessionEvent::ItemStarted(ItemStartedEvent { item })]
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_collab_tool_call(
        &mut self,
        call_id: &str,
        tool: CollabTool,
        sender_session_id: String,
        receiver_session_ids: Vec<String>,
        prompt: Option<String>,
        agents_states: HashMap<String, CollabAgentState>,
        status: CollabToolCallStatus,
    ) -> Vec<SessionEvent> {
        let (tool, item_id) = if let Some(running) = self.running_collab_tool_calls.remove(call_id) { (running.tool, running.item_id) } else {
            warn!(
                call_id,
                "Received collab tool end without begin; synthesizing new item"
            );
            (tool, self.get_next_item_id())
        };
        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::CollabToolCall(CollabToolCallItem {
                tool,
                sender_session_id,
                receiver_session_ids,
                prompt,
                agents_states,
                status,
            }),
        };
        vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })]
    }

    fn handle_patch_apply_begin(
        &mut self,
        ev: &protocol::PatchApplyBeginEvent,
    ) -> Vec<SessionEvent> {
        self.running_patch_applies
            .insert(ev.call_id.clone(), ev.clone());

        Vec::new()
    }

    fn map_change_kind(&self, kind: &protocol::FileChange) -> PatchChangeKind {
        match kind {
            protocol::FileChange::Add { .. } => PatchChangeKind::Add,
            protocol::FileChange::Delete { .. } => PatchChangeKind::Delete,
            protocol::FileChange::Update { .. } => PatchChangeKind::Update,
        }
    }

    fn handle_patch_apply_end(&mut self, ev: &protocol::PatchApplyEndEvent) -> Vec<SessionEvent> {
        if let Some(running_patch_apply) = self.running_patch_applies.remove(&ev.call_id) {
            let status = if ev.success {
                PatchApplyStatus::Completed
            } else {
                PatchApplyStatus::Failed
            };
            let item = SessionItem {
                id: self.get_next_item_id(),

                details: SessionItemDetails::FileChange(FileChangeItem {
                    changes: running_patch_apply
                        .changes
                        .iter()
                        .map(|(path, change)| FileUpdateChange {
                            path: path.to_str().unwrap_or("").to_owned(),
                            kind: self.map_change_kind(change),
                        })
                        .collect(),
                    status,
                }),
            };

            return vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })];
        }

        Vec::new()
    }

    fn handle_exec_command_end(&mut self, ev: &protocol::ExecCommandEndEvent) -> Vec<SessionEvent> {
        let Some(RunningCommand {
            command,
            item_id,
            aggregated_output,
        }) = self.running_commands.remove(&ev.call_id)
        else {
            warn!(
                call_id = ev.call_id,
                "ExecCommandEnd without matching ExecCommandBegin; skipping item.completed"
            );
            return Vec::new();
        };
        let status = if ev.exit_code == 0 {
            CommandExecutionStatus::Completed
        } else {
            CommandExecutionStatus::Failed
        };
        let aggregated_output = if ev.aggregated_output.is_empty() {
            aggregated_output
        } else {
            ev.aggregated_output.clone()
        };
        let item = SessionItem {
            id: item_id,

            details: SessionItemDetails::CommandExecution(CommandExecutionItem {
                command,
                aggregated_output,
                exit_code: Some(ev.exit_code),
                status,
            }),
        };

        vec![SessionEvent::ItemCompleted(ItemCompletedEvent { item })]
    }

    fn todo_items_from_plan(&self, args: &UpdatePlanArgs) -> Vec<TodoItem> {
        args.plan
            .iter()
            .map(|p| TodoItem {
                text: p.step.clone(),
                completed: matches!(p.status, StepStatus::Completed),
            })
            .collect()
    }

    fn handle_plan_update(&mut self, args: &UpdatePlanArgs) -> Vec<SessionEvent> {
        let items = self.todo_items_from_plan(args);

        if let Some(running) = &mut self.running_todo_list {
            running.items = items.clone();
            let item = SessionItem {
                id: running.item_id.clone(),
                details: SessionItemDetails::TodoList(TodoListItem { items }),
            };
            return vec![SessionEvent::ItemUpdated(ItemUpdatedEvent { item })];
        }

        let item_id = self.get_next_item_id();
        self.running_todo_list = Some(RunningTodoList {
            item_id: item_id.clone(),
            items: items.clone(),
        });
        let item = SessionItem {
            id: item_id,
            details: SessionItemDetails::TodoList(TodoListItem { items }),
        };
        vec![SessionEvent::ItemStarted(ItemStartedEvent { item })]
    }

    fn handle_task_started(&mut self, _: &protocol::TurnStartedEvent) -> Vec<SessionEvent> {
        self.last_critical_error = None;
        vec![SessionEvent::TurnStarted(TurnStartedEvent {})]
    }

    fn handle_task_complete(&mut self) -> Vec<SessionEvent> {
        let usage = if let Some(u) = &self.last_total_token_usage {
            Usage {
                input_tokens: u.input_tokens,
                cached_input_tokens: u.cached_input_tokens,
                output_tokens: u.output_tokens,
            }
        } else {
            Usage::default()
        };

        let mut items = Vec::new();

        if let Some(running) = self.running_todo_list.take() {
            let item = SessionItem {
                id: running.item_id,
                details: SessionItemDetails::TodoList(TodoListItem {
                    items: running.items,
                }),
            };
            items.push(SessionEvent::ItemCompleted(ItemCompletedEvent { item }));
        }

        if !self.running_commands.is_empty() {
            for (_, running) in self.running_commands.drain() {
                let item = SessionItem {
                    id: running.item_id,
                    details: SessionItemDetails::CommandExecution(CommandExecutionItem {
                        command: running.command,
                        aggregated_output: running.aggregated_output,
                        exit_code: None,
                        status: CommandExecutionStatus::Completed,
                    }),
                };
                items.push(SessionEvent::ItemCompleted(ItemCompletedEvent { item }));
            }
        }

        if let Some(error) = self.last_critical_error.take() {
            items.push(SessionEvent::TurnFailed(TurnFailedEvent { error }));
        } else {
            items.push(SessionEvent::TurnCompleted(TurnCompletedEvent { usage }));
        }

        items
    }
}

fn is_collab_failure(status: &CoreAgentStatus) -> bool {
    matches!(
        status,
        CoreAgentStatus::Errored(_) | CoreAgentStatus::NotFound
    )
}

impl From<CoreAgentStatus> for CollabAgentState {
    fn from(value: CoreAgentStatus) -> Self {
        match value {
            CoreAgentStatus::PendingInit => Self {
                status: CollabAgentStatus::PendingInit,
                message: None,
            },
            CoreAgentStatus::Running => Self {
                status: CollabAgentStatus::Running,
                message: None,
            },
            CoreAgentStatus::Completed(message) => Self {
                status: CollabAgentStatus::Completed,
                message,
            },
            CoreAgentStatus::Errored(message) => Self {
                status: CollabAgentStatus::Errored,
                message: Some(message),
            },
            CoreAgentStatus::Shutdown => Self {
                status: CollabAgentStatus::Shutdown,
                message: None,
            },
            CoreAgentStatus::NotFound => Self {
                status: CollabAgentStatus::NotFound,
                message: None,
            },
        }
    }
}

impl EventProcessor for EventProcessorWithJsonOutput {
    fn print_config_summary(&mut self, _: &Config, _: &str, ev: &protocol::SessionConfiguredEvent) {
        self.process_event(protocol::Event {
            id: "".to_owned(),
            msg: protocol::EventMsg::SessionConfigured(ev.clone()),
        });
    }

    #[allow(clippy::print_stdout)]
    fn process_event(&mut self, event: protocol::Event) -> SavfoxStatus {
        let aggregated = self.collect_session_events(&event);
        for conv_event in aggregated {
            match serde_json::to_string(&conv_event) {
                Ok(line) => {
                    println!("{line}");
                }
                Err(e) => {
                    error!("Failed to serialize event: {e:?}");
                }
            }
        }

        let protocol::Event { msg, .. } = event;

        match msg {
            protocol::EventMsg::TurnComplete(protocol::TurnCompleteEvent {
                last_agent_message,
            }) => {
                if let Some(output_file) = self.last_message_path.as_deref() {
                    let last_message = last_agent_message
                        .as_deref()
                        .or(self.last_proposed_plan.as_deref());
                    handle_last_message(last_message, output_file);
                }
                SavfoxStatus::InitiateShutdown
            }
            protocol::EventMsg::ShutdownComplete => SavfoxStatus::Shutdown,
            _ => SavfoxStatus::Running,
        }
    }
}
