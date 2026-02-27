# savfox-login-oauth

Authentication and login library for Savfox. This crate implements the OAuth device code flow and PKCE-based browser login, allowing users to authenticate with the Savfox backend from both the TUI and headless CLI.

The crate provides `request_device_code` and `complete_device_code_login` for the device code grant flow, as well as `LoginServer` which starts a local HTTP server to receive the OAuth callback during browser-based PKCE authentication. It re-exports core authentication types and helpers from `savfox-core` and `savfox-app-server-protocol`, including `AuthManager`, `login_with_api_key`, `logout`, and `save_auth`, so downstream consumers can use this crate as a single entry point for all auth operations.
