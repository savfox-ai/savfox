# OpenAI Provider

OpenAI is the default provider for Savfox. It supports the Responses API, Chat Completions API, and Compact API through the `WireApi::Responses`, `WireApi::Chat`, and `WireApi::Compact` wire formats.

## Authentication

### API Key (Recommended for Developers)

Set your API key via `savfox login`:

```bash
savfox login
```

Or configure it directly in `~/.savfox/config.toml`:

```toml
[auth]
provider = "openai"
api_key = "sk-proj-..."
```

The key is sent as a standard `Authorization: Bearer <key>` header.

### ChatGPT OAuth

For ChatGPT Plus/Team subscribers, Savfox supports OAuth-based authentication that manages token refresh automatically:

```bash
savfox login
```

Select the ChatGPT option when prompted. This opens a browser-based OAuth flow.

## Model Selection

Set the default model in your config:

```toml
[model]
provider_id = "openai"
model = "gpt-4o"
```

Override per-run with the `--model` flag:

```bash
savfox -m gpt-4o exec "Explain this code"
savfox -m o3-mini exec "Solve this problem step by step"
savfox -m gpt-4.1 exec "Review this PR"
```

### Available Models

Any model available through the OpenAI API can be used. Common choices:

| Model | Use Case |
|-------|----------|
| `gpt-4o` | General-purpose, fast, multimodal |
| `gpt-4.1` | Strong at coding and instruction following |
| `o3-mini` | Reasoning tasks with lower cost |
| `o3` | Maximum reasoning capability |

The full list depends on your API plan. Check with `savfox gateway models` or the OpenAI dashboard.

## Base URL Override

Point Savfox at an OpenAI-compatible API by overriding the base URL. This works with Azure OpenAI, proxy servers, or any service that implements the OpenAI API:

```toml
[model]
provider_id = "openai"
model = "gpt-4o"
base_url = "https://my-proxy.example.com/v1"
```

Or via CLI override:

```bash
savfox -c model.base_url=https://my-proxy.example.com/v1 exec "Hello"
```

### Azure OpenAI

For Azure deployments, set the base URL to your Azure endpoint:

```toml
[model]
provider_id = "openai"
model = "gpt-4o"
base_url = "https://my-resource.openai.azure.com/openai/deployments/my-deployment"
```

Azure uses a different auth header. Set the key via environment variable:

```bash
export AZURE_OPENAI_API_KEY="your-azure-key"
```

## Environment HTTP Headers

Inject custom headers into every API request:

```toml
[model]
provider_id = "openai"
model = "gpt-4o"

[model.env_http_headers]
"X-Custom-Header" = "my-value"
```

## Retry Configuration

OpenAI requests include automatic retries with exponential backoff for transient errors (429 rate limits, 5xx server errors):

- Max attempts: 3
- Retries on 429 (rate limit): yes
- Retries on 5xx: yes
- Retries on transport errors: yes

These defaults work well for most use cases and do not require manual tuning.

## Streaming

All OpenAI requests use server-sent events (SSE) for streaming. The agent receives tokens incrementally as they are generated, providing real-time output in both interactive and non-interactive modes.
