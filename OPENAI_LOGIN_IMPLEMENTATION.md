# OpenAI Login Methods Implementation - Todo List

## Overview
Implement three login methods similar to Codex CLI:
1. Sign in with ChatGPT (Browser OAuth)
2. Sign in with Device Code
3. Provide your own API key

## Status Legend
- [ ] Pending
- [x] Completed
- [~] Partially Complete

---

## 1. Sign in with ChatGPT (Browser OAuth Flow)

### 1.1 Core OAuth Implementation
- [x] PKCE code generation (code_verifier, code_challenge) - `crates/login-oauth/src/pkce.rs`
- [x] Local OAuth callback server on port 1455 - `crates/login-oauth/src/server.rs`
- [x] Authorization URL construction with all required params - `server.rs:build_authorize_url()`
- [x] State parameter for CSRF protection
- [x] Browser auto-launch support
- [x] Token exchange endpoint (`/oauth/token`)
- [x] Token persistence to models/chatgpt.json

### 1.2 Token Management
- [x] ID token parsing and validation
- [x] Access token storage
- [x] Refresh token storage
- [x] Token refresh logic (8-day interval) - `crates/core/src/auth.rs`
- [x] API key exchange via token-exchange grant - `server.rs:obtain_api_key()`

### 1.3 TUI Integration
- [x] Browser login flow UI - `crates/tui/src/onboarding/auth.rs`
- [x] "Continue in browser" state rendering
- [x] Success message display
- [x] Cancel handling (Esc key)

### 1.4 Workspace Restrictions
- [x] Forced workspace ID validation - `server.rs:ensure_workspace_allowed()`
- [x] Account ID matching for token refresh

---

## 2. Sign in with Device Code (Headless Flow)

### 2.1 Device Code Implementation
- [x] Request user code from `/deviceauth/usercode` - `crates/login-oauth/src/device_code_auth.rs`
- [x] Display verification URL and one-time code
- [x] Polling for token completion (`/deviceauth/token`)
- [x] 15-minute timeout handling
- [x] Token exchange after authorization

### 2.2 TUI Integration
- [x] Device code login flow UI - `crates/tui/src/onboarding/auth.rs`
- [x] Shimmer animation while waiting
- [x] OSC 8 hyperlinks for terminal emulators
- [x] Cancel handling (Esc key)
- [x] Headless login helper - `crates/tui/src/onboarding/auth/headless_chatgpt_login.rs`

### 2.3 CLI Support
- [x] `--device-auth` flag support
- [x] Fallback to browser login if device code not supported

---

## 3. Provide Your Own API Key

### 3.1 API Key Implementation
- [x] Environment variable detection (`OPENAI_API_KEY`, `SAVFOX_API_KEY`) - `crates/core/src/auth.rs`
- [x] API key validation (non-empty check)
- [x] Persistence to models/chatgpt.json

### 3.2 TUI Integration
- [x] API key input UI - `crates/tui/src/onboarding/auth.rs`
- [x] Pre-population from environment variable
- [x] Paste support
- [x] Input masking/visibility

### 3.3 CLI Support
- [x] `--with-api-key` flag for stdin input
- [x] API key mode in login status

---

## 4. Common Infrastructure

### 4.1 Auth Storage
- [x] File-based storage (`models/chatgpt.json`) - `crates/core/src/auth/storage.rs`
- [x] Keyring storage support
- [x] Ephemeral (in-memory) storage for external tokens
- [x] Auto mode (keyring with file fallback)

### 4.2 Auth Manager
- [x] Central `AuthManager` singleton - `crates/core/src/auth.rs`
- [x] Token refresh automation
- [x] 401 recovery handling
- [x] External auth refresher support

### 4.3 Auth Modes
- [x] `ApiKey` mode
- [x] `Chatgpt` mode (managed OAuth)
- [x] `ChatgptAuthTokens` mode (external tokens)

### 4.4 Login Restrictions
- [x] Forced login method enforcement
- [x] Forced workspace ID enforcement
- [x] Auto-logout on restriction violation

---

## 5. CLI Commands

### 5.1 Login Commands
- [x] `savfox login` - browser login
- [x] `savfox login --device-auth` - device code login
- [x] `savfox login --with-api-key` - API key from stdin
- [x] `savfox login status` - show current auth status
- [x] `savfox logout` - clear auth

### 5.2 CLI Implementation
- [x] Login command handler - `crates/savfox-cli/src/login.rs`
- [x] Status display with plan type
- [x] Account email display

---

## Implementation Summary

All three login methods are **fully implemented** in the Savfox codebase:

| Feature | Status | Location |
|---------|--------|----------|
| Browser OAuth | Complete | `crates/login-oauth/src/server.rs` |
| Device Code | Complete | `crates/login-oauth/src/device_code_auth.rs` |
| API Key | Complete | `crates/core/src/auth.rs` |
| TUI Auth Flow | Complete | `crates/tui/src/onboarding/auth.rs` |
| Auth Manager | Complete | `crates/core/src/auth.rs` |
| CLI Commands | Complete | `crates/savfox-cli/src/login.rs` |

---

## Potential Improvements (Optional)

- [ ] Add API key format validation (sk- prefix check)
- [ ] Add token expiration display in status
- [ ] Add multi-account support
- [ ] Add session management UI
- [ ] Improve error messages for network failures
