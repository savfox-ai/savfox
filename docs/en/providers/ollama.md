# Ollama Provider

Ollama lets you run open-source LLMs locally. Savfox connects to Ollama's OpenAI-compatible API using the `WireApi::Chat` wire format, so no special protocol handling is needed.

## Prerequisites

1. Install Ollama from [ollama.com](https://ollama.com/).
2. Pull a model:
   ```bash
   ollama pull llama3
   ollama pull codellama
   ollama pull deepseek-coder-v2
   ```
3. Start the Ollama server (it typically runs on port 11434):
   ```bash
   ollama serve
   ```

## Configuration

### Quick Start with --oss

The fastest way to use Ollama:

```bash
savfox --oss login
```

This configures Savfox to use the local Ollama instance automatically.

### Manual Configuration

Set Ollama as your provider in `~/.savfox/config.toml`:

```toml
[model]
provider_id = "ollama"
model = "llama3"
base_url = "http://localhost:11434/v1"
```

The `/v1` suffix is important -- it points to Ollama's OpenAI-compatible endpoint.

### Override per-run

```bash
savfox -m ollama:llama3 exec "Explain this code"
savfox -m ollama:codellama exec "Add error handling to main.rs"
savfox -m ollama:deepseek-coder-v2 exec "Write unit tests"
```

## Custom Host and Port

If Ollama runs on a different machine or port:

```toml
[model]
provider_id = "ollama"
model = "llama3"
base_url = "http://192.168.1.50:11434/v1"
```

## Authentication

Ollama does not require an API key by default. If you have placed Ollama behind a reverse proxy that requires authentication, set headers:

```toml
[model]
provider_id = "ollama"
model = "llama3"
base_url = "http://localhost:11434/v1"

[model.env_http_headers]
"Authorization" = "Bearer $OLLAMA_API_KEY"
```

## Available Models

List models pulled on your Ollama instance:

```bash
ollama list
```

Popular models for coding tasks:

| Model | Size | Use Case |
|-------|------|----------|
| `llama3` | 8B | General purpose |
| `codellama` | 7B-34B | Code generation and analysis |
| `deepseek-coder-v2` | 16B | Strong code completion |
| `mistral` | 7B | Fast general purpose |
| `qwen2.5-coder` | 7B-32B | Code-focused |

## Performance Tips

- **GPU acceleration**: Ollama uses your GPU by default if available. Check with `ollama ps`.
- **Context length**: Large codebases may exceed the model's context window. Use smaller, focused prompts.
- **Model size**: Larger models (34B+) need more VRAM. Start with 7B-8B models if resources are limited.

## Ollama Chat vs Ollama Completion

Savfox uses Ollama's `/v1/chat/completions` endpoint (Chat wire format). This is the recommended endpoint for conversational use. The older `/api/generate` endpoint is not used.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| Connection refused | Verify Ollama is running: `ollama serve` |
| Model not found | Pull the model first: `ollama pull <model>` |
| Slow responses | Check GPU usage; consider a smaller model |
| Out of memory | Use a smaller model or reduce context size |
