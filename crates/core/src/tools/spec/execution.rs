use std::collections::BTreeMap;

use super::JsonSchema;
use super::declarations::{FunctionToolDecl, function_tool};
use crate::client_common::tools::ToolSpec;

fn create_approval_parameters(include_prefix_rule: bool) -> BTreeMap<String, JsonSchema> {
    let mut properties = BTreeMap::from([
        (
            "sandbox_permissions".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Sandbox permissions for the command. Set to \"require_escalated\" to request running without sandbox restrictions; defaults to \"use_default\".".to_owned(),
                ),
            },
        ),
        (
            "justification".to_owned(),
            JsonSchema::String {
                description: Some(
                    r#"Only set if sandbox_permissions is \"require_escalated\". 
                    Request approval from the user to run this command outside the sandbox. 
                    Phrased as a simple question that summarizes the purpose of the 
                    command as it relates to the task at hand - e.g. 'Do you want to 
                    fetch and pull the latest version of this git branch?'"#.to_owned(),
                ),
            },
        ),
    ]);

    if include_prefix_rule {
        properties.insert(
            "prefix_rule".to_owned(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some(
                    r#"Only specify when sandbox_permissions is `require_escalated`. 
                    Suggest a prefix command pattern that will allow you to fulfill similar requests from the user in the future.
                    Should be a short but reasonable prefix, e.g. [\"git\", \"pull\"] or [\"uv\", \"run\"] or [\"pytest\"]."#.to_owned(),
                ),
            },
        );
    }

    properties
}

pub(super) fn create_exec_command_tool(include_prefix_rule: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "cmd".to_owned(),
            JsonSchema::String {
                description: Some("Shell command to execute.".to_owned()),
            },
        ),
        (
            "workdir".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional working directory to run the command in; defaults to the turn cwd.".to_owned(),
                ),
            },
        ),
        (
            "shell".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Shell binary to launch. Defaults to the user's default shell.".to_owned(),
                ),
            },
        ),
        (
            "login".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to run the shell with -l/-i semantics. Defaults to true.".to_owned(),
                ),
            },
        ),
        (
            "tty".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to allocate a TTY for the command. Defaults to false (plain pipes); set to true to open a PTY and access TTY process.".to_owned(),
                ),
            },
        ),
        (
            "yield_time_ms".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "How long to wait (in milliseconds) for output before yielding.".to_owned(),
                ),
            },
        ),
        (
            "max_output_tokens".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of tokens to return. Excess output will be truncated.".to_owned(),
                ),
            },
        ),
    ]);
    properties.extend(create_approval_parameters(include_prefix_rule));

    function_tool(FunctionToolDecl {
        name: "exec_command",
        description: "Runs a command in a PTY, returning output or a session ID for ongoing interaction.",
        properties,
        required: &["cmd"],
    })
}

pub(super) fn create_write_stdin_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_owned(),
            JsonSchema::Number {
                description: Some("Identifier of the running unified exec session.".to_owned()),
            },
        ),
        (
            "chars".to_owned(),
            JsonSchema::String {
                description: Some("Bytes to write to stdin (may be empty to poll).".to_owned()),
            },
        ),
        (
            "yield_time_ms".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "How long to wait (in milliseconds) for output before yielding.".to_owned(),
                ),
            },
        ),
        (
            "max_output_tokens".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of tokens to return. Excess output will be truncated.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "write_stdin",
        description: "Writes characters to an existing unified exec session and returns recent output.",
        properties,
        required: &["session_id"],
    })
}

pub(super) fn create_shell_tool(include_prefix_rule: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "command".to_owned(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("The command to execute".to_owned()),
            },
        ),
        (
            "workdir".to_owned(),
            JsonSchema::String {
                description: Some("The working directory to execute the command in".to_owned()),
            },
        ),
        (
            "timeout_ms".to_owned(),
            JsonSchema::Number {
                description: Some("The timeout for the command in milliseconds".to_owned()),
            },
        ),
    ]);
    properties.extend(create_approval_parameters(include_prefix_rule));

    let description = if cfg!(windows) {
        r#"Runs a Powershell command (Windows) and returns its output. Arguments to `shell` will be passed to CreateProcessW(). Most commands should be prefixed with ["powershell.exe", "-Command"].
        
Examples of valid command strings:

- ls -a (show hidden): ["powershell.exe", "-Command", "Get-ChildItem -Force"]
- recursive find by name: ["powershell.exe", "-Command", "Get-ChildItem -Recurse -Filter *.py"]
- recursive grep: ["powershell.exe", "-Command", "Get-ChildItem -Path C:\\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive"]
- ps aux | grep python: ["powershell.exe", "-Command", "Get-Process | Where-Object { $_.ProcessName -like '*python*' }"]
- setting an env var: ["powershell.exe", "-Command", "$env:FOO='bar'; echo $env:FOO"]
- running an inline Python script: ["powershell.exe", "-Command", "@'\\nprint('Hello, world!')\\n'@ | python -"]"#
    } else {
        r#"Runs a shell command and returns its output.
- The arguments to `shell` will be passed to execvp(). Most terminal commands should be prefixed with ["bash", "-lc"].
- Always set the `workdir` param when using the shell function. Do not use `cd` unless absolutely necessary."#
    }.to_owned();

    function_tool(FunctionToolDecl {
        name: "shell",
        description: &description,
        properties,
        required: &["command"],
    })
}

pub(super) fn create_shell_command_tool(include_prefix_rule: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "command".to_owned(),
            JsonSchema::String {
                description: Some(
                    "The shell script to execute in the user's default shell".to_owned(),
                ),
            },
        ),
        (
            "workdir".to_owned(),
            JsonSchema::String {
                description: Some("The working directory to execute the command in".to_owned()),
            },
        ),
        (
            "login".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to run the shell with login shell semantics. Defaults to true.".to_owned(),
                ),
            },
        ),
        (
            "timeout_ms".to_owned(),
            JsonSchema::Number {
                description: Some("The timeout for the command in milliseconds".to_owned()),
            },
        ),
    ]);
    properties.extend(create_approval_parameters(include_prefix_rule));

    let description = if cfg!(windows) {
        r#"Runs a Powershell command (Windows) and returns its output.
        
Examples of valid command strings:

- ls -a (show hidden): "Get-ChildItem -Force"
- recursive find by name: "Get-ChildItem -Recurse -Filter *.py"
- recursive grep: "Get-ChildItem -Path C:\\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive"
- ps aux | grep python: "Get-Process | Where-Object { $_.ProcessName -like '*python*' }"
- setting an env var: "$env:FOO='bar'; echo $env:FOO"
- running an inline Python script: "@'\\nprint('Hello, world!')\\n'@ | python -"#
    } else {
        r#"Runs a shell command and returns its output.
- Always set the `workdir` param when using the shell_command function. Do not use `cd` unless absolutely necessary."#
    }.to_owned();

    function_tool(FunctionToolDecl {
        name: "shell_command",
        description: &description,
        properties,
        required: &["command"],
    })
}
