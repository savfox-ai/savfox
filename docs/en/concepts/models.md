# Models

Savfox supports multiple LLM providers and models. This guide covers model configuration, selection, and management.

## Built-in Providers

| Provider | Wire API | Auth Header | Default Base URL |
|----------|----------|-------------|-----------------|
| OpenAI | Chat | Bearer | `https://api.openai.com/v1` |
| Anthropic | Anthropic | x-api-key | `https://api.anthropic.com` |
| Ollama | Chat | None | `http://localhost:11434/v1` |
| Groq | Chat | Bearer | `https://api.groq.com/openai/v1` |
| xAI | Chat | Bearer | `https://api.x.ai/v1` |
| DeepSeek | Chat | Bearer | `https://api.deepseek.com` |
| Mistral | Chat | Bearer | `https://api.mistral.ai/v1` |
| Together | Chat | Bearer | `https://api.together.xyz/v1` |
| LM Studio | Chat | None | `http://localhost:1234/v1` |

## Configuration

### Set Default Model

```toml
[model]
default = "gpt-4o"
```

### API Keys

Set via environment variables:
```bash
export OPENAI_API_KEY="sk-..."
export ANTHROPIC_API_KEY="sk-ant-..."
```

Or in config:
```toml
[providers.openai]
api_key = "sk-..."
```

### Custom Providers

Any OpenAI-compatible API can be used:

```toml
[[custom_providers]]
id = "my-llm"
name = "My LLM Service"
base_url = "https://my-llm.example.com/v1"
api_key_env = "MY_LLM_KEY"
wire_api = "chat"
models = ["my-model-7b", "my-model-70b"]
```

## Wire API Types

| Type | Description | Used By |
|------|-------------|---------|
| `Chat` | OpenAI Chat Completions format | Most providers |
| `Responses` | OpenAI Responses API | OpenAI (new) |
| `Anthropic` | Anthropic Messages format | Anthropic |
| `Compact` | Lightweight streaming format | Internal |

## Model Selection

### Per-Session

Override model for a specific session via WS-RPC:
```json
{"method": "sessions.update", "params": {"id": "session-id", "model": "claude-sonnet-4-20250514"}}
```

### Per-Agent

Set model in agent configuration:
```toml
[agents.my-agent]
model = "gpt-4o"
```

### CLI Override

```bash
savfox exec --model claude-sonnet-4-20250514 "Explain this code"
```

## Model Fallback

Configure fallback chains for high availability:

```toml
[model.fallback]
chain = ["gpt-4o", "claude-sonnet-4-20250514", "llama-3.1-70b"]
triggers = ["rate_limit", "server_error", "timeout"]
max_retries = 2
```

## Listing Models

### CLI
```bash
savfox gateway models
```

### WS-RPC
```json
{"method": "models.list", "params": {}, "id": 1}
```

### REST API
```bash
curl http://localhost:18881/v1/models -H "Authorization: Bearer $TOKEN"
```

## See Also

- [Provider Guides](../providers/openai.md)
- [Custom Provider Setup](../providers/custom.md)
