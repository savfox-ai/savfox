# savfox-ollama

Ollama integration for Savfox. This crate provides a client (`OllamaClient`) for communicating with a local Ollama server to support the `--oss` (open-source/local) mode.

The `ensure_oss_ready` function verifies the Ollama server is reachable, checks whether the requested model is available locally, and pulls it if missing using a progress reporter (with both CLI and TUI variants). The crate also includes wire API detection logic that queries the Ollama server version to determine whether to use the Chat or Responses API -- servers at version 0.13.4 or later use the Responses API. The default OSS model is `gpt-oss:20b`.
