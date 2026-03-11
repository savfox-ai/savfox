# Custom Provider Configuration

Savfox supports any LLM API that implements the OpenAI Chat Completions protocol. You can add custom providers by configuring a `base_url`, selecting the appropriate `WireApi`, and optionally setting authentication headers.

## Wire API Formats

Savfox supports four wire-level API formats:

| WireApi | Protocol | Used By |
|---------|----------|---------|
| `Responses` | OpenAI Responses API | OpenAI (default) |
| `Chat` | OpenAI Chat Completions (`/v1/chat/completions`) | Ollama, LM Studio, Groq, Together, most OpenAI-compatible APIs |
| `Compact` | Compact Responses API | Optimized streaming format |
| `Anthropic` | Anthropic Messages API | Anthropic Claude |

Most third-party providers and local servers implement the Chat Completions format. Use `Chat` as the default for custom providers.

## Adding an OpenAI-Compatible Provider

Any service that implements `/v1/chat/completions` works with zero code changes. Just configure the base URL and credentials:

```toml
[model]
provider_id = "custom"
model = "my-model"
base_url = "https://api.example.com/v1"

[model.env_http_headers]
"Authorization" = "Bearer $MY_API_KEY"
```

### Example: Groq

```toml
[model]
provider_id = "groq"
model = "llama-3.1-70b-versatile"
base_url = "https://api.groq.com/openai/v1"

[model.env_http_headers]
"Authorization" = "Bearer $GROQ_API_KEY"
```

### Example: Together AI

```toml
[model]
provider_id = "together"
model = "meta-llama/Llama-3-70b-chat-hf"
base_url = "https://api.together.xyz/v1"

[model.env_http_headers]
"Authorization" = "Bearer $TOGETHER_API_KEY"
```

### Example: DeepSeek

```toml
[model]
provider_id = "deepseek"
model = "deepseek-chat"
base_url = "https://api.deepseek.com/v1"

[model.env_http_headers]
"Authorization" = "Bearer $DEEPSEEK_API_KEY"
```

### Example: Mistral

```toml
[model]
provider_id = "mistral"
model = "mistral-large-latest"
base_url = "https://api.mistral.ai/v1"

[model.env_http_headers]
"Authorization" = "Bearer $MISTRAL_API_KEY"
```

### Example: XAI (Grok)

```toml
[model]
provider_id = "xai"
model = "grok-2"
base_url = "https://api.x.ai/v1"

[model.env_http_headers]
"Authorization" = "Bearer $XAI_API_KEY"
```

### Example: LM Studio

```toml
[model]
provider_id = "lmstudio"
model = "local-model"
base_url = "http://localhost:1234/v1"
```

LM Studio runs locally and does not require an API key.

## Authentication Methods

### Bearer Token (Most Common)

```toml
[model.env_http_headers]
"Authorization" = "Bearer $API_KEY"
```

### Custom Header

Some providers use non-standard authentication headers:

```toml
[model.env_http_headers]
"x-api-key" = "$API_KEY"
```

### No Authentication

Local servers (Ollama, LM Studio) typically need no auth. Omit the `env_http_headers` section.

## Environment Variable Expansion

Header values prefixed with `$` are expanded from environment variables at runtime:

```toml
[model.env_http_headers]
"Authorization" = "Bearer $MY_SECRET_KEY"
```

Set the variable before running Savfox:

```bash
export MY_SECRET_KEY="sk-..."
savfox exec "Hello"
```

## Custom Query Parameters

Some providers require query parameters (e.g., Azure OpenAI API version):

```toml
[model]
provider_id = "azure"
model = "gpt-4o"
base_url = "https://my-resource.openai.azure.com/openai/deployments/gpt-4o"
```

## Per-Run Override

Switch providers on the fly without editing config:

```bash
savfox -c model.provider_id=groq -c model.model=llama-3.1-70b-versatile \
  -c model.base_url=https://api.groq.com/openai/v1 \
  exec "Summarize this file"
```

## Built-In Providers

These providers are registered by default and only need an API key:

`openai`, `ollama`, `lmstudio`, `anthropic`, `groq`, `xai`, `deepseek`, `mistral`, `together`, `doubao`, `zhipu`

For these, set `provider_id` to the name and provide credentials. The `base_url` and wire format are preconfigured.
