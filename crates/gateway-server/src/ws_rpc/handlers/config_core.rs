#![allow(unused_imports, clippy::module_inception)]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::super::heartbeat_config_path;
use super::super::types::{INTERNAL_ERROR, INVALID_REQUEST, RpcResult};
use super::super::utils::{opt_bool, opt_str, opt_u64};
use crate::channel::GatewayChannel;
use crate::cron_service::CronService;
use crate::log_store;
use crate::session::{GatewaySessionManager, SessionStore};

pub(crate) async fn handle_config_schema() -> RpcResult {
    Ok(json!({
        "schema": {
            "type": "object",
            "properties": {
                "gateway": {
                    "type": "object",
                    "title": "Gateway",
                    "description": "Gateway server settings (port, auth, binding)",
                    "properties": {
                        "host": {
                            "type": "string",
                            "title": "Host",
                            "description": "Host address to bind to (e.g., 127.0.0.1 or 0.0.0.0)",
                            "default": "127.0.0.1"
                        },
                        "port": {
                            "type": "integer",
                            "title": "Port",
                            "description": "Port to listen on",
                            "default": 18881,
                            "minimum": 1,
                            "maximum": 65535
                        },
                        "token": {
                            "type": "string",
                            "title": "Auth Token",
                            "description": "Bearer token for authentication (auto-generated if not set)"
                        },
                        "tls_cert": {
                            "type": "string",
                            "title": "TLS Certificate",
                            "description": "Path to TLS certificate file (PEM format)"
                        },
                        "tls_key": {
                            "type": "string",
                            "title": "TLS Key",
                            "description": "Path to TLS private key file (PEM format)"
                        }
                    }
                },
                "env": {
                    "type": "object",
                    "title": "Environment",
                    "description": "Environment variables passed to the gateway process",
                    "properties": {
                        "shell_env": {
                            "type": "object",
                            "title": "Shell Env",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "timeout_ms": {
                                    "type": "integer",
                                    "title": "Timeout (ms)",
                                    "default": 3000
                                }
                            }
                        },
                        "vars": {
                            "type": "object",
                            "title": "Variables",
                            "description": "Key/value variables injected into runtime environment",
                            "additionalProperties": { "type": "string" }
                        }
                    }
                },
                "update": {
                    "type": "object",
                    "title": "Updates",
                    "description": "Auto-update settings and release channel",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "title": "Channel",
                            "enum": ["stable", "beta", "dev"],
                            "default": "stable"
                        },
                        "check_on_start": {
                            "type": "boolean",
                            "title": "Check On Start",
                            "default": true
                        }
                    }
                },
                "auth": {
                    "type": "object",
                    "title": "Authentication",
                    "description": "API keys and authentication profiles",
                    "properties": {
                        "profiles": {
                            "type": "object",
                            "title": "Profiles",
                            "additionalProperties": {
                                "type": "object",
                                "properties": {
                                    "provider": {
                                        "type": "string",
                                        "title": "Provider"
                                    },
                                    "mode": {
                                        "type": "string",
                                        "title": "Mode",
                                        "enum": ["api_key", "oauth", "token"]
                                    },
                                    "email": {
                                        "type": "string",
                                        "title": "Email"
                                    }
                                }
                            }
                        },
                        "order": {
                            "type": "object",
                            "title": "Profile Order",
                            "additionalProperties": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "cooldowns": {
                            "type": "object",
                            "title": "Cooldowns",
                            "properties": {
                                "billing_backoff_hours": {
                                    "type": "number",
                                    "title": "Billing Backoff (hours)"
                                },
                                "billing_max_hours": {
                                    "type": "number",
                                    "title": "Billing Max (hours)"
                                },
                                "failure_window_hours": {
                                    "type": "number",
                                    "title": "Failure Window (hours)"
                                }
                            }
                        }
                    }
                },
                "messages": {
                    "type": "object",
                    "title": "Messages",
                    "description": "Message handling and routing settings",
                    "properties": {
                        "max_context_chars": {
                            "type": "integer",
                            "title": "Max Context Chars"
                        },
                        "markdown": {
                            "type": "boolean",
                            "title": "Markdown Enabled",
                            "default": true
                        },
                        "include_quoted_reply": {
                            "type": "boolean",
                            "title": "Include Quoted Reply",
                            "default": true
                        }
                    }
                },
                "commands": {
                    "type": "object",
                    "title": "Commands",
                    "description": "Custom slash commands",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "title": "Enabled",
                                "default": true
                            },
                            "description": {
                                "type": "string",
                                "title": "Description"
                            },
                            "prompt": {
                                "type": "string",
                                "title": "Prompt"
                            }
                        }
                    }
                },
                "hooks": {
                    "type": "object",
                    "title": "Hooks",
                    "description": "Webhooks and event hooks",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": false
                        },
                        "path": {
                            "type": "string",
                            "title": "Path"
                        },
                        "token": {
                            "type": "string",
                            "title": "Token"
                        },
                        "default_session_id": {
                            "type": "string",
                            "title": "Default Session ID"
                        },
                        "allow_request_session_id": {
                            "type": "boolean",
                            "title": "Allow Request Session ID",
                            "default": false
                        },
                        "allowed_session_id_prefixes": {
                            "type": "array",
                            "title": "Allowed Session ID Prefixes",
                            "items": { "type": "string" }
                        },
                        "allowed_agent_ids": {
                            "type": "array",
                            "title": "Allowed Agent IDs",
                            "items": { "type": "string" }
                        },
                        "max_body_bytes": {
                            "type": "integer",
                            "title": "Max Body Bytes"
                        },
                        "presets": {
                            "type": "array",
                            "title": "Presets",
                            "items": { "type": "string" }
                        },
                        "transforms_dir": {
                            "type": "string",
                            "title": "Transforms Directory"
                        }
                    }
                },
                "skills": {
                    "type": "object",
                    "title": "Skills",
                    "description": "Skill packs and capabilities",
                    "properties": {
                        "allow_bundled": {
                            "type": "array",
                            "title": "Allow Bundled",
                            "items": { "type": "string" }
                        },
                        "load": {
                            "type": "object",
                            "title": "Load",
                            "properties": {
                                "extra_dirs": {
                                    "type": "array",
                                    "title": "Extra Directories",
                                    "items": { "type": "string" }
                                },
                                "watch": {
                                    "type": "boolean",
                                    "title": "Watch",
                                    "default": false
                                },
                                "watch_debounce_ms": {
                                    "type": "integer",
                                    "title": "Watch Debounce (ms)"
                                }
                            }
                        },
                        "entries": {
                            "type": "object",
                            "title": "Entries",
                            "additionalProperties": {
                                "type": "object",
                                "properties": {
                                    "enabled": {
                                        "type": "boolean",
                                        "title": "Enabled",
                                        "default": true
                                    },
                                    "api_key": {
                                        "type": "string",
                                        "title": "API Key"
                                    },
                                    "env": {
                                        "type": "object",
                                        "title": "Env",
                                        "additionalProperties": { "type": "string" }
                                    },
                                    "config": {
                                        "type": "object",
                                        "title": "Config",
                                        "additionalProperties": true
                                    }
                                }
                            }
                        }
                    }
                },
                "wizard": {
                    "type": "object",
                    "title": "Setup Wizard",
                    "description": "Setup wizard state and history",
                    "properties": {
                        "last_run_at": {
                            "type": "string",
                            "title": "Last Run At"
                        },
                        "last_run_version": {
                            "type": "string",
                            "title": "Last Run Version"
                        },
                        "last_run_commit": {
                            "type": "string",
                            "title": "Last Run Commit"
                        },
                        "last_run_command": {
                            "type": "string",
                            "title": "Last Run Command"
                        },
                        "last_run_mode": {
                            "type": "string",
                            "title": "Last Run Mode",
                            "enum": ["local", "remote"]
                        }
                    }
                },
                "browser": {
                    "type": "object",
                    "title": "Browser",
                    "description": "Browser automation settings",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": false
                        },
                        "evaluate_enabled": {
                            "type": "boolean",
                            "title": "Evaluate Enabled",
                            "default": false
                        },
                        "cdp_url": {
                            "type": "string",
                            "title": "CDP URL"
                        },
                        "headless": {
                            "type": "boolean",
                            "title": "Headless",
                            "default": true
                        },
                        "no_sandbox": {
                            "type": "boolean",
                            "title": "No Sandbox",
                            "default": false
                        },
                        "executable_path": {
                            "type": "string",
                            "title": "Executable Path"
                        },
                        "default_profile": {
                            "type": "string",
                            "title": "Default Profile"
                        }
                    }
                },
                "canvasHost": {
                    "type": "object",
                    "title": "Canvas Host",
                    "description": "Canvas rendering and display",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": false
                        },
                        "root": {
                            "type": "string",
                            "title": "Root"
                        },
                        "port": {
                            "type": "integer",
                            "title": "Port"
                        },
                        "live_reload": {
                            "type": "boolean",
                            "title": "Live Reload",
                            "default": false
                        }
                    }
                },
                "talk": {
                    "type": "object",
                    "title": "Talk",
                    "description": "Voice and speech settings",
                    "properties": {
                        "voice_id": {
                            "type": "string",
                            "title": "Voice ID"
                        },
                        "voice_aliases": {
                            "type": "object",
                            "title": "Voice Aliases",
                            "additionalProperties": { "type": "string" }
                        },
                        "model_id": {
                            "type": "string",
                            "title": "Model ID"
                        },
                        "output_format": {
                            "type": "string",
                            "title": "Output Format"
                        },
                        "api_key": {
                            "type": "string",
                            "title": "API Key"
                        },
                        "interrupt_on_speech": {
                            "type": "boolean",
                            "title": "Interrupt On Speech",
                            "default": false
                        }
                    }
                },
                "agents": {
                    "type": "object",
                    "title": "Agents",
                    "description": "Agent configurations, models, and identities",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "model": {
                                "type": "string",
                                "title": "Model (legacy)",
                                "description": "Legacy flat model ID (use models.primary instead)"
                            },
                            "provider": {
                                "type": "string",
                                "title": "Provider (legacy)",
                                "description": "Legacy provider field",
                                "enum": ["openai", "anthropic", "azure", "ollama", "lmstudio", "google"]
                            },
                            "models": {
                                "type": "object",
                                "title": "Models",
                                "properties": {
                                    "primary": {
                                        "type": "string",
                                        "title": "Primary Model",
                                        "description": "Global model ID (e.g. openai/gpt-4o)"
                                    },
                                    "fallbacks": {
                                        "type": "array",
                                        "title": "Fallback Models",
                                        "items": { "type": "string" },
                                        "description": "Fallback model IDs tried in order"
                                    }
                                }
                            },
                            "system_prompt": {
                                "type": "string",
                                "title": "System Prompt",
                                "description": "System prompt for this agent"
                            },
                            "dm_scope": {
                                "type": "string",
                                "title": "DM Session Scope",
                                "description": "How direct-message sessions are scoped: main (shared), per_peer (per user), per_channel_peer (per channel+user), per_account_channel_peer (per account+channel+user)",
                                "enum": ["main", "per_peer", "per_channel_peer", "per_account_channel_peer"],
                                "default": "main"
                            },
                            "identity": {
                                "type": "object",
                                "title": "Identity",
                                "description": "Agent identity settings",
                                "properties": {
                                    "name": {
                                        "type": "string",
                                        "title": "Name",
                                        "description": "Agent display name"
                                    },
                                    "avatar": {
                                        "type": "string",
                                        "title": "Avatar",
                                        "description": "Avatar URL or emoji"
                                    },
                                    "description": {
                                        "type": "string",
                                        "title": "Description",
                                        "description": "Agent description"
                                    }
                                }
                            },
                            "thinking": {
                                "type": "string",
                                "title": "Thinking Level",
                                "description": "Thinking/reasoning effort level for this agent",
                                "enum": ["off", "minimal", "low", "medium", "high", "xhigh"],
                                "default": "medium"
                            },
                            "tools": {
                                "type": "object",
                                "title": "Tool Controls",
                                "description": "Tool allow/deny lists for this agent",
                                "properties": {
                                    "allow_list": {
                                        "type": "array",
                                        "title": "Allowed Tools",
                                        "items": { "type": "string" },
                                        "description": "Only allow these tools (empty = all)"
                                    },
                                    "deny_list": {
                                        "type": "array",
                                        "title": "Denied Tools",
                                        "items": { "type": "string" },
                                        "description": "Block these tools"
                                    }
                                }
                            },
                            "memory": {
                                "type": "object",
                                "title": "Memory Settings",
                                "properties": {
                                    "enabled": {
                                        "type": "boolean",
                                        "title": "Memory Enabled",
                                        "description": "Enable memory system for this agent",
                                        "default": true
                                    }
                                }
                            },
                            "compaction": {
                                "type": "object",
                                "title": "Compaction",
                                "description": "Context window compaction settings",
                                "properties": {
                                    "mode": {
                                        "type": "string",
                                        "title": "Compaction Mode",
                                        "enum": ["auto", "manual", "off"],
                                        "default": "auto"
                                    },
                                    "max_history_share": {
                                        "type": "number",
                                        "title": "Max History Share",
                                        "description": "Max fraction of context for history (0-1)",
                                        "default": 0.7,
                                        "minimum": 0,
                                        "maximum": 1
                                    }
                                }
                            },
                            "sandbox": {
                                "type": "object",
                                "title": "Sandbox",
                                "description": "Code execution sandbox settings",
                                "properties": {
                                    "mode": {
                                        "type": "string",
                                        "title": "Sandbox Mode",
                                        "enum": ["off", "non_main", "all"],
                                        "default": "off"
                                    }
                                }
                            },
                            "heartbeat": {
                                "type": "object",
                                "title": "Heartbeat",
                                "description": "Periodic agent heartbeat settings",
                                "properties": {
                                    "every": {
                                        "type": "string",
                                        "title": "Interval",
                                        "description": "Heartbeat interval (e.g. '30m', '1h')"
                                    },
                                    "active_hours": {
                                        "type": "object",
                                        "title": "Active Hours",
                                        "properties": {
                                            "start": { "type": "string", "title": "Start", "description": "Start time (HH:MM)" },
                                            "end": { "type": "string", "title": "End", "description": "End time (HH:MM)" },
                                            "timezone": { "type": "string", "title": "Timezone", "description": "IANA timezone" }
                                        }
                                    },
                                    "prompt": {
                                        "type": "string",
                                        "title": "Heartbeat Prompt",
                                        "description": "Custom prompt for heartbeat messages"
                                    }
                                }
                            },
                            "group_activation": {
                                "type": "string",
                                "title": "Group Activation",
                                "description": "When to respond in group chats",
                                "enum": ["mention", "keyword", "always", "command", "off"],
                                "default": "mention"
                            },
                            "channel_replies": {
                                "type": "array",
                                "title": "Channel Replies",
                                "description": "Per-channel route reply modes for this agent. The default route uses group_activation.",
                                "items": {
                                    "type": "object",
                                    "required": ["channel_id"],
                                    "properties": {
                                        "channel_id": {
                                            "type": "string",
                                            "title": "Channel Route",
                                            "description": "Saved channel config id or channel route key"
                                        },
                                        "group_activation": {
                                            "type": "string",
                                            "title": "Reply Mode",
                                            "enum": ["mention", "keyword", "always", "command", "off"],
                                            "default": "mention"
                                        }
                                    }
                                }
                            },
                            "group_keywords": {
                                "type": "array",
                                "title": "Group Keywords",
                                "description": "Keywords that activate the agent when group activation is set to keyword mode",
                                "items": { "type": "string" }
                            },
                            "agent_aliases": {
                                "type": "array",
                                "title": "Agent Aliases",
                                "description": "Additional names or call-signs that explicitly target this agent in chat",
                                "items": { "type": "string" }
                            },
                            "ingest_policy": {
                                "type": "string",
                                "title": "Ingest Policy",
                                "description": "How non-reply messages are buffered into ambient context",
                                "enum": [
                                    "none",
                                    "reply_only",
                                    "targeted_only",
                                    "all_human_messages",
                                    "all_non_bot_messages",
                                    "all_messages"
                                ]
                            },
                            "external_bot_policy": {
                                "type": "string",
                                "title": "External Bot Policy",
                                "description": "How messages from third-party bots are handled",
                                "enum": ["ignore", "ingest_only", "reply_allowed"],
                                "default": "ignore"
                            },
                            "idle_reply": {
                                "type": "object",
                                "title": "Idle Reply",
                                "description": "Delayed fallback reply settings for quiet rooms",
                                "properties": {
                                    "enabled": {
                                        "type": "boolean",
                                        "title": "Enabled",
                                        "description": "Whether the agent should reply after a room stays quiet",
                                        "default": false
                                    },
                                    "delay_secs": {
                                        "type": "integer",
                                        "title": "Delay Seconds",
                                        "description": "How long to wait after the last buffered room message before replying",
                                        "default": 180,
                                        "minimum": 30
                                    },
                                    "max_per_hour": {
                                        "type": "integer",
                                        "title": "Max Replies Per Hour",
                                        "description": "Maximum number of idle fallback replies allowed per session each hour",
                                        "default": 1,
                                        "minimum": 1
                                    },
                                    "prompt": {
                                        "type": "string",
                                        "title": "Idle Prompt",
                                        "description": "Optional custom instruction used when the idle fallback reply fires"
                                    }
                                }
                            }
                        }
                    }
                },
                "models": {
                    "type": "object",
                    "title": "Models",
                    "description": "AI model configurations and providers",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "provider": {
                                "type": "string",
                                "title": "Provider",
                                "description": "Model provider",
                                "enum": ["openai", "anthropic", "azure", "google", "ollama", "lmstudio", "openrouter", "deepseek", "custom"]
                            },
                            "api_key": {
                                "type": "string",
                                "title": "API Key",
                                "description": "API key for authentication"
                            },
                            "base_url": {
                                "type": "string",
                                "title": "Base URL",
                                "description": "Custom API base URL"
                            },
                            "max_tokens": {
                                "type": "integer",
                                "title": "Max Tokens",
                                "description": "Maximum tokens for responses",
                                "default": 4096
                            },
                            "temperature": {
                                "type": "number",
                                "title": "Temperature",
                                "description": "Response randomness (0-2)",
                                "default": 0.7,
                                "minimum": 0,
                                "maximum": 2
                            },
                            "default": {
                                "type": "boolean",
                                "title": "Default Model",
                                "description": "Use as default model",
                                "default": false
                            },
                            "cost_input_per_m": {
                                "type": "number",
                                "title": "Input Cost per 1M tokens",
                                "description": "Cost in USD per million input tokens"
                            },
                            "cost_output_per_m": {
                                "type": "number",
                                "title": "Output Cost per 1M tokens",
                                "description": "Cost in USD per million output tokens"
                            },
                            "cost_cache_read_per_m": {
                                "type": "number",
                                "title": "Cache Read Cost per 1M tokens",
                                "description": "Cost in USD per million cached input tokens"
                            },
                            "cost_cache_write_per_m": {
                                "type": "number",
                                "title": "Cache Write Cost per 1M tokens",
                                "description": "Cost in USD per million cache write tokens"
                            },
                            "context_window": {
                                "type": "integer",
                                "title": "Context Window",
                                "description": "Maximum context window size in tokens"
                            },
                            "max_output_tokens": {
                                "type": "integer",
                                "title": "Max Output Tokens",
                                "description": "Maximum output tokens the model supports"
                            },
                            "auth_type": {
                                "type": "string",
                                "title": "Auth Type",
                                "description": "Authentication method for this provider",
                                "enum": ["api_key", "bearer", "aws_sdk", "oauth", "custom_header"],
                                "default": "api_key"
                            },
                            "custom_headers": {
                                "type": "object",
                                "title": "Custom Headers",
                                "description": "Additional HTTP headers to send with requests",
                                "additionalProperties": { "type": "string" }
                            }
                        }
                    }
                },
                "channels": {
                    "type": "object",
                    "title": "Channels",
                    "description": "Messaging channels (Telegram, Discord, Slack, etc.)",
                    "properties": {
                        "discord": {
                            "type": "object",
                            "title": "Discord",
                            "description": "Discord bot configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "bot_token": {
                                    "type": "string",
                                    "title": "Bot Token",
                                    "description": "Discord bot token"
                                },
                                "application_id": {
                                    "type": "string",
                                    "title": "Application ID",
                                    "description": "Discord application ID"
                                }
                            }
                        },
                        "telegram": {
                            "type": "object",
                            "title": "Telegram",
                            "description": "Telegram bot configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "bot_token": {
                                    "type": "string",
                                    "title": "Bot Token",
                                    "description": "Telegram bot token"
                                }
                            }
                        },
                        "slack": {
                            "type": "object",
                            "title": "Slack",
                            "description": "Slack bot configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "bot_token": {
                                    "type": "string",
                                    "title": "Bot Token",
                                    "description": "Slack bot token"
                                },
                                "signing_secret": {
                                    "type": "string",
                                    "title": "Signing Secret",
                                    "description": "Slack signing secret"
                                }
                            }
                        },
                        "whatsapp": {
                            "type": "object",
                            "title": "WhatsApp",
                            "description": "WhatsApp Business configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "phone_number_id": {
                                    "type": "string",
                                    "title": "Phone Number ID"
                                },
                                "access_token": {
                                    "type": "string",
                                    "title": "Access Token"
                                }
                            }
                        },
                        "signal": {
                            "type": "object",
                            "title": "Signal",
                            "description": "Signal messaging configuration",
                            "properties": {
                                "enabled": {
                                    "type": "boolean",
                                    "title": "Enabled",
                                    "default": false
                                },
                                "phone_number": {
                                    "type": "string",
                                    "title": "Phone Number"
                                }
                            }
                        }
                    }
                },
                "cron": {
                    "type": "object",
                    "title": "Cron",
                    "description": "Scheduled tasks and automation",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "title": "Enabled",
                                "default": true
                            },
                            "schedule": {
                                "type": "string",
                                "title": "Schedule",
                                "description": "Cron expression (e.g., '0 9 * * *' for daily at 9am)"
                            },
                            "command": {
                                "type": "string",
                                "title": "Command",
                                "description": "Command to execute"
                            },
                            "channel": {
                                "type": "string",
                                "title": "Channel",
                                "description": "Target channel ID"
                            }
                        }
                    }
                },
                "memory": {
                    "type": "object",
                    "title": "Memory",
                    "description": "Memory and knowledge storage",
                    "properties": {
                        "enabled": {
                            "type": "boolean",
                            "title": "Enabled",
                            "default": true
                        },
                        "provider": {
                            "type": "string",
                            "title": "Provider",
                            "description": "Embedding provider",
                            "enum": ["openai", "voyage", "gemini", "ollama"]
                        },
                        "embedding_model": {
                            "type": "string",
                            "title": "Embedding Model",
                            "description": "Model for embeddings",
                            "default": "text-embedding-3-small"
                        },
                        "persist": {
                            "type": "boolean",
                            "title": "Persist",
                            "description": "Persist memory to disk",
                            "default": true
                        }
                    }
                },
                "audio": {
                    "type": "object",
                    "title": "Audio",
                    "description": "Audio input/output settings",
                    "properties": {
                        "tts_enabled": {
                            "type": "boolean",
                            "title": "TTS Enabled",
                            "default": false
                        },
                        "tts_provider": {
                            "type": "string",
                            "title": "TTS Provider",
                            "enum": ["openai", "elevenlabs", "browser"]
                        },
                        "voice_wake_enabled": {
                            "type": "boolean",
                            "title": "Voice Wake Enabled",
                            "default": false
                        },
                        "wake_word": {
                            "type": "string",
                            "title": "Wake Word",
                            "default": "hey savfox"
                        }
                    }
                },
                "logging": {
                    "type": "object",
                    "title": "Logging",
                    "description": "Log levels and output configuration",
                    "properties": {
                        "level": {
                            "type": "string",
                            "title": "Log Level",
                            "enum": ["trace", "debug", "info", "warn", "error"],
                            "default": "info"
                        },
                        "file": {
                            "type": "string",
                            "title": "Log File",
                            "description": "Path to log file"
                        },
                        "max_size_mb": {
                            "type": "integer",
                            "title": "Max Size (MB)",
                            "default": 10
                        }
                    }
                },
                "tools": {
                    "type": "object",
                    "title": "Tools",
                    "description": "Tool configurations (browser, search, etc.)",
                    "properties": {
                        "browser_enabled": {
                            "type": "boolean",
                            "title": "Browser Enabled",
                            "default": false
                        },
                        "search_enabled": {
                            "type": "boolean",
                            "title": "Search Enabled",
                            "default": false
                        },
                        "search_provider": {
                            "type": "string",
                            "title": "Search Provider",
                            "enum": ["google", "bing", "duckduckgo", "searx"]
                        }
                    }
                },
                "session": {
                    "type": "object",
                    "title": "Session",
                    "description": "Session management and persistence",
                    "properties": {
                        "auto_save": {
                            "type": "boolean",
                            "title": "Auto Save",
                            "default": true
                        },
                        "max_history": {
                            "type": "integer",
                            "title": "Max History",
                            "description": "Maximum messages to keep in history",
                            "default": 100
                        }
                    }
                },
                "plugins": {
                    "type": "object",
                    "title": "Plugins",
                    "description": "Plugin management and extensions",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "title": "Enabled",
                                "default": true
                            },
                            "config": {
                                "type": "object",
                                "title": "Configuration"
                            }
                        }
                    }
                }
            }
        },
        "uiHints": {
            "gateway.host": { "order": 1 },
            "gateway.port": { "order": 2 },
            "gateway.token": { "order": 3, "sensitive": true },
            "gateway.tls_cert": { "order": 4 },
            "gateway.tls_key": { "order": 5, "sensitive": true },
            "agents.*.model": { "order": 1 },
            "agents.*.provider": { "order": 2 },
            "models.*.provider": { "order": 1 },
            "models.*.api_key": { "order": 2, "sensitive": true },
            "models.*.base_url": { "order": 3 },
            "channels.*.enabled": { "order": 1 },
            "channels.*.bot_token": { "order": 2, "sensitive": true },
            "hooks.token": { "order": 3, "sensitive": true },
            "talk.api_key": { "order": 5, "sensitive": true },
            "skills.entries.*.api_key": { "order": 2, "sensitive": true },
            "memory.enabled": { "order": 1 },
            "memory.provider": { "order": 2 },
            "logging.level": { "order": 1 }
        }
    }))
}

// ── Usage ───────────────────────────────────────────────────────────────────

pub(crate) async fn handle_usage_status(session_store: &Arc<SessionStore>) -> RpcResult {
    let sessions = session_store.list().await;
    let total_input: u64 = sessions.iter().map(|s| s.input_tokens).sum();
    let total_output: u64 = sessions.iter().map(|s| s.output_tokens).sum();
    let total: u64 = sessions.iter().map(|s| s.total_tokens).sum();

    // Build hourly distribution from session update times.
    let mut hourly = vec![0u64; 24];
    for s in &sessions {
        let secs = s.updated_at / 1000;
        let hour = ((secs % 86400) / 3600) as usize;
        if hour < 24 {
            hourly[hour] += 1;
        }
    }

    Ok(json!({
        "total_tokens": total,
        "prompt_tokens": total_input,
        "completion_tokens": total_output,
        "session_count": sessions.len(),
        "total_messages": null,
        "tool_calls": null,
        "errors": null,
        "cache_hits": null,
        "cache_misses": null,
        "hourly_distribution": hourly,
    }))
}

pub(crate) async fn handle_usage_cost(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let period = opt_str(params, "period", "all");
    let session_id = params.get("session_id").and_then(|v| v.as_str());

    if let Some(key) = session_id {
        // Per-session usage.
        match session_store.get(key).await {
            Some(entry) => Ok(json!({
                "period": period,
                "session_id": key,
                "input_tokens": entry.input_tokens,
                "output_tokens": entry.output_tokens,
                "total_tokens": entry.total_tokens,
            })),
            None => Ok(json!({
                "period": period,
                "session_id": key,
                "total_tokens": 0,
            })),
        }
    } else {
        // Return per-session entries with token breakdown.
        let sessions = session_store.list().await;

        // Time filtering based on period.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let cutoff_ms = match period {
            "today" => now_ms.saturating_sub(24 * 60 * 60 * 1000),
            "week" => now_ms.saturating_sub(7 * 24 * 60 * 60 * 1000),
            "month" => now_ms.saturating_sub(30 * 24 * 60 * 60 * 1000),
            _ => 0, // "all"
        };

        let entries: Vec<Value> = sessions
            .iter()
            .filter(|s| s.updated_at >= cutoff_ms || cutoff_ms == 0)
            .filter(|s| s.total_tokens > 0)
            .map(|s| {
                json!({
                    "session_id": s.session_id,
                    "model": s.model,
                    "tokens": s.total_tokens,
                    "input_tokens": s.input_tokens,
                    "output_tokens": s.output_tokens,
                    "cost": null,
                })
            })
            .collect();

        let total_tokens: u64 = entries
            .iter()
            .filter_map(|e| e.get("tokens").and_then(|v| v.as_u64()))
            .sum();
        let total_input: u64 = entries
            .iter()
            .filter_map(|e| e.get("input_tokens").and_then(|v| v.as_u64()))
            .sum();
        let total_output: u64 = entries
            .iter()
            .filter_map(|e| e.get("output_tokens").and_then(|v| v.as_u64()))
            .sum();

        Ok(json!({
            "period": period,
            "total_tokens": total_tokens,
            "prompt_tokens": total_input,
            "completion_tokens": total_output,
            "total_sessions": entries.len(),
            "entries": entries,
        }))
    }
}

// ── Logs ────────────────────────────────────────────────────────────────────

pub(crate) async fn handle_logs_tail(params: &Value) -> RpcResult {
    let lines = opt_u64(params, "lines", 50);
    let entries = log_store::list_logs(lines as usize).await;
    let value = serde_json::to_value(entries).unwrap_or(json!([]));
    Ok(json!({ "lines": lines, "entries": value }))
}

// ── System ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentHeartbeatSettings {
    enabled: bool,
    interval_ms: u64,
    coalesce_window_ms: u64,
    #[serde(default)]
    cron_job_ids: Vec<String>,
}

impl Default for AgentHeartbeatSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_ms: 30_000,
            coalesce_window_ms: 30_000,
            cron_job_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HeartbeatSettingsDocument {
    #[serde(default)]
    agents: HashMap<String, AgentHeartbeatSettings>,
}

#[derive(Debug, Clone, Default)]
struct PendingHeartbeatEvent {
    event_type: String,
    text: String,
    timestamp: String,
}

#[derive(Debug, Clone, Default)]
struct HeartbeatRuntimeState {
    last_delivered_ms: u64,
    pending: Option<PendingHeartbeatEvent>,
    flush_scheduled: bool,
}

fn heartbeat_runtime_store() -> &'static Mutex<HashMap<String, HeartbeatRuntimeState>> {
    static STORE: OnceLock<Mutex<HashMap<String, HeartbeatRuntimeState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn heartbeat_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn heartbeat_agent_from_params(params: &Value) -> String {
    params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_owned()
}

async fn load_heartbeat_settings(channel: &GatewayChannel) -> HeartbeatSettingsDocument {
    let path = heartbeat_config_path(channel);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_owned());
    serde_json::from_str::<HeartbeatSettingsDocument>(&content).unwrap_or_default()
}

async fn save_heartbeat_settings(
    channel: &GatewayChannel,
    settings: &HeartbeatSettingsDocument,
) -> Result<(), String> {
    let path = heartbeat_config_path(channel);
    let content = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("serialize heartbeat settings failed: {e}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|e| format!("write heartbeat settings failed: {e}"))
}

async fn heartbeat_settings_for_agent(
    channel: &GatewayChannel,
    agent_id: &str,
) -> AgentHeartbeatSettings {
    let settings = load_heartbeat_settings(channel).await;
    settings
        .agents
        .get(agent_id)
        .cloned()
        .or_else(|| settings.agents.get("*").cloned())
        .unwrap_or_default()
}

pub(crate) async fn handle_last_heartbeat(params: &Value) -> RpcResult {
    let agent_id = heartbeat_agent_from_params(params);
    let state = heartbeat_runtime_store().lock().await;
    let snapshot = state.get(&agent_id).cloned().unwrap_or_default();
    let timestamp = if snapshot.last_delivered_ms == 0 {
        chrono::Utc::now().to_rfc3339()
    } else {
        chrono::DateTime::from_timestamp_millis(snapshot.last_delivered_ms as i64)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
    };
    Ok(json!({
        "agent": agent_id,
        "timestamp": timestamp,
        "last_delivered_ms": snapshot.last_delivered_ms,
        "has_pending": snapshot.pending.is_some(),
    }))
}

pub(crate) async fn handle_set_heartbeats(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = heartbeat_agent_from_params(params);
    let enabled = opt_bool(params, "enabled", true);
    let interval_ms = opt_u64(params, "interval_ms", 30000);
    let coalesce_window_ms = opt_u64(params, "coalesce_window_ms", 30000);

    let cron_job_ids = params
        .get("cron_job_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(str::trim))
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        });

    let mut settings = load_heartbeat_settings(channel).await;
    let entry = settings.agents.entry(agent_id.clone()).or_default();
    entry.enabled = enabled;
    entry.interval_ms = interval_ms.max(1000);
    entry.coalesce_window_ms = coalesce_window_ms.max(1000);
    if let Some(cron_job_ids) = cron_job_ids {
        entry.cron_job_ids = cron_job_ids;
    }
    let response = entry.clone();

    save_heartbeat_settings(channel, &settings)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({
        "agent": agent_id,
        "enabled": response.enabled,
        "interval_ms": response.interval_ms,
        "coalesce_window_ms": response.coalesce_window_ms,
        "cron_job_ids": response.cron_job_ids,
    }))
}

pub(crate) async fn handle_system_presence(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let status = opt_str(params, "status", "online");

    // Broadcast presence change to all connected clients.
    session_mgr
        .broadcast_to_all(
            "system.presence",
            json!({ "status": status, "timestamp": chrono::Utc::now().to_rfc3339() }),
        )
        .await;

    Ok(json!({ "status": status }))
}

pub(crate) async fn handle_system_event(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
    cron_service: &Arc<CronService>,
) -> RpcResult {
    let event_type = opt_str(params, "type", "unknown");
    let text = opt_str(params, "text", "");
    let heartbeat = opt_bool(params, "heartbeat", false);

    if heartbeat {
        let agent_id = heartbeat_agent_from_params(params);
        let settings = heartbeat_settings_for_agent(channel, &agent_id).await;
        if !settings.enabled {
            return Ok(json!({
                "type": event_type,
                "received": true,
                "heartbeat": true,
                "agent": agent_id,
                "delivered": false,
                "reason": "heartbeat disabled",
            }));
        }

        let now_ms = heartbeat_now_ms();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let mut should_flush = false;
        let mut flush_delay_ms = settings.coalesce_window_ms;
        let mut coalesced = false;
        {
            let mut store = heartbeat_runtime_store().lock().await;
            let state = store.entry(agent_id.clone()).or_default();
            let elapsed = now_ms.saturating_sub(state.last_delivered_ms);
            if elapsed < settings.coalesce_window_ms {
                state.pending = Some(PendingHeartbeatEvent {
                    event_type: event_type.to_owned(),
                    text: text.to_owned(),
                    timestamp: timestamp.clone(),
                });
                if !state.flush_scheduled {
                    state.flush_scheduled = true;
                    should_flush = true;
                    flush_delay_ms = settings.coalesce_window_ms.saturating_sub(elapsed).max(1);
                }
                coalesced = true;
            } else {
                state.last_delivered_ms = now_ms;
                state.pending = None;
            }
        }

        if coalesced {
            if should_flush {
                schedule_heartbeat_flush(
                    agent_id.clone(),
                    flush_delay_ms,
                    Arc::clone(session_mgr),
                    Arc::clone(channel),
                    Arc::clone(cron_service),
                );
            }
            return Ok(json!({
                "type": event_type,
                "received": true,
                "heartbeat": true,
                "agent": agent_id,
                "delivered": false,
                "coalesced": true,
                "window_ms": settings.coalesce_window_ms,
            }));
        }

        broadcast_heartbeat_event(session_mgr, event_type, text, &timestamp).await;
        trigger_heartbeat_cron_jobs(&agent_id, &settings, channel, cron_service).await;

        return Ok(json!({
            "type": event_type,
            "received": true,
            "heartbeat": true,
            "agent": agent_id,
            "delivered": true,
            "coalesced": false,
            "timestamp": timestamp,
        }));
    }

    // Broadcast the system event to all connected WebSocket clients.
    session_mgr
        .broadcast_to_all(
            "system.event",
            json!({
                "type": event_type,
                "text": text,
                "heartbeat": heartbeat,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    // If text is provided, inject it into the main agent session.
    if !text.is_empty() {
        match channel.invoke_agent_text(text, "default").await {
            Ok(reply) => {
                return Ok(json!({
                    "type": event_type,
                    "received": true,
                    "response": reply,
                }));
            }
            Err(err) => {
                return Ok(json!({
                    "type": event_type,
                    "received": true,
                    "error": format!("{err}"),
                }));
            }
        }
    }

    Ok(json!({ "type": event_type, "received": true }))
}

fn schedule_heartbeat_flush(
    agent_id: String,
    delay_ms: u64,
    session_mgr: Arc<GatewaySessionManager>,
    channel: Arc<GatewayChannel>,
    cron_service: Arc<CronService>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let pending = {
            let mut store = heartbeat_runtime_store().lock().await;
            let state = store.entry(agent_id.clone()).or_default();
            state.flush_scheduled = false;
            let pending = state.pending.take();
            if pending.is_some() {
                state.last_delivered_ms = heartbeat_now_ms();
            }
            pending
        };

        let Some(pending) = pending else {
            return;
        };

        broadcast_heartbeat_event(
            &session_mgr,
            &pending.event_type,
            &pending.text,
            &pending.timestamp,
        )
        .await;

        let settings = heartbeat_settings_for_agent(&channel, &agent_id).await;
        trigger_heartbeat_cron_jobs(&agent_id, &settings, &channel, &cron_service).await;
    });
}

async fn broadcast_heartbeat_event(
    session_mgr: &Arc<GatewaySessionManager>,
    event_type: &str,
    text: &str,
    timestamp: &str,
) {
    session_mgr
        .broadcast_to_all(
            "system.event",
            json!({
                "type": event_type,
                "text": text,
                "heartbeat": true,
                "timestamp": timestamp,
            }),
        )
        .await;
}

async fn trigger_heartbeat_cron_jobs(
    agent_id: &str,
    settings: &AgentHeartbeatSettings,
    channel: &Arc<GatewayChannel>,
    cron_service: &Arc<CronService>,
) {
    for job_id in &settings.cron_job_ids {
        if let Err(err) = cron_service.run_job(job_id, channel).await {
            tracing::warn!(
                agent = agent_id,
                cron_job_id = %job_id,
                "heartbeat cron trigger failed: {err}"
            );
        }
    }
}

// ── System instance management ─────────────────────────────────────────────

/// Disconnect a connected instance (graceful close).
pub(crate) async fn handle_system_disconnect(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    use savfox_protocol::SessionId;

    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or((INVALID_REQUEST, "missing session_id".to_owned()))?;

    let session_id = SessionId::from_string(session_id_str)
        .map_err(|e| (INVALID_REQUEST, format!("invalid session_id: {e}")))?;

    session_mgr.remove_session(&session_id).await;

    Ok(json!({
        "disconnected": true,
        "session_id": session_id_str,
    }))
}

/// Kick a connected instance (forceful removal).
pub(crate) async fn handle_system_kick(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    use savfox_protocol::SessionId;

    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or((INVALID_REQUEST, "missing session_id".to_owned()))?;

    let reason = opt_str(params, "reason", "kicked by operator");

    let session_id = SessionId::from_string(session_id_str)
        .map_err(|e| (INVALID_REQUEST, format!("invalid session_id: {e}")))?;

    // Notify the session before removing it.
    if let Some(session_arc) = session_mgr.get_session(&session_id).await {
        let session = session_arc.read().await;
        let msg = crate::protocol::GatewayMessage::Event {
            event: "system.kicked".to_owned(),
            payload: json!({ "reason": reason }),
            seq: Some(session.next_seq()),
        };
        let _ = session.sender.try_send(msg);
    }

    session_mgr.remove_session(&session_id).await;

    Ok(json!({
        "kicked": true,
        "session_id": session_id_str,
        "reason": reason,
    }))
}

/// Return the current execution approval policy.
pub(crate) async fn handle_approvals_policy(
    _params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    // Re-use existing exec approvals get handler logic.
    super::skill::handle_exec_approvals_get(channel).await
}
