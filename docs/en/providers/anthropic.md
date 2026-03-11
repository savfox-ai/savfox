# Anthropic Provider

The Anthropic provider connects Savfox to Claude models via the Anthropic API. It uses the `WireApi::Anthropic` wire format with Anthropic-specific authentication and request structure.

## Authentication

Anthropic uses the `x-api-key` header (not the standard `Authorization: Bearer` pattern). Configure your key:

```bash
savfox login
```

Or set it directly in `~/.savfox/config.toml`:

```toml
[auth]
provider = "anthropic"
api_key = "sk-ant-api03-..."
```

The key is injected via `env_http_headers` internally:

```toml
[model.env_http_headers]
"x-api-key" = "$ANTHROPIC_API_KEY"
```

You can also use an environment variable:

```bash
export ANTHROPIC_API_KEY="sk-ant-api03-..."
savfox -m claude-sonnet-4-20250514 exec "Review this code"
```

## Model Selection

Set the default model:

```toml
[model]
provider_id = "anthropic"
model = "claude-sonnet-4-20250514"
```

Override per-run:

```bash
savfox -m claude-sonnet-4-20250514 exec "Explain this function"
savfox -m claude-opus-4-20250514 exec "Design a new architecture"
```

### Available Models

| Model ID | Description |
|----------|-------------|
| `claude-opus-4-20250514` | Most capable, best for complex tasks |
| `claude-sonnet-4-20250514` | Balanced performance and speed |
| `claude-haiku-3-20250307` | Fastest, lowest cost |

Check the [Anthropic docs](https://docs.anthropic.com/en/docs/about-claude/models) for the latest model list.

## Base URL

The default base URL is `https://api.anthropic.com/v1`. Override it for proxies or custom deployments:

```toml
[model]
provider_id = "anthropic"
model = "claude-sonnet-4-20250514"
base_url = "https://my-proxy.example.com/v1"
```

## API Version Header

Anthropic requires an `anthropic-version` header. Savfox sets this automatically. If you need to pin a specific version:

```toml
[model.env_http_headers]
"anthropic-version" = "2024-01-01"
```

## Tool Use

Anthropic's tool use format differs from OpenAI's. Savfox handles the translation automatically. When the agent calls tools (shell commands, file edits, etc.), Savfox converts tool definitions and results to Anthropic's expected format.

No additional configuration is required for tool use to work.

## Streaming

Anthropic responses stream via SSE. The agent receives incremental tokens for real-time output. Anthropic's SSE format uses `event: content_block_delta` messages, which Savfox parses with a dedicated SSE handler.

## Retry Behavior

Requests to Anthropic retry automatically on:

- 429 (rate limit) -- with exponential backoff
- 5xx (server errors)
- Transport-level failures (timeouts, connection resets)

Default: up to 3 attempts.

## Example Configuration

A complete `config.toml` for Anthropic:

```toml
[auth]
provider = "anthropic"

[model]
provider_id = "anthropic"
model = "claude-sonnet-4-20250514"

[model.env_http_headers]
"x-api-key" = "$ANTHROPIC_API_KEY"
```

Then run:

```bash
export ANTHROPIC_API_KEY="sk-ant-api03-..."
savfox exec "Write tests for the user service"
```
