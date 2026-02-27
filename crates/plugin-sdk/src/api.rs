use std::future::Future;
use std::pin::Pin;

use crate::types::{
    PluginChannelDefinition, PluginCommandContext, PluginCommandResult, PluginConfig,
    PluginContext, PluginHookContext, PluginHookDefinition, PluginHookEvent, PluginHookResult,
    PluginResult, PluginToolDefinition,
};

pub trait Plugin: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn version(&self) -> Option<&str> {
        None
    }
    fn description(&self) -> Option<&str> {
        None
    }

    fn initialize(
        &self,
        _ctx: PluginContext,
    ) -> Pin<Box<dyn Future<Output = PluginResult<()>> + Send>> {
        Box::pin(async { Ok(()) })
    }

    fn init(&self, ctx: PluginContext) -> Pin<Box<dyn Future<Output = PluginResult<()>> + Send>> {
        self.initialize(ctx)
    }

    fn shutdown(
        &self,
        _ctx: PluginContext,
    ) -> Pin<Box<dyn Future<Output = PluginResult<()>> + Send>> {
        Box::pin(async { Ok(()) })
    }

    fn start(&self, ctx: PluginContext) -> Pin<Box<dyn Future<Output = PluginResult<()>> + Send>> {
        self.init(ctx)
    }

    fn stop(&self, ctx: PluginContext) -> Pin<Box<dyn Future<Output = PluginResult<()>> + Send>> {
        self.shutdown(ctx)
    }

    fn uninstall(
        &self,
        _ctx: PluginContext,
    ) -> Pin<Box<dyn Future<Output = PluginResult<()>> + Send>> {
        Box::pin(async { Ok(()) })
    }

    fn register_channel(&self) -> Vec<PluginChannelDefinition> {
        Vec::new()
    }

    fn register_tool(&self) -> Vec<PluginToolDefinition> {
        self.tools()
    }

    fn register_hook(&self) -> Vec<PluginHookDefinition> {
        self.hooks()
    }

    fn tools(&self) -> Vec<PluginToolDefinition> {
        Vec::new()
    }

    fn commands(&self) -> Vec<crate::types::PluginCommandDefinition> {
        Vec::new()
    }

    fn hooks(&self) -> Vec<crate::types::PluginHookDefinition> {
        Vec::new()
    }

    fn execute_tool(
        &self,
        name: String,
        params: serde_json::Value,
        ctx: PluginContext,
    ) -> Pin<Box<dyn Future<Output = PluginResult<serde_json::Value>> + Send>> {
        let _ = (name, params, ctx);
        Box::pin(async {
            Err(crate::types::PluginError::ExecutionError(
                "Tool not implemented".to_string(),
            ))
        })
    }

    fn execute_command(
        &self,
        name: String,
        cmd_ctx: PluginCommandContext,
        ctx: PluginContext,
    ) -> Pin<Box<dyn Future<Output = PluginCommandResult> + Send>> {
        let _ = (name, cmd_ctx, ctx);
        Box::pin(async {
            PluginCommandResult {
                text: None,
                error: Some("Command not implemented".to_string()),
            }
        })
    }

    fn handle_hook(
        &self,
        event: PluginHookEvent,
        hook_ctx: PluginHookContext,
        ctx: PluginContext,
    ) -> Pin<Box<dyn Future<Output = PluginHookResult> + Send>> {
        let _ = (event, hook_ctx, ctx);
        Box::pin(async { PluginHookResult::default() })
    }

    fn validate_config(&self, _config: &PluginConfig) -> PluginResult<()> {
        Ok(())
    }
}

pub type BoxedPlugin = Box<dyn Plugin>;

pub struct PluginBuilder {
    id: String,
    name: String,
    version: Option<String>,
    description: Option<String>,
    channels: Vec<PluginChannelDefinition>,
    tools: Vec<PluginToolDefinition>,
    commands: Vec<crate::types::PluginCommandDefinition>,
    hooks: Vec<PluginHookDefinition>,
}

impl PluginBuilder {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: None,
            description: None,
            channels: Vec::new(),
            tools: Vec::new(),
            commands: Vec::new(),
            hooks: Vec::new(),
        }
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn register_channel(mut self, channel: PluginChannelDefinition) -> Self {
        self.channels.push(channel);
        self
    }

    pub fn channel(self, channel: PluginChannelDefinition) -> Self {
        self.register_channel(channel)
    }

    pub fn register_tool(mut self, tool: PluginToolDefinition) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn tool(mut self, tool: PluginToolDefinition) -> Self {
        self = self.register_tool(tool);
        self
    }

    pub fn command(mut self, command: crate::types::PluginCommandDefinition) -> Self {
        self.commands.push(command);
        self
    }

    pub fn register_hook(mut self, hook: PluginHookDefinition) -> Self {
        self.hooks.push(hook);
        self
    }

    pub fn hook(mut self, hook: crate::types::PluginHookDefinition) -> Self {
        self = self.register_hook(hook);
        self
    }
}

#[macro_export]
macro_rules! declare_plugin {
    ($plugin_type:ty) => {
        #[no_mangle]
        pub extern "C" fn _savfox_create_plugin() -> *mut dyn $crate::Plugin {
            let plugin: Box<dyn $crate::Plugin> = Box::new(<$plugin_type>::default());
            Box::into_raw(plugin)
        }

        #[no_mangle]
        pub extern "C" fn _savfox_destroy_plugin(ptr: *mut dyn $crate::Plugin) {
            unsafe {
                let _ = Box::from_raw(ptr);
            }
        }
    };
}
