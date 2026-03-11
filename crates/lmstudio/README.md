# savfox-lmstudio

LM Studio integration for Savfox. This crate provides a client (`LMStudioClient`) that communicates with a local LM Studio server to manage models for the `--oss` (open-source/local) mode.

The `ensure_oss_ready` function verifies that the LM Studio server is reachable, checks whether the requested model is available locally, downloads it if missing, and starts loading it in the background. The default OSS model is `openai/gpt-oss-20b`. The crate depends on `savfox-core` for configuration and uses `reqwest` for HTTP communication with the LM Studio API.
