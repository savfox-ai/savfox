use std::collections::{HashMap, HashSet};

use crate::auto_reply::{CommandAction, CommandContext, CommandResult};

pub type CommandHandler = fn(&CommandContext, &ParsedCommandArgs) -> CommandResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionalArg {
    pub name: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedArg {
    pub name: String,
    pub aliases: Vec<String>,
    pub takes_value: bool,
    pub required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandArgsSchema {
    pub positional: Vec<PositionalArg>,
    pub named: Vec<NamedArg>,
    pub allow_extra_positionals: bool,
}

impl CommandArgsSchema {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub aliases: Vec<String>,
    pub args: CommandArgsSchema,
    pub handler: CommandHandler,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCommandArgs {
    pub positionals: Vec<String>,
    pub named: HashMap<String, String>,
    pub flags: HashSet<String>,
}

impl ParsedCommandArgs {
    #[must_use]
    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }

    #[must_use]
    pub fn named(&self, key: &str) -> Option<&str> {
        self.named.get(key).map(String::as_str)
    }

    #[must_use]
    pub fn has_flag(&self, key: &str) -> bool {
        self.flags.contains(key)
    }
}

#[derive(Debug, Clone)]
pub struct CommandInvocation {
    pub command: Command,
    pub args: ParsedCommandArgs,
}

pub struct CommandRegistry {
    commands: HashMap<String, Command>,
    alias_map: HashMap<String, String>,
    plugin_commands: HashMap<String, String>,
}

impl CommandRegistry {
    #[must_use]
    pub fn new() -> Self {
        let mut registry = Self {
            commands: HashMap::new(),
            alias_map: HashMap::new(),
            plugin_commands: HashMap::new(),
        };
        registry.register_builtin_commands();
        registry
    }

    fn register_builtin_commands(&mut self) {
        self.register(Command {
            name: "help".to_string(),
            description: "Show available commands".to_string(),
            aliases: vec!["/?".to_string()],
            args: CommandArgsSchema::none(),
            handler: |_ctx, _args| CommandResult {
                reply: Some(
                    r#"Available Commands:
  /help                 - Show this help message
  /status               - Show current status
  /model [name]         - Show or set the model
  /model --model <name> - Set model with named argument
  /compact [turns]      - Compact current session context
  /reset                - Reset current session"#
                        .to_string(),
                ),
                action: Some(CommandAction::ShowHelp),
                error: None,
            },
        });

        self.register(Command {
            name: "status".to_string(),
            description: "Show current status".to_string(),
            aliases: vec![],
            args: CommandArgsSchema::none(),
            handler: |ctx, _args| {
                let status = format!(
                    "Session: {}\nChannel: {}\nAuthorized: {}",
                    ctx.session_id.as_deref().unwrap_or("none"),
                    ctx.channel_id,
                    ctx.is_authorized
                );
                CommandResult {
                    reply: Some(status),
                    action: Some(CommandAction::ShowStatus),
                    error: None,
                }
            },
        });

        self.register(Command {
            name: "model".to_string(),
            description: "Show or set the model".to_string(),
            aliases: vec![],
            args: CommandArgsSchema {
                positional: vec![PositionalArg {
                    name: "model".to_string(),
                    required: false,
                }],
                named: vec![
                    NamedArg {
                        name: "model".to_string(),
                        aliases: vec!["m".to_string()],
                        takes_value: true,
                        required: false,
                    },
                    NamedArg {
                        name: "profile".to_string(),
                        aliases: vec!["p".to_string()],
                        takes_value: true,
                        required: false,
                    },
                ],
                allow_extra_positionals: false,
            },
            handler: |ctx, args| {
                let model = args.named("model").or_else(|| args.positional(0));
                if let Some(model) = model {
                    let model = model.trim().to_string();
                    CommandResult {
                        reply: Some(format!("Model set to: {model}")),
                        action: Some(CommandAction::SetModel {
                            model,
                        }),
                        error: None,
                    }
                } else {
                    let current = ctx
                        .metadata
                        .get("model")
                        .cloned()
                        .unwrap_or_else(|| "default".to_string());
                    CommandResult {
                        reply: Some(format!("Current model: {current}")),
                        action: None,
                        error: None,
                    }
                }
            },
        });

        self.register(Command {
            name: "reset".to_string(),
            description: "Reset current session".to_string(),
            aliases: vec![],
            args: CommandArgsSchema::none(),
            handler: |_ctx, _args| CommandResult {
                reply: Some("Session reset.".to_string()),
                action: Some(CommandAction::Reset),
                error: None,
            },
        });

        self.register(Command {
            name: "compact".to_string(),
            description: "Compact current session context".to_string(),
            aliases: vec![],
            args: CommandArgsSchema {
                positional: vec![PositionalArg {
                    name: "turns".to_string(),
                    required: false,
                }],
                named: vec![NamedArg {
                    name: "turns".to_string(),
                    aliases: vec!["n".to_string()],
                    takes_value: true,
                    required: false,
                }],
                allow_extra_positionals: false,
            },
            handler: |_ctx, args| {
                let turns_raw = args.named("turns").or_else(|| args.positional(0));
                let parsed_turns = match turns_raw {
                    Some(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u32>() {
                        Ok(v) => Some(v),
                        Err(_) => {
                            return CommandResult {
                                reply: None,
                                action: None,
                                error: Some("Invalid turns value: expected integer".to_string()),
                            };
                        }
                    },
                    _ => None,
                };

                let reply = match parsed_turns {
                    Some(v) => format!("Compaction requested (keep last {v} turns)."),
                    None => "Compaction requested.".to_string(),
                };
                CommandResult {
                    reply: Some(reply),
                    action: Some(CommandAction::Compact {
                        turns: parsed_turns,
                    }),
                    error: None,
                }
            },
        });

        self.register(Command {
            name: "whoami".to_string(),
            description: "Show your sender ID".to_string(),
            aliases: vec!["id".to_string()],
            args: CommandArgsSchema::none(),
            handler: |ctx, _args| CommandResult {
                reply: Some(format!("Your sender ID: {}", ctx.sender_id)),
                action: None,
                error: None,
            },
        });

        self.register(Command {
            name: "new".to_string(),
            description: "Start a new session".to_string(),
            aliases: vec![],
            args: CommandArgsSchema::none(),
            handler: |_ctx, _args| CommandResult {
                reply: Some("Starting new session.".to_string()),
                action: Some(CommandAction::NewSession),
                error: None,
            },
        });

        self.register(Command {
            name: "stop".to_string(),
            description: "Stop current run".to_string(),
            aliases: vec![],
            args: CommandArgsSchema::none(),
            handler: |_ctx, _args| CommandResult {
                reply: Some("Stopping current run.".to_string()),
                action: Some(CommandAction::Stop),
                error: None,
            },
        });

        self.register(Command {
            name: "models".to_string(),
            description: "List available models".to_string(),
            aliases: vec![],
            args: CommandArgsSchema::none(),
            handler: |_ctx, _args| CommandResult {
                reply: Some(
                    r#"Available Models:
  openai/gpt-4o
  openai/gpt-4o-mini
  openai/o1
  anthropic/claude-4-sonnet
  anthropic/claude-3.5-sonnet
  google/gemini-2.0-flash
  deepseek/deepseek-chat
  ollama/llama3.2"#
                        .to_string(),
                ),
                action: None,
                error: None,
            },
        });

        self.register(Command {
            name: "think".to_string(),
            description: "Set thinking level".to_string(),
            aliases: vec!["thinking".to_string(), "t".to_string()],
            args: CommandArgsSchema {
                positional: vec![PositionalArg {
                    name: "level".to_string(),
                    required: false,
                }],
                named: vec![NamedArg {
                    name: "level".to_string(),
                    aliases: vec!["l".to_string()],
                    takes_value: true,
                    required: false,
                }],
                allow_extra_positionals: false,
            },
            handler: |_ctx, args| {
                let level = args
                    .named("level")
                    .or_else(|| args.positional(0))
                    .unwrap_or("medium")
                    .to_lowercase();
                let valid = ["off", "minimal", "low", "medium", "high", "xhigh"];
                if !valid.contains(&level.as_str()) {
                    return CommandResult {
                        reply: None,
                        action: None,
                        error: Some(format!("Invalid thinking level. Valid: {valid:?}")),
                    };
                }
                CommandResult {
                    reply: Some(format!("Thinking level set to: {level}")),
                    action: Some(CommandAction::SetThinking { level }),
                    error: None,
                }
            },
        });

        self.register(Command {
            name: "verbose".to_string(),
            description: "Toggle verbose mode".to_string(),
            aliases: vec!["v".to_string()],
            args: CommandArgsSchema {
                positional: vec![PositionalArg {
                    name: "state".to_string(),
                    required: false,
                }],
                named: vec![
                    NamedArg {
                        name: "on".to_string(),
                        aliases: vec![],
                        takes_value: false,
                        required: false,
                    },
                    NamedArg {
                        name: "off".to_string(),
                        aliases: vec![],
                        takes_value: false,
                        required: false,
                    },
                ],
                allow_extra_positionals: false,
            },
            handler: |_ctx, args| {
                let state = args
                    .positional(0)
                    .map(str::to_lowercase)
                    .unwrap_or_else(|| "on".to_string());
                let enabled = if args.has_flag("off") {
                    false
                } else if args.has_flag("on") {
                    true
                } else {
                    matches!(state.as_str(), "on" | "true" | "1")
                };
                CommandResult {
                    reply: Some(format!(
                        "Verbose mode: {}",
                        if enabled { "on" } else { "off" }
                    )),
                    action: Some(CommandAction::SetVerbose { enabled }),
                    error: None,
                }
            },
        });

        self.register(Command {
            name: "commands".to_string(),
            description: "List all commands".to_string(),
            aliases: vec![],
            args: CommandArgsSchema::none(),
            handler: |_ctx, _args| CommandResult {
                reply: Some(
                    r#"Slash Commands:
  /help, /?      - Show help
  /status        - Show status
  /whoami, /id   - Show sender ID
  /model [name]  - Set/get model
  /models        - List models
  /reset         - Reset session
  /compact [n]   - Compact session
  /new           - New session
  /stop          - Stop run
  /think, /t [L] - Thinking level
  /verbose, /v   - Verbose mode
  /usage [mode]  - Usage info"#
                        .to_string(),
                ),
                action: None,
                error: None,
            },
        });

        self.register(Command {
            name: "usage".to_string(),
            description: "Show usage info".to_string(),
            aliases: vec![],
            args: CommandArgsSchema {
                positional: vec![PositionalArg {
                    name: "mode".to_string(),
                    required: false,
                }],
                named: vec![NamedArg {
                    name: "mode".to_string(),
                    aliases: vec!["m".to_string()],
                    takes_value: true,
                    required: false,
                }],
                allow_extra_positionals: false,
            },
            handler: |ctx, args| {
                let mode = args
                    .named("mode")
                    .or_else(|| args.positional(0))
                    .unwrap_or("tokens");
                let tokens_used = ctx
                    .metadata
                    .get("tokens_used")
                    .map(|s| s.parse::<u64>().unwrap_or(0))
                    .unwrap_or(0);
                let reply = match mode {
                    "off" => "Usage display disabled.".to_string(),
                    "cost" => format!("Estimated cost: ${:.4}", tokens_used as f64 * 0.00001),
                    "full" => format!(
                        "Tokens used: {}\nEstimated cost: ${:.4}",
                        tokens_used,
                        tokens_used as f64 * 0.00001
                    ),
                    _ => format!("Tokens used: {}", tokens_used),
                };
                CommandResult {
                    reply: Some(reply),
                    action: None,
                    error: None,
                }
            },
        });
    }

    pub fn register(&mut self, command: Command) {
        let canonical = normalize_command_name(&command.name);
        if canonical.is_empty() {
            return;
        }
        self.register_alias(&canonical, &canonical);
        self.register_alias(&format!("/{canonical}"), &canonical);
        for alias in &command.aliases {
            self.register_alias(alias, &canonical);
        }
        self.commands.insert(canonical, command);
    }

    pub fn register_plugin_command(
        &mut self,
        plugin_id: &str,
        command: Command,
    ) -> Result<(), String> {
        let plugin_id = plugin_id.trim();
        if plugin_id.is_empty() {
            return Err("plugin_id cannot be empty".to_string());
        }

        let canonical = normalize_command_name(&command.name);
        if canonical.is_empty() {
            return Err("command name cannot be empty".to_string());
        }

        if self.commands.contains_key(&canonical) && !self.plugin_commands.contains_key(&canonical)
        {
            return Err(format!("command '/{canonical}' already exists"));
        }

        if let Some(owner) = self.plugin_commands.get(&canonical)
            && owner != plugin_id
        {
            return Err(format!(
                "command '/{canonical}' is already registered by plugin '{owner}'"
            ));
        }

        self.register(command);
        self.plugin_commands
            .insert(canonical, plugin_id.to_string());
        Ok(())
    }

    pub fn unregister_plugin_commands(&mut self, plugin_id: &str) -> usize {
        let plugin_id = plugin_id.trim();
        if plugin_id.is_empty() {
            return 0;
        }

        let mut removed = Vec::new();
        for (command, owner) in &self.plugin_commands {
            if owner == plugin_id {
                removed.push(command.clone());
            }
        }

        for canonical in &removed {
            self.commands.remove(canonical);
            self.alias_map.retain(|_, target| target != canonical);
            self.plugin_commands.remove(canonical);
        }

        removed.len()
    }

    fn register_alias(&mut self, alias: &str, canonical: &str) {
        let key = alias.trim().to_lowercase();
        if key.is_empty() {
            return;
        }
        self.alias_map.insert(key.clone(), canonical.to_string());
        if let Some(stripped) = key.strip_prefix('/') {
            if !stripped.is_empty() {
                self.alias_map
                    .insert(stripped.to_string(), canonical.to_string());
            }
        } else {
            self.alias_map
                .insert(format!("/{key}"), canonical.to_string());
        }
    }

    #[must_use]
    pub fn parse_command(&self, text: &str) -> Option<Result<CommandInvocation, String>> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (head, rest) = split_head_and_tail(trimmed);
        let key = head.to_lowercase();
        let canonical = self.alias_map.get(&key)?;
        let command = self.commands.get(canonical)?.clone();
        match parse_args(rest, &command.args) {
            Ok(args) => Some(Ok(CommandInvocation { command, args })),
            Err(err) => Some(Err(format!(
                "Invalid arguments for /{}: {err}",
                command.name
            ))),
        }
    }

    pub fn handle_command(&self, text: &str, ctx: &CommandContext) -> Option<CommandResult> {
        match self.parse_command(text)? {
            Ok(invocation) => {
                let handler = invocation.command.handler;
                Some(handler(ctx, &invocation.args))
            }
            Err(err) => Some(CommandResult {
                reply: None,
                action: None,
                error: Some(err),
            }),
        }
    }

    #[must_use]
    pub fn has_command(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        let (head, _) = split_head_and_tail(trimmed);
        self.alias_map.contains_key(&head.to_lowercase())
    }

    #[must_use]
    pub fn resolve_command_name(&self, text: &str) -> Option<String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (head, _) = split_head_and_tail(trimmed);
        self.alias_map.get(&head.to_lowercase()).cloned()
    }

    #[must_use]
    pub fn list_commands(&self) -> Vec<&Command> {
        let mut commands: Vec<_> = self.commands.values().collect();
        commands.sort_by(|a, b| a.name.cmp(&b.name));
        commands
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_command_name(name: &str) -> String {
    name.trim().trim_start_matches('/').to_lowercase()
}

fn split_head_and_tail(s: &str) -> (&str, &str) {
    if let Some(idx) = s.find(char::is_whitespace) {
        let (head, tail) = s.split_at(idx);
        (head, tail.trim())
    } else {
        (s, "")
    }
}

fn parse_args(raw: &str, schema: &CommandArgsSchema) -> Result<ParsedCommandArgs, String> {
    let tokens = tokenize_args(raw)?;
    let mut named_lookup: HashMap<String, &NamedArg> = HashMap::new();
    for named in &schema.named {
        named_lookup.insert(format!("--{}", named.name.to_lowercase()), named);
        for alias in &named.aliases {
            let alias = alias.trim().to_lowercase();
            if alias.is_empty() {
                continue;
            }
            if alias.starts_with('-') {
                named_lookup.insert(alias, named);
            } else if alias.len() == 1 {
                named_lookup.insert(format!("-{alias}"), named);
            } else {
                named_lookup.insert(format!("--{alias}"), named);
            }
        }
    }

    let mut parsed = ParsedCommandArgs::default();
    let mut i = 0usize;
    while i < tokens.len() {
        let token = &tokens[i];
        if token.starts_with('-') {
            let (option, inline_value) = if let Some((left, right)) = token.split_once('=') {
                (left.to_lowercase(), Some(right.to_string()))
            } else {
                (token.to_lowercase(), None)
            };

            let Some(spec) = named_lookup.get(&option) else {
                return Err(format!("unknown option '{token}'"));
            };
            let spec = *spec;

            if spec.takes_value {
                let value = if let Some(v) = inline_value {
                    v
                } else {
                    i += 1;
                    let Some(next) = tokens.get(i) else {
                        return Err(format!("option '{token}' requires a value"));
                    };
                    if next.starts_with('-') {
                        return Err(format!("option '{token}' requires a value"));
                    }
                    next.clone()
                };
                parsed.named.insert(spec.name.clone(), value);
            } else {
                parsed.flags.insert(spec.name.clone());
            }
        } else {
            parsed.positionals.push(token.clone());
        }
        i += 1;
    }

    for named in &schema.named {
        if named.required
            && !parsed.named.contains_key(&named.name)
            && !parsed.flags.contains(&named.name)
        {
            return Err(format!("missing required option '--{}'", named.name));
        }
    }

    let required_positional = schema.positional.iter().filter(|p| p.required).count();
    if parsed.positionals.len() < required_positional {
        return Err("missing required positional argument(s)".to_string());
    }
    if !schema.allow_extra_positionals && parsed.positionals.len() > schema.positional.len() {
        return Err("too many positional arguments".to_string());
    }

    Ok(parsed)
}

fn tokenize_args(raw: &str) -> Result<Vec<String>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else if ch == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if quote.is_some() {
        return Err("unterminated quoted argument".to_string());
    }
    if !current.is_empty() {
        out.push(current);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Command, CommandArgsSchema, CommandRegistry, PositionalArg};
    use crate::auto_reply::{CommandContext, CommandResult};

    fn sample_ctx() -> CommandContext {
        CommandContext {
            sender_id: "u1".to_string(),
            channel_id: "discord:chan".to_string(),
            session_id: Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e160".to_string()),
            is_authorized: true,
            is_mentioned: true,
            is_group: true,
            metadata: HashMap::from([("model".to_string(), "openai/gpt-4o".to_string())]),
        }
    }

    fn plugin_echo_handler(
        _ctx: &CommandContext,
        args: &super::ParsedCommandArgs,
    ) -> CommandResult {
        let who = args.positional(0).unwrap_or("world");
        CommandResult {
            reply: Some(format!("plugin says hi to {who}")),
            action: None,
            error: None,
        }
    }

    #[test]
    fn parses_model_with_named_args() {
        let registry = CommandRegistry::new();
        let result = registry
            .handle_command("/model --model gpt-4o-mini --profile prod", &sample_ctx())
            .expect("command should parse");
        assert!(result.error.is_none());
        assert_eq!(
            result.reply.as_deref(),
            Some("Model set to: gpt-4o-mini@prod")
        );
    }

    #[test]
    fn parses_compact_with_short_option() {
        let registry = CommandRegistry::new();
        let result = registry
            .handle_command("/compact -n 12", &sample_ctx())
            .expect("command should parse");
        assert!(result.error.is_none());
        assert_eq!(
            result.reply.as_deref(),
            Some("Compaction requested (keep last 12 turns).")
        );
    }

    #[test]
    fn rejects_unknown_option() {
        let registry = CommandRegistry::new();
        let result = registry
            .handle_command("/model --unknown 1", &sample_ctx())
            .expect("known command should return parse error");
        assert_eq!(
            result.error.as_deref(),
            Some("Invalid arguments for /model: unknown option '--unknown'")
        );
    }

    #[test]
    fn plugin_can_register_custom_command() {
        let mut registry = CommandRegistry::new();
        registry
            .register_plugin_command(
                "echo_plugin",
                Command {
                    name: "echo".to_string(),
                    description: "Echo from plugin".to_string(),
                    aliases: vec!["say".to_string()],
                    args: CommandArgsSchema {
                        positional: vec![PositionalArg {
                            name: "who".to_string(),
                            required: false,
                        }],
                        named: vec![],
                        allow_extra_positionals: false,
                    },
                    handler: plugin_echo_handler,
                },
            )
            .expect("plugin command should register");

        let result = registry
            .handle_command("/say savfox", &sample_ctx())
            .expect("plugin command should parse");
        assert_eq!(result.reply.as_deref(), Some("plugin says hi to savfox"));
    }

    #[test]
    fn plugin_command_cannot_override_builtin() {
        let mut registry = CommandRegistry::new();
        let err = registry
            .register_plugin_command(
                "echo_plugin",
                Command {
                    name: "help".to_string(),
                    description: "Should fail".to_string(),
                    aliases: vec![],
                    args: CommandArgsSchema::none(),
                    handler: plugin_echo_handler,
                },
            )
            .expect_err("built-in commands must not be overridden by plugins");
        assert_eq!(err, "command '/help' already exists");
    }

    #[test]
    fn unregister_plugin_commands_removes_aliases() {
        let mut registry = CommandRegistry::new();
        registry
            .register_plugin_command(
                "echo_plugin",
                Command {
                    name: "echo".to_string(),
                    description: "Echo from plugin".to_string(),
                    aliases: vec!["say".to_string()],
                    args: CommandArgsSchema::none(),
                    handler: plugin_echo_handler,
                },
            )
            .expect("plugin command should register");

        let removed = registry.unregister_plugin_commands("echo_plugin");
        assert_eq!(removed, 1);
        assert!(!registry.has_command("/echo"));
        assert!(!registry.has_command("/say"));
    }
}
