# savfox-mcp-server

MCP (Model Context Protocol) server implementation for Savfox. This crate provides both a library and a binary (`savfox-mcp-server`) that exposes Savfox as a tool to other MCP-compatible AI agents over the JSON-RPC stdio transport.

The server reads JSON-RPC messages from stdin, processes them through a `MessageProcessor` that handles MCP requests, responses, and notifications, and writes outgoing messages to stdout. It supports tool execution with configurable approval policies for shell commands and file patches. Configuration is loaded from the standard Savfox config files with optional CLI overrides via `-c` flags. The crate depends on the `rmcp` library for MCP protocol types and message parsing.
