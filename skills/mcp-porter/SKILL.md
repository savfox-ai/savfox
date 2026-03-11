---
name: mcp-porter
description: Bridge MCP (Model Context Protocol) servers as Savfox tools.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🔌"
    requires:
      bins: []
      env: []
    install: []
---

# MCP Porter Skill

Bridge external MCP servers into Savfox, making their tools available to the agent.

## List Available MCP Servers

Check configured MCP servers:
```bash
savfox mcp list
```

## Start an MCP Server

Launch an MCP server and connect:
```bash
savfox mcp start --name my-server --command "npx -y @modelcontextprotocol/server-filesystem /path/to/dir"
```

## Common MCP Servers

### Filesystem Server
```bash
npx -y @modelcontextprotocol/server-filesystem /path/to/allowed/dir
```

### GitHub Server
```bash
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx npx -y @modelcontextprotocol/server-github
```

### PostgreSQL Server
```bash
npx -y @modelcontextprotocol/server-postgres postgresql://user:pass@localhost/db
```

### Brave Search Server
```bash
BRAVE_API_KEY=xxx npx -y @modelcontextprotocol/server-brave-search
```

### Puppeteer Server
```bash
npx -y @modelcontextprotocol/server-puppeteer
```

## Configuration

Add MCP servers to `config.toml`:
```toml
[[mcp_servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/docs"]

[[mcp_servers]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_xxx" }
```

## Inspect Server Capabilities

```bash
savfox mcp inspect --name my-server
```

Shows available tools, resources, and prompts from the server.

## Guidelines

- MCP servers run as child processes — they are stopped when Savfox exits
- Each server's tools are namespaced: `mcp_<server-name>_<tool-name>`
- Servers that crash are automatically restarted (up to 3 retries)
- Use `savfox mcp logs --name my-server` to debug connection issues
- Prefer official `@modelcontextprotocol/*` servers when available
