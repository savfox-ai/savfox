use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::debug;

use crate::types::{PluginHookContext, PluginHookEvent, PluginHookResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookName {
    BeforeAgentStart,
    AgentEnd,
    BeforeCompaction,
    AfterCompaction,
    MessageReceived,
    MessageSending,
    MessageSent,
    BeforeToolCall,
    AfterToolCall,
    ToolResultPersist,
    SessionStart,
    SessionEnd,
    GatewayStart,
    GatewayStop,
}

impl HookName {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookName::BeforeAgentStart => "before_agent_start",
            HookName::AgentEnd => "agent_end",
            HookName::BeforeCompaction => "before_compaction",
            HookName::AfterCompaction => "after_compaction",
            HookName::MessageReceived => "message_received",
            HookName::MessageSending => "message_sending",
            HookName::MessageSent => "message_sent",
            HookName::BeforeToolCall => "before_tool_call",
            HookName::AfterToolCall => "after_tool_call",
            HookName::ToolResultPersist => "tool_result_persist",
            HookName::SessionStart => "session_start",
            HookName::SessionEnd => "session_end",
            HookName::GatewayStart => "gateway_start",
            HookName::GatewayStop => "gateway_stop",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "before_agent_start" => Some(HookName::BeforeAgentStart),
            "agent_end" => Some(HookName::AgentEnd),
            "before_compaction" => Some(HookName::BeforeCompaction),
            "after_compaction" => Some(HookName::AfterCompaction),
            "message_received" => Some(HookName::MessageReceived),
            "message_sending" => Some(HookName::MessageSending),
            "message_sent" => Some(HookName::MessageSent),
            "before_tool_call" => Some(HookName::BeforeToolCall),
            "after_tool_call" => Some(HookName::AfterToolCall),
            "tool_result_persist" => Some(HookName::ToolResultPersist),
            "session_start" => Some(HookName::SessionStart),
            "session_end" => Some(HookName::SessionEnd),
            "gateway_start" => Some(HookName::GatewayStart),
            "gateway_stop" => Some(HookName::GatewayStop),
            _ => None,
        }
    }
}

impl std::fmt::Display for HookName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

pub type HookHandler = Arc<
    dyn Fn(
            PluginHookEvent,
            PluginHookContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = PluginHookResult> + Send + 'static>,
        > + Send
        + Sync,
>;

pub struct HookRegistration {
    pub plugin_id: String,
    pub hook_name: HookName,
    pub handler: HookHandler,
    pub priority: i32,
}

pub struct HookSystem {
    hooks: Arc<RwLock<HashMap<HookName, Vec<HookRegistration>>>>,
}

impl HookSystem {
    pub fn new() -> Self {
        Self {
            hooks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register(
        &self,
        plugin_id: String,
        hook_name: HookName,
        handler: HookHandler,
        priority: i32,
    ) {
        let mut hooks = self.hooks.write().await;
        let registrations = hooks.entry(hook_name).or_insert_with(Vec::new);
        registrations.push(HookRegistration {
            plugin_id,
            hook_name,
            handler,
            priority,
        });
        registrations.sort_by(|a, b| b.priority.cmp(&a.priority));
        debug!("Registered hook handler for {:?}", hook_name);
    }

    pub async fn unregister_all(&self, plugin_id: &str) {
        let mut hooks = self.hooks.write().await;
        for registrations in hooks.values_mut() {
            registrations.retain(|r| r.plugin_id != plugin_id);
        }
    }

    pub async fn emit(
        &self,
        hook_name: HookName,
        event: PluginHookEvent,
        ctx: PluginHookContext,
    ) -> PluginHookResult {
        let hooks = self.hooks.read().await;

        if let Some(registrations) = hooks.get(&hook_name) {
            for registration in registrations {
                let result = (registration.handler)(event.clone(), ctx.clone()).await;

                if result.cancel {
                    debug!(
                        "Hook {:?} cancelled by plugin {}",
                        hook_name, registration.plugin_id
                    );
                    return result;
                }
            }
        }

        PluginHookResult::default()
    }

    pub async fn list_hooks(&self) -> Vec<(HookName, usize)> {
        let hooks = self.hooks.read().await;
        hooks
            .iter()
            .map(|(name, registrations)| (*name, registrations.len()))
            .collect()
    }
}

impl Default for HookSystem {
    fn default() -> Self {
        Self::new()
    }
}

pub fn create_simple_hook<F, Fut>(f: F) -> HookHandler
where
    F: Fn(PluginHookEvent, PluginHookContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = PluginHookResult> + Send + 'static,
{
    Arc::new(move |event, ctx| {
        let fut = f(event, ctx);
        Box::pin(fut)
    })
}
