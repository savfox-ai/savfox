# MCP 服务器

Savfox 可以作为 [MCP（模型上下文协议）](https://modelcontextprotocol.io/) 服务器运行，将自身能力作为工具暴露给其他 AI 智能体和应用使用。

## 什么是 MCP？

模型上下文协议（Model Context Protocol）是一个开放标准，用于将 AI 模型连接到外部工具和数据源。将 Savfox 作为 MCP 服务器运行后，你可以将其集成到任何兼容 MCP 的客户端中（Claude Desktop、其他 AI IDE、自定义智能体等）。

## 运行 MCP 服务器

```bash
savfox mcp-server
```

这会启动一个通过 stdio 使用 JSON-RPC 2.0 通信的 MCP 服务器。服务器从 stdin 读取请求，将响应写入 stdout。

## 配置 MCP 客户端

要在 MCP 客户端中使用 Savfox 作为工具，需要将其添加到客户端的 MCP 服务器配置中。

### 示例：Claude Desktop

在 Claude Desktop 配置文件（`claude_desktop_config.json`）中：

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

### 示例：通用 MCP 客户端

任何支持 stdio 传输的 MCP 客户端都可以连接到 Savfox：

```json
{
  "command": "savfox",
  "args": ["mcp-server"],
  "transport": "stdio"
}
```

## 管理 MCP 服务器

Savfox 也可以作为 MCP **客户端**，连接到其他 MCP 服务器来扩展自身的工具能力。

### 在 config.toml 中配置 MCP 服务器

```toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]

[mcp.servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_..." }
```

### 通过 CLI 管理

```bash
savfox mcp          # 管理 MCP 服务器配置
```

## 协议

MCP 服务器实现标准 MCP 协议：

- **传输方式**：stdio（stdin/stdout）
- **数据格式**：JSON-RPC 2.0
- **能力**：工具列表、工具调用、流式结果

当被外部智能体作为工具调用时，Savfox 可以：
- 执行代码分析和修改
- 在沙箱环境中运行命令
- 应用补丁和 diff
- 搜索和导航代码库

## 审批处理

当 MCP 服务器需要执行可能存在风险的操作时，它会遵循已配置的审批策略。调用方 MCP 客户端会收到审批请求，可以通过程序化方式响应或提示用户确认。
