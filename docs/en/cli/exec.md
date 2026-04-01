# Non-Interactive Execution (`savfox exec`)

The `exec` subcommand (alias `e`) runs a single task non-interactively. The agent processes your prompt, executes any necessary tool calls, and exits when finished. This is the primary interface for scripting, CI pipelines, and one-shot tasks.

## Basic Usage

```bash
savfox exec "Add error handling to the parse_config function"
savfox e "Explain what src/main.rs does"
```

The prompt is a positional argument. Wrap it in quotes if it contains spaces or shell metacharacters.

## Flags

| Flag | Description |
|------|-------------|
| `--json` | Emit events as newline-delimited JSON (JSONL) to stdout |
| `--color <MODE>` | Color output: `auto`, `always`, `never` |
| `--output-last-message <FILE>` | Write the agent's final text response to a file |
| `--output-schema <FILE>` | JSON Schema file that constrains the response shape |

Global flags like `--model`, `--sandbox`, and `--full-auto` also apply. See the [CLI Reference](../cli-reference.md) for the full list.

## Model Selection

Override the default model for a single run:

```bash
savfox -m gpt-4o exec "Refactor the auth module"
savfox -m claude-sonnet-4-20250514 exec "Review this code for security issues"
savfox -m ollama:llama3 exec "Summarize this file"
```

## Stdin Piping

When no prompt argument is given, `exec` reads from stdin. This lets you pipe content into the agent:

```bash
echo "Explain this error" | savfox exec
cat error.log | savfox exec "What went wrong here?"
git diff HEAD~1 | savfox exec "Review this diff"
```

You can combine piped input with a prompt argument. The piped content becomes additional context alongside the prompt text.

## JSON Output

The `--json` flag switches output to JSONL, with one JSON object per line. Each object represents an event from the agent (text output, tool call, tool result, etc.):

```bash
savfox exec --json "List all TODO comments" > results.jsonl
```

This is useful for programmatic consumption. Parse each line independently:

```bash
savfox exec --json "Count lines of code" | jq '.type'
```

## Structured Output with Schema

Constrain the agent's final response to match a JSON Schema:

```bash
savfox exec --output-schema schema.json "Extract all function signatures from src/lib.rs"
```

Where `schema.json` defines the expected shape:

```json
{
  "type": "object",
  "properties": {
    "functions": {
      "type": "array",
      "items": { "type": "string" }
    }
  }
}
```

## Sandbox and Approval

Control what the agent can do during execution:

```bash
# Read-only analysis (no file writes)
savfox --sandbox read-only exec "Explain the architecture"

# Allow writes to the workspace only
savfox --sandbox workspace-write exec "Fix the failing tests"

# Full auto mode (workspace-write + relaxed approval)
savfox --full-auto exec "Refactor the database layer"
```

## Saving the Final Message

Write the agent's last text response to a file for downstream use:

```bash
savfox exec --output-last-message summary.txt "Summarize the recent changes"
```

## Config Overrides

Pass ad-hoc config values without editing `config.toml`:

```bash
savfox -c model.model=gpt-4o -c sandbox.mode=read-only exec "Audit this code"
```

## Examples

```bash
# One-shot code generation
savfox exec "Create a Rust function that validates email addresses"

# Pipe a file for analysis
cat src/lib.rs | savfox exec "Find potential bugs in this code"

# CI pipeline usage
savfox exec --json --full-auto "Run clippy and fix all warnings" > fix-report.jsonl

# Use a local model
savfox -m ollama:codellama exec "Add doc comments to all public functions"
```
