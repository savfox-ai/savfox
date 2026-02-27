pub mod api;
pub mod config;
pub mod discovery;
pub mod hooks;
pub mod lifecycle;
pub mod loader;
pub mod manifest;
pub mod registry;
pub mod types;

pub use api::{BoxedPlugin, Plugin, PluginBuilder};
pub use config::{PluginLoadConfig, PluginsConfig};
pub use discovery::{
    DiscoveredPlugin, PluginDiscoveryPaths, discover_plugins, find_plugin_by_id, is_plugin_dir,
};
pub use hooks::{HookHandler, HookName, HookSystem};
pub use lifecycle::{PluginLifecycleManager, PluginState, PluginStatus};
pub use loader::{PluginLoadResult, PluginLoaderConfig, load_plugin_from_path, load_plugins};
pub use manifest::{
    ManifestLoadResult, PLUGIN_MANIFEST_FILENAME, PluginManifest, load_plugin_manifest,
};
pub use registry::{PluginRegistry, RegisteredPlugin};
pub use types::{
    PluginChannelDefinition, PluginCommandContext, PluginCommandDefinition, PluginCommandResult,
    PluginConfig, PluginConfigUiHint, PluginContext, PluginError, PluginHookContext,
    PluginHookDefinition, PluginHookEvent, PluginHookResult, PluginIsolationMode, PluginKind,
    PluginMetadata, PluginResult, PluginToolDefinition,
};

pub mod builtins {
    use crate::api::Plugin;
    use crate::types::{
        PluginCommandContext, PluginCommandDefinition, PluginCommandResult, PluginContext,
        PluginToolDefinition,
    };

    pub struct EchoPlugin {
        pub id: String,
        pub name: String,
    }

    impl EchoPlugin {
        pub fn new() -> Self {
            Self {
                id: "echo".to_string(),
                name: "Echo Plugin".to_string(),
            }
        }
    }

    impl Default for EchoPlugin {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Plugin for EchoPlugin {
        fn id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }

        fn description(&self) -> Option<&str> {
            Some("A simple echo plugin for testing")
        }

        fn tools(&self) -> Vec<PluginToolDefinition> {
            vec![PluginToolDefinition {
                name: "echo".to_string(),
                description: "Echo back the input message".to_string(),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "The message to echo back"
                        }
                    },
                    "required": ["message"]
                })),
                required: vec!["message".to_string()],
            }]
        }

        fn commands(&self) -> Vec<PluginCommandDefinition> {
            vec![PluginCommandDefinition {
                name: "echo".to_string(),
                description: "Echo back a message".to_string(),
                accepts_args: true,
                require_auth: false,
            }]
        }

        fn execute_tool(
            &self,
            name: String,
            params: serde_json::Value,
            _ctx: PluginContext,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = crate::types::PluginResult<serde_json::Value>>
                    + Send,
            >,
        > {
            Box::pin(async move {
                if name != "echo" {
                    return Err(crate::types::PluginError::ExecutionError(format!(
                        "Unknown tool: {}",
                        name
                    )));
                }

                let message = params["message"].as_str().ok_or_else(|| {
                    crate::types::PluginError::ExecutionError(
                        "Missing 'message' parameter".to_string(),
                    )
                })?;

                Ok(serde_json::json!({
                    "echo": message
                }))
            })
        }

        fn execute_command(
            &self,
            name: String,
            cmd_ctx: PluginCommandContext,
            _ctx: PluginContext,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PluginCommandResult> + Send>>
        {
            Box::pin(async move {
                if name != "echo" {
                    return PluginCommandResult {
                        text: None,
                        error: Some(format!("Unknown command: {}", name)),
                    };
                }

                let message = cmd_ctx
                    .args
                    .clone()
                    .unwrap_or_else(|| "No message provided".to_string());
                PluginCommandResult {
                    text: Some(format!("🔊 {}", message)),
                    error: None,
                }
            })
        }
    }

    pub struct StatusPlugin {
        pub id: String,
        pub name: String,
    }

    impl StatusPlugin {
        pub fn new() -> Self {
            Self {
                id: "status".to_string(),
                name: "Status Plugin".to_string(),
            }
        }
    }

    impl Default for StatusPlugin {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Plugin for StatusPlugin {
        fn id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }

        fn description(&self) -> Option<&str> {
            Some("Provides system status commands")
        }

        fn commands(&self) -> Vec<PluginCommandDefinition> {
            vec![
                PluginCommandDefinition {
                    name: "ping".to_string(),
                    description: "Check if the bot is responsive".to_string(),
                    accepts_args: false,
                    require_auth: false,
                },
                PluginCommandDefinition {
                    name: "time".to_string(),
                    description: "Show current server time".to_string(),
                    accepts_args: false,
                    require_auth: false,
                },
            ]
        }

        fn execute_command(
            &self,
            name: String,
            _cmd_ctx: PluginCommandContext,
            _ctx: PluginContext,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PluginCommandResult> + Send>>
        {
            Box::pin(async move {
                match name.as_str() {
                    "ping" => PluginCommandResult {
                        text: Some("🏓 Pong!".to_string()),
                        error: None,
                    },
                    "time" => {
                        let now = chrono::Utc::now();
                        PluginCommandResult {
                            text: Some(format!(
                                "🕐 Current time: {}",
                                now.format("%Y-%m-%d %H:%M:%S UTC")
                            )),
                            error: None,
                        }
                    }
                    _ => PluginCommandResult {
                        text: None,
                        error: Some(format!("Unknown command: {}", name)),
                    },
                }
            })
        }
    }
}
