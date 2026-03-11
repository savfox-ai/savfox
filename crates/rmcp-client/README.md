# savfox-rmcp-client

An MCP (Model Context Protocol) client built on top of the `rmcp` crate. This crate provides `RmcpClient`, which can connect to MCP servers via stdio (child process) or streamable HTTP transports, list available tools, invoke them, and handle elicitation flows (interactive prompts from the server to the user).

The crate includes full OAuth 2.0 authentication support for streamable HTTP MCP servers. It can perform browser-based OAuth login, persist and load OAuth tokens via the platform keyring (through `savfox-keyring-store`), and determine the authentication status of a remote server before connecting. The `perform_oauth_login` module drives the authorization code flow with PKCE, spinning up a local HTTP callback server to receive the redirect.

Key public exports include `RmcpClient` (the main client), OAuth token management functions (`save_oauth_tokens`, `delete_oauth_tokens`, `load_oauth_tokens`), authentication status helpers (`determine_streamable_http_auth_status`, `supports_oauth_login`), and elicitation types for interactive tool use. Platform-specific keyring backends are selected at compile time for Linux, macOS, Windows, and BSD variants.
