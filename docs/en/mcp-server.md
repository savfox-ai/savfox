# MCP Server

Savfox can run as an [MCP (Model Context Protocol)](https://modelcontextprotocol.io/) server, exposing its capabilities as tools that other AI agents and applications can use.

## What is MCP?

Model Context Protocol is an open standard for connecting AI models to external tools and data sources. By running Savfox as an MCP server, you can integrate it into any MCP-compatible client (Claude Desktop, other AI IDEs, custom agents).

## Running the MCP Server

```bash
savfox mcp-server
```

This starts an MCP server that communicates over stdio using JSON-RPC 2.0. The server reads requests from stdin and writes responses to stdout.

## Configuring MCP Clients

To use Savfox as a tool in an MCP client, add it to the client's MCP server configuration.

### Example: Claude Desktop

In your Claude Desktop config (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "savfox": {
      "command": "savfox",
      "args": ["mcp-server"]
    }
  }
}
```

### Example: Generic MCP Client

Any MCP client that supports stdio transport can connect to Savfox:

```json
{
  "command": "savfox",
  "args": ["mcp-server"],
  "transport": "stdio"
}
```

## Managing MCP Servers

Savfox can also act as an MCP **client**, connecting to other MCP servers to extend its own tool capabilities.

### Configure MCP Servers in config.toml

```toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]

[mcp.servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_..." }
```

### Manage via CLI

```bash
savfox mcp          # manage MCP server configurations
```

## Protocol

The MCP server implements the standard MCP protocol:

- **Transport**: stdio (stdin/stdout)
- **Format**: JSON-RPC 2.0
- **Capabilities**: Tool listing, tool invocation, streaming results

When invoked as a tool by an external agent, Savfox can:
- Execute code analysis and modifications
- Run commands in sandboxed environments
- Apply patches and diffs
- Search and navigate codebases

## Approval Handling

When the MCP server needs to execute potentially dangerous operations, it follows the configured approval policy. The calling MCP client receives approval requests and can respond programmatically or prompt the user.
