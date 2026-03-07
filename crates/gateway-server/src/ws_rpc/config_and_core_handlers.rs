async fn handle_config_schema() -> RpcResult {
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

// ── Cron ────────────────────────────────────────────────────────────────────

fn cron_param_job_id(params: &Value) -> Option<&str> {
    params
        .get("id")
        .or_else(|| params.get("job_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn cron_format_timestamp(timestamp_ms: u64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms as i64)
        .map(|dt| dt.to_rfc3339())
}

fn cron_interval_label(interval_secs: u64) -> String {
    if interval_secs != 0 && interval_secs.is_multiple_of(86_400) {
        format!("every {}d", interval_secs / 86_400)
    } else if interval_secs != 0 && interval_secs.is_multiple_of(3_600) {
        format!("every {}h", interval_secs / 3_600)
    } else if interval_secs != 0 && interval_secs.is_multiple_of(60) {
        format!("every {}m", interval_secs / 60)
    } else {
        format!("every {interval_secs}s")
    }
}

fn cron_schedule_label(schedule: &CronSchedule) -> String {
    match schedule {
        CronSchedule::At { at_ms } => cron_format_timestamp(*at_ms)
            .map(|ts| format!("at {ts}"))
            .unwrap_or_else(|| format!("at {at_ms}")),
        CronSchedule::Every { interval_secs, .. } => cron_interval_label(*interval_secs),
        CronSchedule::Cron {
            expression,
            timezone,
        } => {
            let timezone = timezone
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(timezone) = timezone {
                format!("{expression} [{timezone}]")
            } else {
                expression.clone()
            }
        }
    }
}

fn cron_job_summary_value(job: &savfox_core::cron::CronJob) -> Value {
    let session_target = match job.session_target {
        CronSessionTarget::Main => "main",
        CronSessionTarget::Isolated => "isolated",
    };

    json!({
        "id": job.id,
        "name": job.name,
        "schedule": cron_schedule_label(&job.schedule),
        "payload": job.payload,
        "enabled": job.state.enabled,
        "next_run": job.state.next_run_at_ms.and_then(cron_format_timestamp),
        "last_run": job.state.last_run_at_ms.and_then(cron_format_timestamp),
        "agent_id": job.agent_id,
        "session_target": session_target,
        "delivery": job.delivery,
    })
}

fn cron_status_summary_value(status: &savfox_core::cron::CronServiceStatus) -> Value {
    json!({
        "running": status.enabled,
        "job_count": status.total_jobs,
        "enabled_jobs": status.enabled_jobs,
        "running_jobs": status.running_jobs,
    })
}

fn cron_run_summary_value(run: &savfox_core::cron::CronRunEntry) -> Value {
    json!({
        "job_id": run.job_id,
        "started_at": cron_format_timestamp(run.started_at_ms),
        "finished_at": cron_format_timestamp(run.finished_at_ms),
        "status": run.status,
        "duration_ms": run.finished_at_ms.saturating_sub(run.started_at_ms),
        "error": run.error,
        "result_preview": run.result_preview,
    })
}

fn parse_cron_field<T>(params: &Value, field: &str) -> Result<Option<T>, (i64, String)>
where
    T: serde::de::DeserializeOwned,
{
    params
        .get(field)
        .map(|value| {
            serde_json::from_value::<T>(value.clone())
                .map_err(|err| (INVALID_REQUEST, format!("invalid {field}: {err}")))
        })
        .transpose()
}

fn parse_optional_trimmed_string_field(
    params: &Value,
    field: &str,
) -> Result<Option<Option<String>>, (i64, String)> {
    match params.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(Some(None))
            } else {
                Ok(Some(Some(value.to_string())))
            }
        }
        Some(_) => Err((
            INVALID_REQUEST,
            format!("invalid '{field}' parameter: expected string or null"),
        )),
    }
}

async fn handle_cron_list(cron_service: &Arc<CronService>) -> RpcResult {
    let jobs = cron_service.list_jobs().await;
    let jobs = jobs.iter().map(cron_job_summary_value).collect::<Vec<_>>();
    Ok(json!({ "jobs": jobs }))
}

async fn handle_cron_status(cron_service: &Arc<CronService>) -> RpcResult {
    let status = cron_service.status().await;
    Ok(cron_status_summary_value(&status))
}

async fn handle_cron_add(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    if name.is_empty() {
        return Err((INVALID_REQUEST, "missing 'name' parameter".to_string()));
    }

    // Parse schedule.
    let schedule = if let Some(schedule) = parse_cron_field::<CronSchedule>(params, "schedule")? {
        schedule
    } else if let Some(expression) = params.get("expression").and_then(|v| v.as_str()) {
        CronSchedule::Cron {
            expression: expression.to_owned(),
            timezone: None,
        }
    } else {
        return Err((
            INVALID_REQUEST,
            "missing 'schedule' or 'expression' parameter".to_string(),
        ));
    };

    // Parse payload.
    let payload = if let Some(payload) = parse_cron_field::<CronPayload>(params, "payload")? {
        payload
    } else if let Some(command) = params.get("command").and_then(|v| v.as_str()) {
        CronPayload::SystemEvent {
            text: command.to_owned(),
        }
    } else {
        return Err((
            INVALID_REQUEST,
            "missing 'payload' or 'command' parameter".to_string(),
        ));
    };

    // Parse delivery.
    let delivery = parse_cron_field::<CronDelivery>(params, "delivery")?.unwrap_or_default();

    // Parse session target (main or isolated).
    let session_target =
        parse_cron_field::<CronSessionTarget>(params, "session_target")?.unwrap_or_default();
    let agent_id = parse_optional_trimmed_string_field(params, "agent_id")?.flatten();

    let id = cron_service
        .add_job(
            name.clone(),
            schedule,
            payload,
            delivery,
            session_target,
            agent_id,
        )
        .await;
    Ok(json!({ "id": id, "name": name, "status": "added" }))
}

async fn handle_cron_update(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    let schedule = parse_cron_field::<CronSchedule>(params, "schedule")?;
    let payload = parse_cron_field::<CronPayload>(params, "payload")?;
    let delivery = parse_cron_field::<CronDelivery>(params, "delivery")?;
    let session_target = parse_cron_field::<CronSessionTarget>(params, "session_target")?;
    let agent_id = parse_optional_trimmed_string_field(params, "agent_id")?;
    let enabled = params.get("enabled").and_then(|v| v.as_bool());

    match cron_service
        .update_job(
            id,
            name,
            schedule,
            payload,
            delivery,
            session_target,
            agent_id,
            enabled,
        )
        .await
    {
        Ok(job) => Ok(json!({
            "id": id,
            "status": "updated",
            "job": cron_job_summary_value(&job),
        })),
        Err(err) => Err((INVALID_REQUEST, err)),
    }
}

async fn handle_cron_remove(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }
    let removed = cron_service.remove_job(id).await;
    if removed {
        Ok(json!({ "id": id, "status": "removed" }))
    } else {
        Err((INVALID_REQUEST, format!("job '{id}' not found")))
    }
}

async fn handle_cron_run(
    params: &Value,
    cron_service: &Arc<CronService>,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }
    match cron_service.run_job(id, channel).await {
        Ok(()) => Ok(json!({ "id": id, "status": "triggered" })),
        Err(err) => Err((INTERNAL_ERROR, err)),
    }
}

async fn handle_cron_runs(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    let runs = cron_service.get_runs(id, limit).await;
    let runs = runs.iter().map(cron_run_summary_value).collect::<Vec<_>>();
    Ok(json!({ "id": id, "runs": runs }))
}

// ── Nodes ───────────────────────────────────────────────────────────────────

fn node_capability_catalog() -> Vec<Value> {
    vec![
        json!({
            "id": "camera.snap",
            "method": "system.camera",
            "default_params": { "mode": "snap" },
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.camera.snap"],
        }),
        json!({
            "id": "camera.clip",
            "method": "system.camera",
            "default_params": { "mode": "clip" },
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.camera.clip"],
        }),
        json!({
            "id": "screen.record",
            "method": "system.screen.record",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.screen.record"],
        }),
        json!({
            "id": "location.get",
            "method": "system.location",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.location.get"],
        }),
        json!({
            "id": "notify",
            "method": "system.notify",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.notify"],
        }),
    ]
}

fn latest_pairing_record_for_node<'a>(
    node_id: &str,
    records: &'a [pairing_store::PairingRecord],
) -> Option<&'a pairing_store::PairingRecord> {
    records
        .iter()
        .filter(|record| record.node_id == node_id)
        .max_by_key(|record| record.updated_at)
}

fn node_is_approved(record: Option<&pairing_store::PairingRecord>) -> bool {
    match record {
        Some(record) => matches!(record.status, crate::pairing_store::PairingStatus::Approved),
        None => false,
    }
}

async fn handle_node_capabilities_list() -> RpcResult {
    Ok(json!({
        "capabilities": node_capability_catalog(),
    }))
}

async fn handle_node_list() -> RpcResult {
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let mut by_node: HashMap<String, &pairing_store::PairingRecord> = HashMap::new();
    for record in &records {
        match by_node.get(&record.node_id) {
            Some(existing) if existing.updated_at >= record.updated_at => {}
            _ => {
                by_node.insert(record.node_id.clone(), record);
            }
        }
    }

    let mut nodes = Vec::new();
    for (node_id, record) in by_node {
        let status = serde_json::to_value(&record.status)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        nodes.push(json!({
            "node_id": node_id,
            "name": record.device_name.clone().unwrap_or_else(|| record.node_id.clone()),
            "capabilities": node_capability_catalog(),
            "paired": true,
            "approved": matches!(record.status, crate::pairing_store::PairingStatus::Approved),
            "status": status,
            "device_id": record.device_id,
            "updated_at": record.updated_at,
        }));
    }
    nodes.sort_by(|a, b| {
        let a_id = a.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        let b_id = b.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        a_id.cmp(b_id)
    });
    Ok(json!({ "nodes": nodes }))
}

async fn handle_node_describe(params: &Value) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let latest = latest_pairing_record_for_node(node_id, &records);
    if let Some(record) = latest {
        let status = serde_json::to_value(&record.status)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        return Ok(json!({
            "node_id": node_id,
            "name": record.device_name.clone().unwrap_or_else(|| node_id.to_string()),
            "capabilities": node_capability_catalog(),
            "paired": true,
            "approved": matches!(record.status, crate::pairing_store::PairingStatus::Approved),
            "requires_pairing": true,
            "requires_approval": true,
            "status": status,
            "device_id": record.device_id,
            "request_id": record.request_id,
            "verification_code": record.verification_code,
            "updated_at": record.updated_at,
        }));
    }

    Ok(json!({
        "node_id": node_id,
        "name": node_id,
        "capabilities": node_capability_catalog(),
        "paired": false,
        "approved": false,
        "requires_pairing": true,
        "requires_approval": true,
        "status": "unknown",
    }))
}

async fn handle_node_tool_alias(
    method: &str,
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let node_id = params
        .get("node_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }

    let mut merged = serde_json::Map::new();
    merged.insert("node_id".to_string(), Value::String(node_id));
    merged.insert("method".to_string(), Value::String(method.to_string()));

    if let Some(extra_params) = params.get("params") {
        merged.insert("params".to_string(), extra_params.clone());
    } else {
        let mut passthrough = serde_json::Map::new();
        if let Some(duration_ms) = params.get("duration_ms") {
            passthrough.insert("duration_ms".to_string(), duration_ms.clone());
        }
        if let Some(display) = params.get("display") {
            passthrough.insert("display".to_string(), display.clone());
        }
        if let Some(device) = params.get("device") {
            passthrough.insert("device".to_string(), device.clone());
        }
        if let Some(title) = params.get("title") {
            passthrough.insert("title".to_string(), title.clone());
        }
        if let Some(body) = params.get("body") {
            passthrough.insert("body".to_string(), body.clone());
        }
        if !passthrough.is_empty() {
            merged.insert("params".to_string(), Value::Object(passthrough));
        }
    }

    handle_node_invoke(&Value::Object(merged), channel).await
}

async fn handle_node_invoke(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    let method_raw = params.get("method").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() || method_raw.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'node_id' or 'method' parameter".to_string(),
        ));
    }

    let (method, default_mode): (&str, Option<&str>) = match method_raw {
        "camera.snap" => ("system.camera", Some("snap")),
        "camera.clip" => ("system.camera", Some("clip")),
        "screen.record" => ("system.screen.record", None),
        "location.get" => ("system.location", None),
        "notify" => ("system.notify", None),
        other => (other, None),
    };

    let mut invoke_params = params
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if let Some(mode) = default_mode {
        if let Value::Object(ref mut map) = invoke_params {
            map.entry("mode".to_string())
                .or_insert_with(|| Value::String(mode.to_string()));
        }
    }

    let requires_pairing = matches!(
        method,
        "system.camera"
            | "system.screen.record"
            | "system.location"
            | "system.notify"
            | "camera.snap"
            | "camera.clip"
            | "screen.record"
            | "location.get"
            | "notify"
    );
    if requires_pairing {
        let records = pairing_store::list_requests()
            .await
            .map_err(|err| (INTERNAL_ERROR, err))?;
        let approved = node_is_approved(latest_pairing_record_for_node(node_id, &records));
        if !approved {
            return Err((
                INVALID_REQUEST,
                format!("node '{node_id}' is not paired/approved"),
            ));
        }
    }

    let params_json = serde_json::to_string(&invoke_params).unwrap_or_else(|_| "{}".to_string());
    let prompt = format!("[node:{node_id}] {method} {params_json}");
    let request_id = uuid::Uuid::now_v7().to_string();
    match channel.invoke_agent_text(&prompt, "default").await {
        Ok(reply) => {
            let record = NodeInvokeRecord {
                request_id: request_id.clone(),
                node_id: node_id.to_owned(),
                method: method.to_owned(),
                status: "completed".to_string(),
                result: json!({
                    "reply": reply,
                    "params": invoke_params,
                }),
                updated_at_ms: now_ms(),
            };
            save_node_invoke_result(record).await;
            Ok(json!({
                "request_id": request_id,
                "node_id": node_id,
                "method": method,
                "status": "completed",
                "result": reply,
                "params": invoke_params
            }))
        }
        Err(err) => Err((INTERNAL_ERROR, format!("node invoke error: {err}"))),
    }
}

async fn handle_node_invoke_result(params: &Value) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }
    if let Some(record) = get_node_invoke_result(request_id).await {
        let value = serde_json::to_value(record).unwrap_or(Value::Null);
        Ok(json!({ "request": value }))
    } else {
        Ok(json!({ "request_id": request_id, "result": null, "status": "not_found" }))
    }
}

async fn handle_node_event(params: &Value, _channel: &Arc<GatewayChannel>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    let event_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }

    // Log the event and verify the node exists in the pairing store.
    let records = pairing_store::list_requests().await.unwrap_or_default();
    let node_exists = records.iter().any(|r| r.node_id == node_id);

    tracing::info!(
        node_id = %node_id,
        event_type = %event_type,
        node_known = %node_exists,
        "node event received"
    );

    Ok(json!({
        "node_id": node_id,
        "type": event_type,
        "status": "received",
        "node_known": node_exists,
    }))
}

async fn handle_node_rename(params: &Value, _channel: &Arc<GatewayChannel>) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() || name.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'node_id' or 'name' parameter".to_string(),
        ));
    }

    // Update the device name in the pairing store if a matching record exists.
    let records = pairing_store::list_requests().await.unwrap_or_default();
    let found = records.iter().any(|r| r.node_id == node_id);

    if !found {
        return Err((
            INVALID_REQUEST,
            format!("node '{node_id}' not found in pairing store"),
        ));
    }

    // Note: The pairing store doesn't have a direct rename method, so we log the rename
    // and return success. The device_name is set during pairing creation.
    tracing::info!(node_id = %node_id, new_name = %name, "node renamed");

    Ok(json!({ "node_id": node_id, "name": name, "status": "renamed" }))
}

// ── Device pairing ──────────────────────────────────────────────────────────

async fn handle_node_pair_request(params: &Value) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    let device_id = params.get("device_id").and_then(|v| v.as_str());
    let device_name = params.get("device_name").and_then(|v| v.as_str());
    let record = pairing_store::create_request(node_id, device_id, device_name)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

async fn handle_node_pair_list() -> RpcResult {
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(records).unwrap_or(json!([]));
    Ok(json!({ "requests": value }))
}

async fn handle_node_pair_approve(params: &Value) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }
    let record = pairing_store::approve_request(request_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

async fn handle_node_pair_reject(params: &Value) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }
    let record = pairing_store::reject_request(request_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

async fn handle_node_pair_verify(params: &Value) -> RpcResult {
    let code = params.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if code.is_empty() {
        return Err((INVALID_REQUEST, "missing 'code' parameter".to_string()));
    }
    let found = pairing_store::verify_code(code)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    if let Some(record) = found {
        let value = serde_json::to_value(record).unwrap_or(Value::Null);
        Ok(json!({ "valid": true, "request": value }))
    } else {
        Ok(json!({ "valid": false }))
    }
}

async fn handle_device_pair_list() -> RpcResult {
    let records = pairing_store::list_devices()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(records).unwrap_or(json!([]));
    Ok(json!({ "devices": value }))
}

async fn handle_device_pair_approve(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::approve_device(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

async fn handle_device_pair_reject(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::reject_device(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

async fn handle_device_token_rotate(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::rotate_device_token(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

async fn handle_device_token_revoke(params: &Value) -> RpcResult {
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if device_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'device_id' parameter".to_string()));
    }
    let record = pairing_store::revoke_device_token(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

// ── TTS (text-to-speech) ────────────────────────────────────────────────────

async fn handle_tts_status(channel: &Arc<GatewayChannel>) -> RpcResult {
    tts_service::status(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_providers() -> RpcResult {
    Ok(json!({ "providers": tts_service::providers() }))
}

async fn handle_tts_enable(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let provider = params.get("provider").and_then(|v| v.as_str());
    let voice = params.get("voice").and_then(|v| v.as_str());
    let model = params.get("model").and_then(|v| v.as_str());
    tts_service::enable(&channel.config().savfox_home, provider, voice, model)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_disable(channel: &Arc<GatewayChannel>) -> RpcResult {
    tts_service::disable(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_convert(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return Err((INVALID_REQUEST, "missing 'text' parameter".to_string()));
    }
    tts_service::convert(&channel.config().savfox_home, channel.http_client(), params)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_tts_set_provider(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if provider.is_empty() {
        return Err((INVALID_REQUEST, "missing 'provider' parameter".to_string()));
    }
    let voice = params.get("voice").and_then(|v| v.as_str());
    let model = params.get("model").and_then(|v| v.as_str());
    tts_service::set_provider(&channel.config().savfox_home, provider, voice, model)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

// ── Skills ──────────────────────────────────────────────────────────────────

async fn handle_skills_status(channel: &Arc<GatewayChannel>) -> RpcResult {
    skills_store::status(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_skills_bins(channel: &Arc<GatewayChannel>) -> RpcResult {
    skills_store::bins(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_skills_install(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        return Err((INVALID_REQUEST, "missing 'name' parameter".to_string()));
    }
    let source = params.get("source").and_then(|v| v.as_str());
    skills_store::install(&channel.config().savfox_home, name, source)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_skills_update(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = params.get("enabled").and_then(|v| v.as_bool());
    let disabled_reason = params.get("disabled_reason").and_then(|v| v.as_str());
    skills_store::update(
        &channel.config().savfox_home,
        if name.is_empty() { None } else { Some(name) },
        enabled,
        disabled_reason,
    )
    .await
    .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_skills_set_env(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() {
        return Err((INVALID_REQUEST, "missing 'key' parameter".to_string()));
    }
    if value.is_empty() {
        return Err((INVALID_REQUEST, "missing 'value' parameter".to_string()));
    }
    skills_store::set_env(&channel.config().savfox_home, key, value)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

// ── Exec approvals ──────────────────────────────────────────────────────────

async fn handle_exec_approvals_get(channel: &Arc<GatewayChannel>) -> RpcResult {
    let approvals = list_pending_approvals(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to load approvals: {err}")))?;
    let policy = approval_policy_store::get_global(&channel.config().savfox_home)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to load approval policy: {err}"),
            )
        })?;
    let count = approvals.len();
    Ok(json!({
        "mode": policy.get("mode").cloned().unwrap_or(Value::String("auto".to_string())),
        "rules": policy.get("rules").cloned().unwrap_or(Value::Array(Vec::new())),
        "approvals": approvals,
        "count": count,
    }))
}

async fn handle_exec_approvals_set(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    approval_policy_store::set_global(&channel.config().savfox_home, mode, params.get("rules"))
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_exec_approvals_node_get(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    approval_policy_store::get_node(&channel.config().savfox_home, node_id)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_exec_approvals_node_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'node_id' parameter".to_string()));
    }
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    approval_policy_store::set_node(
        &channel.config().savfox_home,
        node_id,
        mode,
        params.get("rules"),
    )
    .await
    .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_exec_approval_request(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let command = params.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if command.is_empty() {
        return Err((INVALID_REQUEST, "missing 'command' parameter".to_string()));
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let request = ExecApprovalRequest {
        id: uuid::Uuid::now_v7().to_string(),
        command: command.to_owned(),
        cwd: params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        host: params
            .get("host")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        security: params
            .get("security")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        ask: params
            .get("ask")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        agent_id: params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        session_id: params
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        created_at_ms: now_ms,
        expires_at_ms: params
            .get("expires_at_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(now_ms + 300_000), // Default 5 min expiry
    };

    if let Err(err) = persist_pending_approval(&channel.config().savfox_home, &request).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to persist approval request: {err}"),
        ));
    }

    // Load forwarding config from env and forward the approval.
    let config = load_approval_forwarding_config();
    forward_approval_to_chat(channel, session_mgr, &request, &config).await;

    Ok(json!({
        "request_id": request.id,
        "command": command,
        "status": "pending",
    }))
}

async fn handle_exec_approval_resolve(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let approved = params
        .get("approved")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if request_id.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'request_id' parameter".to_string(),
        ));
    }

    let resolution = ExecApprovalResolution {
        id: request_id.to_owned(),
        approved,
        resolved_by: params
            .get("resolved_by")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        reason: params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
    };

    let resolved_pending = persist_resolved_approval(&channel.config().savfox_home, &resolution)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to persist approval resolution: {err}"),
            )
        })?;

    // Notify channels about the resolution.
    let config = load_approval_forwarding_config();
    notify_approval_resolved(channel, session_mgr, &resolution, &config).await;

    Ok(json!({
        "request_id": request_id,
        "approved": approved,
        "status": "resolved",
        "resolved_pending": resolved_pending,
    }))
}

/// Load approval forwarding config from environment variables.
fn load_approval_forwarding_config() -> ApprovalForwardingConfig {
    let enabled = std::env::var("SAVFOX_APPROVAL_FORWARDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mode = std::env::var("SAVFOX_APPROVAL_MODE").unwrap_or_else(|_| "targets".to_string());

    let targets = std::env::var("SAVFOX_APPROVAL_TARGETS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let agent_filter = std::env::var("SAVFOX_APPROVAL_AGENT_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let session_filter = std::env::var("SAVFOX_APPROVAL_SESSION_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    ApprovalForwardingConfig {
        enabled,
        mode,
        targets,
        agent_filter,
        session_filter,
    }
}

// ── Usage ───────────────────────────────────────────────────────────────────

async fn handle_usage_status(session_store: &Arc<SessionStore>) -> RpcResult {
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

async fn handle_usage_cost(params: &Value, session_store: &Arc<SessionStore>) -> RpcResult {
    let period = params
        .get("period")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
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

async fn handle_logs_tail(params: &Value) -> RpcResult {
    let lines = params.get("lines").and_then(|v| v.as_u64()).unwrap_or(50);
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
        .to_string()
}

async fn load_heartbeat_settings(channel: &GatewayChannel) -> HeartbeatSettingsDocument {
    let path = heartbeat_config_path(channel);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
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

async fn handle_last_heartbeat(params: &Value) -> RpcResult {
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

async fn handle_set_heartbeats(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = heartbeat_agent_from_params(params);
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let interval_ms = params
        .get("interval_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);
    let coalesce_window_ms = params
        .get("coalesce_window_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);

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

async fn handle_system_presence(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let status = params
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("online");

    // Broadcast presence change to all connected clients.
    session_mgr
        .broadcast_to_all(
            "system.presence",
            json!({ "status": status, "timestamp": chrono::Utc::now().to_rfc3339() }),
        )
        .await;

    Ok(json!({ "status": status }))
}

async fn handle_system_event(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
    cron_service: &Arc<CronService>,
) -> RpcResult {
    let event_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let heartbeat = params
        .get("heartbeat")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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
                    event_type: event_type.to_string(),
                    text: text.to_string(),
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

// ── Models (per-provider store) ─────────────────────────────────────────

fn models_dir(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("models")
}

// ── Provider file v2 types ──────────────────────────────────────────────

fn default_version() -> u32 {
    2
}
fn default_auth_type() -> String {
    "api_key".to_string()
}

/// On-disk representation of `models/{provider_id}.json` (v2).
/// Persisted shape is `enabled_models` plus auth/provider metadata.
/// `models` is kept runtime-only for RPC responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    provider_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    auth: Option<ProviderAuth>,
    #[serde(default, rename = "models", skip_serializing)]
    models: Vec<Value>,
    #[serde(default)]
    enabled_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderAuth {
    #[serde(rename = "type", default = "default_auth_type")]
    auth_type: String,
    #[serde(default)]
    env_key: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

fn provider_models_from_enabled_models(provider_id: &str, enabled_models: &[String]) -> Vec<Value> {
    let canonical_provider = savfox_core::canonical_provider_id(provider_id);
    let default_slug = savfox_core::provider_default_model_slug(provider_id);
    let normalized_enabled: Vec<String> = enabled_models
        .iter()
        .filter_map(|slug| {
            let trimmed = slug.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(
                savfox_core::parse_provider_prefixed_model(trimmed)
                    .map(|(_, model_slug)| model_slug.to_string())
                    .unwrap_or_else(|| trimmed.to_string()),
            )
        })
        .collect();

    savfox_core::provider_models_from_enabled_slugs(provider_id, &normalized_enabled)
        .into_iter()
        .map(|model| {
            let model_slug = model.slug;
            json!({
                "id": format!("{canonical_provider}/{model_slug}"),
                "name": model.name,
                "provider": canonical_provider.as_str(),
                "model_slug": model_slug,
                "is_default": default_slug == Some(model_slug.as_str()),
                "builtin": true,
            })
        })
        .collect()
}

fn provider_enabled_models_from_models(models: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut enabled_models = Vec::new();
    for model in models {
        let slug = model
            .get("model_slug")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                model
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|id| {
                        savfox_core::parse_provider_prefixed_model(id)
                            .map(|(_, model_slug)| model_slug)
                            .unwrap_or(id)
                    })
            });
        let Some(slug) = slug else {
            continue;
        };
        let slug = slug.to_string();
        if seen.insert(slug.clone()) {
            enabled_models.push(slug);
        }
    }
    enabled_models
}

fn hydrate_provider_file_enabled_models(file: &mut ProviderFile) {
    if file.enabled_models.is_empty() && !file.models.is_empty() {
        file.enabled_models = provider_enabled_models_from_models(&file.models);
    } else if !file.enabled_models.is_empty() {
        file.enabled_models = file
            .enabled_models
            .iter()
            .filter_map(|slug| {
                let trimmed = slug.trim();
                if trimmed.is_empty() {
                    return None;
                }
                Some(
                    savfox_core::parse_provider_prefixed_model(trimmed)
                        .map(|(_, model_slug)| model_slug.to_string())
                        .unwrap_or_else(|| trimmed.to_string()),
                )
            })
            .collect();
    }
}

fn hydrate_provider_file_models(file: &mut ProviderFile, provider_id_hint: &str) {
    if !file.models.is_empty() || file.enabled_models.is_empty() {
        return;
    }

    let provider_id = if file.provider_id.trim().is_empty() {
        provider_id_hint.trim().to_string()
    } else {
        file.provider_id.trim().to_string()
    };
    if provider_id.is_empty() {
        return;
    }

    if file.provider_id.trim().is_empty() {
        file.provider_id = provider_id.clone();
    }
    file.models = provider_models_from_enabled_models(&provider_id, &file.enabled_models);
}

/// Load a provider file, auto-detecting v1 (bare array) vs v2 (object).
async fn load_provider_file(channel: &GatewayChannel, provider_id: &str) -> ProviderFile {
    let path = models_dir(channel).join(format!("{provider_id}.json"));
    let Ok(data) = tokio::fs::read_to_string(&path).await else {
        return ProviderFile {
            version: 2,
            provider_id: provider_id.to_string(),
            name: String::new(),
            auth: None,
            models: Vec::new(),
            enabled_models: Vec::new(),
        };
    };

    // Try v2 (object with "models" key) first, then fall back to v1 (bare array).
    if let Ok(mut file) = serde_json::from_str::<ProviderFile>(&data) {
        hydrate_provider_file_enabled_models(&mut file);
        hydrate_provider_file_models(&mut file, provider_id);
        return file;
    }
    if let Ok(models) = serde_json::from_str::<Vec<Value>>(&data) {
        return ProviderFile {
            version: 2,
            provider_id: provider_id.to_string(),
            name: String::new(),
            auth: None,
            models,
            enabled_models: Vec::new(),
        };
    }

    ProviderFile {
        version: 2,
        provider_id: provider_id.to_string(),
        name: String::new(),
        auth: None,
        models: Vec::new(),
        enabled_models: Vec::new(),
    }
}

/// Save a v2 provider file. Creates the `models/` dir on first write.
async fn save_provider_file(
    channel: &GatewayChannel,
    provider_id: &str,
    file: &ProviderFile,
) -> Result<(), String> {
    let mut file_to_write = file.clone();
    hydrate_provider_file_enabled_models(&mut file_to_write);
    let dir = models_dir(channel);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("create models dir: {e}"))?;
    let path = dir.join(format!("{provider_id}.json"));
    if file_to_write.models.is_empty()
        && file_to_write.enabled_models.is_empty()
        && file_to_write.auth.is_none()
    {
        let _ = tokio::fs::remove_file(&path).await;
        return Ok(());
    }
    let data = serde_json::to_string_pretty(&file_to_write)
        .map_err(|e| format!("serialize error: {e}"))?;
    tokio::fs::write(&path, data)
        .await
        .map_err(|e| format!("write error: {e}"))
}

/// Inject a single provider's auth into the runtime override maps.
///
/// Sets both the env-variable override (for providers with `env_key`) and
/// the bearer-token override (keyed by provider id, for providers like
/// OpenAI whose built-in config has `env_key: None`).
fn inject_provider_auth(file: &ProviderFile) {
    if let Some(auth) = &file.auth {
        if let Some(api_key) = &auth.api_key {
            if !api_key.is_empty() {
                // Set env-variable override (for providers that use env_key).
                if let Some(env_key) = &auth.env_key {
                    if !env_key.is_empty() {
                        savfox_core::set_env_override(env_key, api_key);
                    }
                }
                // Also set bearer-token override keyed by provider id so
                // that `auth_provider_from_auth` can find it even when the
                // in-memory ModelProviderInfo has `env_key: None`.
                if !file.provider_id.is_empty() {
                    savfox_core::set_bearer_token_override(&file.provider_id, api_key);
                }
            }
        }
    }
}

/// Read all `models/*.json` files and inject their auth into the runtime
/// env-override map. Called once at gateway startup.
pub(crate) async fn inject_all_provider_auth(channel: &GatewayChannel) {
    let dir = models_dir(channel);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let provider_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if provider_id.is_empty() {
            continue;
        }
        let file = load_provider_file(channel, &provider_id).await;
        inject_provider_auth(&file);
    }
}

#[derive(Clone, Debug)]
struct RemoteModelsRequest {
    provider: String,
    base_url: String,
    api_key: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug)]
struct RemoteModelsHttpResponse {
    url: String,
    status: reqwest::StatusCode,
    request_id: Option<String>,
    body: String,
}

fn canonical_models_provider_id(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "zhipu" | "zhipu-ai" => "zhipuai".to_string(),
        "zhipu-coding-plan" | "zhipu-ai-coding-plan" => "zhipuai-coding-plan".to_string(),
        "volc" | "volc-engine" | "ark" => "volcengine".to_string(),
        "together" | "together-ai" => "togetherai".to_string(),
        "gemini" => "google".to_string(),
        "bedrock" => "amazon-bedrock".to_string(),
        "qwen" => "alibaba".to_string(),
        "googlevertex" | "google_vertex" => "google-vertex".to_string(),
        "google_vertex_anthropic" => "google-vertex-anthropic".to_string(),
        other => other.to_string(),
    }
}

fn model_test_default_base_url(provider_id: &str) -> Option<String> {
    savfox_core::provider_default_base_url(provider_id)
}

fn model_test_extract_count(payload: &Value) -> Option<usize> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| arr.len())
        .or_else(|| {
            payload
                .get("models")
                .and_then(Value::as_array)
                .map(|arr| arr.len())
        })
}

fn model_test_extract_models_array(payload: &Value) -> Option<&Vec<Value>> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.as_array())
}

fn model_test_extract_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("x-oai-request-id"))
        .or_else(|| headers.get("cf-ray"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn model_test_truncate_oneline(input: &str, max_chars: usize) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }

    let mut out = String::new();
    for (idx, ch) in collapsed.chars().enumerate() {
        if idx >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn model_test_nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

fn model_test_remote_requested(params: &Value) -> bool {
    params.get("provider").is_some()
        || params.get("provider_id").is_some()
        || params.get("base_url").is_some()
        || params.get("api_key").is_some()
}

fn model_test_resolve_remote_request(
    params: &Value,
) -> Result<Option<RemoteModelsRequest>, (i64, String)> {
    if !model_test_remote_requested(params) {
        return Ok(None);
    }

    let provider = model_test_nonempty_string(params.get("provider"))
        .or_else(|| model_test_nonempty_string(params.get("provider_id")))
        .map(|value| canonical_models_provider_id(&value))
        .unwrap_or_default();
    let explicit_base_url = model_test_nonempty_string(params.get("base_url"));
    let base_url = explicit_base_url
        .or_else(|| model_test_default_base_url(&provider))
        .ok_or((
            INVALID_PARAMS,
            "missing 'base_url' parameter for model discovery".to_string(),
        ))?;
    let api_key = model_test_nonempty_string(params.get("api_key"));

    Ok(Some(RemoteModelsRequest {
        provider,
        base_url,
        api_key,
        account_id: None,
    }))
}

fn model_test_is_openai_platform_base_url(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    normalized == "https://api.openai.com" || normalized == "https://api.openai.com/v1"
}

fn model_test_chatgpt_codex_base_url(channel: &Arc<GatewayChannel>) -> String {
    let base = channel
        .config()
        .chatgpt_base_url
        .trim()
        .trim_end_matches('/');
    if base.ends_with("/codex") {
        base.to_string()
    } else {
        format!("{base}/codex")
    }
}

async fn model_test_apply_openai_auth_fallback(
    request: &mut RemoteModelsRequest,
    channel: &Arc<GatewayChannel>,
) {
    if request.provider != "openai" || request.api_key.is_some() {
        return;
    }

    let auth_manager = gateway_auth_manager(channel);
    let Some(auth) = auth_manager.auth().await else {
        return;
    };

    if let Ok(token) = auth.get_token() {
        let token = token.trim();
        if !token.is_empty() {
            request.api_key = Some(token.to_string());
        }
    }

    if let Some(account_id) = auth.get_account_id() {
        let account_id = account_id.trim();
        if !account_id.is_empty() {
            request.account_id = Some(account_id.to_string());
        }
    }

    if auth.is_chatgpt_auth() && model_test_is_openai_platform_base_url(&request.base_url) {
        request.base_url = model_test_chatgpt_codex_base_url(channel);
    }
}

async fn model_test_fetch_remote_models(
    request: &RemoteModelsRequest,
) -> Result<RemoteModelsHttpResponse, (i64, String)> {
    let url = format!("{}/models", request.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to build model test client: {err}"),
            )
        })?;

    let mut http_request = client.get(&url);
    if let Some(key) = request.api_key.as_deref() {
        http_request = http_request.bearer_auth(key);
    }
    if let Some(account_id) = request.account_id.as_deref() {
        http_request = http_request.header("chatgpt-account-id", account_id);
    }

    let response = http_request.send().await.map_err(|err| {
        (
            INTERNAL_ERROR,
            format!("connection request failed for {url}: {err}"),
        )
    })?;

    let status = response.status();
    let request_id = model_test_extract_request_id(response.headers());
    let body = response.text().await.unwrap_or_default();

    Ok(RemoteModelsHttpResponse {
        url,
        status,
        request_id,
        body,
    })
}

fn model_test_http_failure_message(prefix: &str, response: &RemoteModelsHttpResponse) -> String {
    let mut message = format!(
        "{prefix}: HTTP {} {}",
        response.status.as_u16(),
        response.status.canonical_reason().unwrap_or("Unknown"),
    );
    let body_preview = model_test_truncate_oneline(&response.body, 240);
    if !body_preview.is_empty() {
        message.push_str(&format!(": {body_preview}"));
    }
    if let Some(request_id) = response.request_id.as_deref() {
        message.push_str(&format!(", request id: {request_id}"));
    }
    message
}

fn model_test_model_item_field(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| model_test_nonempty_string(item.get(*key)))
}

fn model_reasoning_flag(item: &Value) -> bool {
    item.get("supports_reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || item
            .get("reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || item
            .get("capabilities")
            .and_then(|value| value.get("reasoning"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || item
            .get("options")
            .and_then(|value| value.get("thinking"))
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .map(|value| matches!(value.trim(), "enabled" | "auto"))
            .unwrap_or(false)
}

fn binary_reasoning_levels_value() -> Value {
    json!([
        {
            "effort": "none",
            "description": "Disable additional reasoning for faster responses",
        },
        {
            "effort": "medium",
            "description": "Enable provider-managed reasoning",
        }
    ])
}

fn extract_model_provider_and_slug(entry: &Value) -> Option<(String, String)> {
    let raw_id = entry.get("id").and_then(Value::as_str).map(str::trim);
    let parsed_from_id = raw_id
        .filter(|value| !value.is_empty())
        .and_then(savfox_core::parse_provider_prefixed_model)
        .map(|(provider_id, model_slug)| {
            (
                canonical_models_provider_id(provider_id),
                model_slug.trim().to_string(),
            )
        });

    let provider_id = entry
        .get("provider")
        .and_then(|value| match value {
            Value::String(provider) => Some(provider.to_string()),
            Value::Object(provider) => provider
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            _ => None,
        })
        .map(|value| canonical_models_provider_id(&value))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            parsed_from_id
                .as_ref()
                .map(|(provider_id, _)| provider_id.clone())
        })?;

    let model_slug = entry
        .get("model_slug")
        .and_then(Value::as_str)
        .or_else(|| entry.get("code").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| parsed_from_id.map(|(_, model_slug)| model_slug))?;

    Some((provider_id, model_slug))
}

fn enrich_model_reasoning_metadata(entry: &mut Value) {
    let provider_and_slug = extract_model_provider_and_slug(entry);
    let registry_model = provider_and_slug
        .as_ref()
        .and_then(|(provider_id, model_slug)| {
            savfox_model::provider_model_info(provider_id, model_slug)
        });
    let source_supports_reasoning = model_reasoning_flag(entry);

    let Some(model) = entry.as_object_mut() else {
        return;
    };

    if let Some(registry_model) = registry_model {
        if model.get("default_reasoning_level").is_none()
            && let Some(default_reasoning_level) = registry_model.default_reasoning_level
        {
            model.insert(
                "default_reasoning_level".to_string(),
                serde_json::to_value(default_reasoning_level).unwrap_or(Value::Null),
            );
        }

        let has_supported_levels = model
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some_and(|levels| !levels.is_empty());
        if !has_supported_levels && !registry_model.supported_reasoning_levels.is_empty() {
            model.insert(
                "supported_reasoning_levels".to_string(),
                serde_json::to_value(registry_model.supported_reasoning_levels)
                    .unwrap_or(Value::Null),
            );
        }
    }

    let has_supported_levels = model
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .is_some_and(|levels| !levels.is_empty());
    let has_default_reasoning = model
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());

    if source_supports_reasoning || has_supported_levels || has_default_reasoning {
        if !has_default_reasoning {
            model.insert("default_reasoning_level".to_string(), json!("medium"));
        }
        if !has_supported_levels {
            model.insert(
                "supported_reasoning_levels".to_string(),
                binary_reasoning_levels_value(),
            );
        }
    }
}

fn model_test_parse_remote_models(payload: &Value, provider_hint: &str) -> Vec<Value> {
    let models = match model_test_extract_models_array(payload) {
        Some(items) => items,
        None => return Vec::new(),
    };

    let canonical_hint = canonical_models_provider_id(provider_hint);
    let mut parsed = Vec::new();
    let mut seen = HashSet::new();

    for item in models {
        let (raw_id, name, provider_from_item) = if let Some(id) =
            model_test_model_item_field(item, &["id", "model", "model_id", "model_slug"])
        {
            (
                id,
                model_test_model_item_field(item, &["name", "name", "label"]),
                model_test_model_item_field(item, &["provider"]),
            )
        } else if let Some(id) = item.as_str().map(str::trim).filter(|v| !v.is_empty()) {
            (id.to_string(), None, None)
        } else {
            continue;
        };

        let mut provider = provider_from_item
            .map(|value| canonical_models_provider_id(&value))
            .unwrap_or_default();
        if provider.is_empty() && !canonical_hint.is_empty() {
            provider = canonical_hint.clone();
        }

        let mut model_slug = raw_id.clone();
        if let Some((prefix, rest)) = savfox_core::parse_provider_prefixed_model(raw_id.as_str()) {
            let prefix = canonical_models_provider_id(prefix);
            if provider.is_empty() && !prefix.is_empty() {
                provider = prefix;
            }
            model_slug = rest.to_string();
        }

        if model_slug.is_empty() {
            continue;
        }

        let id = if raw_id.contains('/') || provider.is_empty() {
            raw_id
        } else {
            format!("{provider}/{model_slug}")
        };

        if !seen.insert(id.clone()) {
            continue;
        }

        let mut entry = serde_json::Map::new();
        entry.insert("id".to_string(), json!(id));
        entry.insert(
            "name".to_string(),
            json!(name.unwrap_or_else(|| model_slug.clone())),
        );
        entry.insert("model_slug".to_string(), json!(model_slug));
        entry.insert("is_default".to_string(), json!(false));
        entry.insert("builtin".to_string(), json!(true));
        if !provider.is_empty() {
            entry.insert("provider".to_string(), json!(provider));
        }
        if let Some(default_reasoning_level) = item
            .get("default_reasoning_level")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            entry.insert(
                "default_reasoning_level".to_string(),
                json!(default_reasoning_level),
            );
        }
        if let Some(supported_reasoning_levels) = item
            .get("supported_reasoning_levels")
            .filter(|value| value.is_array())
        {
            entry.insert(
                "supported_reasoning_levels".to_string(),
                supported_reasoning_levels.clone(),
            );
        }
        if model_reasoning_flag(item) {
            entry.insert("supports_reasoning".to_string(), json!(true));
        }

        let mut value = Value::Object(entry);
        enrich_model_reasoning_metadata(&mut value);
        parsed.push(value);
    }

    parsed
}

async fn handle_models_test(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    use savfox_core::models_manager::manager::RefreshStrategy;

    if let Some(model_id) = params
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let models = channel
            .session_manager()
            .list_models(channel.config(), RefreshStrategy::OnlineIfUncached)
            .await;

        let ok = models.iter().any(|preset| {
            preset.id == model_id
                || preset.slug == model_id
                || preset.name.eq_ignore_ascii_case(model_id)
        });

        let message = if ok {
            format!("Model `{model_id}` is available.")
        } else {
            format!("Model `{model_id}` was not found in the available model list.")
        };

        return Ok(json!({
            "ok": ok,
            "message": message,
            "model": model_id,
        }));
    }

    let mut request = model_test_resolve_remote_request(params)?.ok_or((
        INVALID_PARAMS,
        "missing provider/base_url parameters for connection test".to_string(),
    ))?;
    model_test_apply_openai_auth_fallback(&mut request, channel).await;
    let response = model_test_fetch_remote_models(&request).await?;

    if response.status.is_success() {
        let model_count = serde_json::from_str::<Value>(&response.body)
            .ok()
            .and_then(|payload| model_test_extract_count(&payload));
        let mut message = if let Some(count) = model_count {
            format!("Connection successful. Retrieved {count} model(s).")
        } else {
            "Connection successful.".to_string()
        };
        if let Some(request_id) = response.request_id.as_deref() {
            message.push_str(&format!(" request id: {request_id}"));
        }

        return Ok(json!({
            "ok": true,
            "message": message,
            "status": response.status.as_u16(),
            "model_count": model_count,
            "base_url": request.base_url,
        }));
    }

    let message = model_test_http_failure_message("Connection failed", &response);

    Ok(json!({
        "ok": false,
        "message": message,
        "status": response.status.as_u16(),
        "base_url": request.base_url,
    }))
}

/// Load models for a single provider (thin wrapper over `load_provider_file`).
async fn load_provider_models(channel: &GatewayChannel, provider_id: &str) -> Vec<Value> {
    load_provider_file(channel, provider_id).await.models
}

/// Load all models from all `models/*.json` files, merged into a flat HashMap keyed by model ID.
async fn load_all_provider_models(channel: &GatewayChannel) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let dir = models_dir(channel);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let provider_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if provider_id.is_empty() {
            continue;
        }
        let file = load_provider_file(channel, &provider_id).await;
        for model in file.models {
            if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                out.insert(id.to_string(), model);
            }
        }
    }
    out
}

/// Save models array to `models/{provider_id}.json`. Preserves existing auth.
async fn save_provider_models(
    channel: &GatewayChannel,
    provider_id: &str,
    models: &[Value],
) -> Result<(), String> {
    let mut file = load_provider_file(channel, provider_id).await;
    file.models = models.to_vec();
    save_provider_file(channel, provider_id, &file).await
}

async fn handle_models_list(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    if let Some(mut request) = model_test_resolve_remote_request(params)? {
        model_test_apply_openai_auth_fallback(&mut request, channel).await;
        let response = model_test_fetch_remote_models(&request).await?;
        if !response.status.is_success() {
            return Err((
                INTERNAL_ERROR,
                model_test_http_failure_message("remote model list request failed", &response),
            ));
        }

        let payload = serde_json::from_str::<Value>(&response.body).map_err(|err| {
            let preview = model_test_truncate_oneline(&response.body, 240);
            (
                INTERNAL_ERROR,
                format!(
                    "failed to parse model list response from {}: {err}; body={preview}",
                    response.url
                ),
            )
        })?;
        let models = model_test_parse_remote_models(&payload, &request.provider);
        return Ok(json!({ "models": models }));
    }

    // Per-provider store is the source of truth for the models page / selector.
    // Built-in engine presets are intentionally omitted: they lack a provider prefix
    // in their IDs and would display as one-model "provider" groups.  The wizard
    // writes every selected model into the per-provider store, so anything the user
    // has configured will appear here.
    let custom = load_all_provider_models(channel).await;
    let mut models: Vec<Value> = Vec::with_capacity(custom.len());
    for (_id, config) in &custom {
        let mut entry = config.clone();
        if let Value::Object(ref mut map) = entry {
            map.entry("builtin".to_string()).or_insert(json!(false));
            // Never expose secrets over the wire.
            map.remove("api_key");
        }
        enrich_model_reasoning_metadata(&mut entry);
        models.push(entry);
    }

    Ok(json!({ "models": models }))
}

async fn handle_models_add(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let model_slug = params
        .get("model_slug")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if provider.is_empty() || model_slug.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'provider' and/or 'model_slug'".to_string(),
        ));
    }

    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{provider}/{model_slug}"));

    let mut entry = json!({
        "id": id,
        "provider": provider,
        "model_slug": model_slug,
        "is_default": false,
        "is_disabled": false,
        "builtin": false,
    });

    // Copy optional fields
    for field in &[
        "api_key",
        "base_url",
        "max_tokens",
        "temperature",
        "name",
        "is_disabled",
        "cost_input_per_m",
        "cost_output_per_m",
        "cost_cache_read_per_m",
        "cost_cache_write_per_m",
        "context_window",
        "max_output_tokens",
        "auth_type",
        "custom_headers",
    ] {
        if let Some(v) = params.get(*field) {
            entry[*field] = v.clone();
        }
    }

    let mut models = load_provider_models(channel, provider).await;
    // Remove existing entry with same id if present
    models.retain(|m| m.get("id").and_then(|v| v.as_str()) != Some(&id));
    models.push(entry);
    save_provider_models(channel, provider, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "added" }))
}

async fn handle_models_update(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let provider_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(channel, provider_id).await;
    // Find existing entry or create new one
    let entry = if let Some(pos) = models
        .iter()
        .position(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        &mut models[pos]
    } else {
        models.push(json!({"id": id}));
        models.last_mut().unwrap()
    };

    // Merge updatable fields
    for field in &[
        "provider",
        "model_slug",
        "api_key",
        "base_url",
        "name",
        "is_disabled",
        "max_tokens",
        "temperature",
        "cost_input_per_m",
        "cost_output_per_m",
        "cost_cache_read_per_m",
        "cost_cache_write_per_m",
        "context_window",
        "max_output_tokens",
        "auth_type",
        "custom_headers",
    ] {
        if let Some(v) = params.get(*field) {
            entry[*field] = v.clone();
        }
    }

    save_provider_models(channel, provider_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "updated" }))
}

async fn handle_models_delete(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let provider_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(channel, provider_id).await;
    models.retain(|m| m.get("id").and_then(|v| v.as_str()) != Some(id));
    save_provider_models(channel, provider_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "deleted" }))
}

async fn handle_models_setdefault(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_string()));
    }

    let provider_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(channel, provider_id).await;

    // Clear default on all entries in this provider
    for m in models.iter_mut() {
        if let Value::Object(map) = m {
            map.insert("is_default".to_string(), json!(false));
        }
    }

    // Set default on target (create entry if it doesn't exist yet)
    if let Some(entry) = models
        .iter_mut()
        .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        entry["is_default"] = json!(true);
    } else {
        models.push(json!({"id": id, "is_default": true}));
    }

    save_provider_models(channel, provider_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "default_set" }))
}

async fn handle_models_import(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let provider_id = params
        .get("provider_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if provider_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'provider_id'".to_string()));
    }

    let models_arr = params.get("models").and_then(|v| v.as_array()).ok_or((
        INVALID_REQUEST,
        "missing or invalid 'models' array".to_string(),
    ))?;
    if models_arr.is_empty() {
        return Err((INVALID_REQUEST, "'models' array is empty".to_string()));
    }

    // Build model entries
    let mut entries: Vec<Value> = Vec::with_capacity(models_arr.len());
    for m in models_arr {
        let mut entry = m.clone();
        if let Value::Object(ref mut map) = entry {
            map.entry("provider".to_string())
                .or_insert_with(|| json!(provider_id));
            map.entry("builtin".to_string()).or_insert(json!(false));
        }
        entries.push(entry);
    }

    // Build auth section from params
    let api_key_val = params.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let env_key_val = params.get("env_key").and_then(|v| v.as_str()).unwrap_or("");
    let display_name_val = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(provider_id);

    let auth = if !api_key_val.is_empty() && !env_key_val.is_empty() {
        Some(ProviderAuth {
            auth_type: "api_key".to_string(),
            env_key: Some(env_key_val.to_string()),
            api_key: Some(api_key_val.to_string()),
        })
    } else if !env_key_val.is_empty() {
        Some(ProviderAuth {
            auth_type: "api_key".to_string(),
            env_key: Some(env_key_val.to_string()),
            api_key: None,
        })
    } else {
        None
    };

    let file = ProviderFile {
        version: 2,
        provider_id: provider_id.to_string(),
        name: display_name_val.to_string(),
        auth,
        models: entries.clone(),
        enabled_models: Vec::new(),
    };

    // Write v2 provider file to models/{provider_id}.json
    save_provider_file(channel, provider_id, &file)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    // Inject auth into runtime so the engine can authenticate immediately
    inject_provider_auth(&file);

    // Patch user config: set API key + active model + provider.
    let base_url = params
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let config_provider_id = savfox_core::canonical_provider_id(provider_id);

    // Find the first default model (or first model)
    let default_entry = entries
        .iter()
        .find(|e| {
            e.get("is_default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .or_else(|| entries.first());

    // Build config patch with top-level fields the core engine expects
    let mut patch = json!({});

    // Set selected model in the structured [model] section.
    if let Some(default) = default_entry {
        let model_slug = default
            .get("model_slug")
            .and_then(|v| v.as_str())
            .or_else(|| {
                // Fall back: strip provider prefix from id
                default
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .map(|id| {
                        savfox_core::parse_provider_prefixed_model(id)
                            .map(|(_, code)| code)
                            .unwrap_or(id)
                    })
            })
            .unwrap_or("");
        if !model_slug.is_empty() {
            patch["model"] = json!({
                "slug": model_slug,
                "provider": config_provider_id.clone(),
            });
        }
    }

    // Read existing config to deep-merge env and model_providers.
    let mut config: serde_json::Map<String, Value> = load_config_value_or_empty(channel)
        .await
        .as_object()
        .cloned()
        .unwrap_or_default();

    // Deep-merge top-level scalar fields from patch
    if let Some(patch_obj) = patch.as_object() {
        for (key, value) in patch_obj {
            config.insert(key.clone(), value.clone());
        }
    }

    if config.get("model").and_then(Value::as_object).is_some() {
        config.remove("model_provider");
        config.remove("model_reasoning_effort");
    }

    // Deep-merge [env]: preserve existing keys, add/overwrite new ones
    if !api_key_val.is_empty() && !env_key_val.is_empty() {
        let env = config.entry("env").or_insert_with(|| json!({}));
        if let Some(env_map) = env.as_object_mut() {
            env_map.insert(env_key_val.to_string(), json!(api_key_val));
        }
    }

    // Keep built-in providers (like openai) out of persisted `model_providers`.
    // Only custom providers need explicit on-disk entries.
    {
        let is_builtin_provider =
            savfox_core::built_in_model_providers().contains_key(config_provider_id.as_str());

        if is_builtin_provider {
            if let Some(providers_map) = config
                .get_mut("model_providers")
                .and_then(Value::as_object_mut)
            {
                providers_map.remove(config_provider_id.as_str());
                if config_provider_id != provider_id {
                    providers_map.remove(provider_id);
                }
                if providers_map.is_empty() {
                    config.remove("model_providers");
                }
            }
        } else {
            let providers = config.entry("model_providers").or_insert_with(|| json!({}));
            if let Some(providers_map) = providers.as_object_mut() {
                let provider = providers_map
                    .entry(config_provider_id.clone())
                    .or_insert_with(|| json!({ "name": display_name_val, "wire_api": "chat" }));
                if let Some(provider_map) = provider.as_object_mut() {
                    if !base_url.is_empty() {
                        provider_map.insert("base_url".to_string(), json!(base_url));
                    }
                    if !env_key_val.is_empty() {
                        provider_map.insert("env_key".to_string(), json!(env_key_val));
                    }
                }
            }
        }
    }

    let mut merged = Value::Object(config);
    sanitize_config_before_write(&mut merged, channel).await?;
    write_config_json(channel, &merged)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    let model_count = entries.len();
    Ok(json!({
        "status": "imported",
        "provider_id": provider_id,
        "model_count": model_count,
    }))
}

// ── Tools ───────────────────────────────────────────────────────────────────

async fn handle_tools_invoke(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let tool = params.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let action = params.get("action").and_then(|v| v.as_str());
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    if tool.is_empty() {
        return Err((INVALID_REQUEST, "missing 'tool' parameter".to_string()));
    }

    // Build a prompt that forces the agent to invoke the requested tool.
    let mut args = arguments.clone();
    if let Some(act) = action {
        if let Value::Object(ref mut map) = args {
            map.entry("action")
                .or_insert_with(|| Value::String(act.to_owned()));
        }
    }
    let args_str = serde_json::to_string_pretty(&args).unwrap_or_else(|_| "{}".to_string());
    let prompt = format!(
        "Use the `{tool}` tool with exactly these arguments:\n\
         ```json\n{args_str}\n```\n\
         Return only the raw tool output. Do not add commentary."
    );

    match channel.invoke_agent_text(&prompt, "default").await {
        Ok(output) => {
            // Try to parse the output as JSON for a cleaner response.
            let result = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                Value::String(savfox_core::external_content::wrap_external_content(
                    &format!("tool:{tool}"),
                    &output,
                ))
            });
            Ok(json!({
                "ok": true,
                "tool": tool,
                "result": result,
            }))
        }
        Err(err) => Err((INTERNAL_ERROR, format!("tool invocation failed: {err}"))),
    }
}
