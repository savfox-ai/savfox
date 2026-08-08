# CLI 参考

## 全局选项

以下选项适用于所有子命令：

| 选项 | 说明 |
|------|------|
| `-m, --model <MODEL>` | 指定使用的 LLM 模型 |
| `-c, --config <KEY=VALUE>` | 覆盖配置值（可重复使用） |
| `--oss` | 使用本地开源提供商（Ollama、LM Studio） |
| `--search` | 启用网络搜索模式 |
| `--sandbox <MODE>` | 沙箱策略：`read-only`、`workspace-write`、`full-access` |
| `--ask-for-approval <POLICY>` | 审批策略：`never`、`on-request`、`unless-trusted`、`on-failure` |
| `--full-auto` | 低干预模式（`--ask-for-approval on-request --sandbox workspace-write`） |
| `--yolo` | 跳过所有确认和沙箱限制（**危险**） |
| `-C, --cd <DIR>` | 设置工作目录 |
| `-i, --image <FILE>` | 附加图片（逗号分隔路径） |
| `--add-dir <DIR>` | 沙箱模式下额外可写目录 |
| `--enable <FEATURE>` | 启用特性标志（可重复使用） |
| `--disable <FEATURE>` | 禁用特性标志（可重复使用） |

## 子命令

### `exec`（别名：`e`）

非交互式执行任务。

```bash
savfox exec "为注册表单添加输入验证"
savfox e "解释这个函数的作用"
```

**选项：**

| 选项 | 说明 |
|------|------|
| `--json` | 以 JSONL 格式输出事件（每行一个 JSON 对象） |
| `--color <MODE>` | 颜色输出：`auto`、`always`、`never` |
| `--output-last-message <FILE>` | 将智能体最终消息保存到文件 |
| `--output-schema <FILE>` | 指定期望响应格式的 JSON Schema 文件 |

### `review`

非交互式代码审查。

```bash
savfox review
savfox review "重点关注安全问题"
```

### `resume`

恢复之前的交互式会话。

```bash
savfox resume              # 交互式会话选择器
savfox resume --last       # 恢复最近的会话
savfox resume <SESSION_ID> # 恢复指定会话
```

### `fork`

从之前的会话分叉，探索不同方向。

```bash
savfox fork
```

### `apply`（别名：`a`）

将智能体生成的最新 diff 应用到工作目录。

```bash
savfox apply
```

### `login` / `logout`

管理认证凭证。

```bash
savfox login       # 交互式登录流程
savfox --oss login # 本地 OSS 提供商登录
savfox logout      # 移除已存储的凭证
```

### `gateway`

启动或管理网关服务器。

```bash
savfox gateway                          # 启动服务器
savfox gateway --port 8080 --token abc  # 自定义端口和令牌
savfox gateway status                   # 检查服务器状态
savfox gateway logs --follow            # 实时查看日志
savfox gateway models                   # 列出可用模型
```

详见 [网关服务器](gateway.md)。

**管理子命令：**

| 子命令 | 说明 |
|--------|------|
| `status` | 检查网关健康状态 |
| `logs [--follow] [--lines N]` | 查看或实时跟踪日志 |
| `models` | 列出可用的 LLM 模型 |
| `approvals {list\|approve\|deny}` | 管理执行审批 |
| `devices {list\|pair\|revoke}` | 管理设备配对 |
| `channels` | 管理聊天桥接通道 |
| `nodes` | 管理连接的节点 |

### `status`

查询需要认证的 Gateway 状态接口。Gateway 不可达、令牌被拒绝或响应数据无效时，命令会以非零状态退出。

```bash
savfox status --token "$SAVFOX_GATEWAY_TOKEN"
savfox status --format json
```

`--token` 默认读取 `SAVFOX_GATEWAY_TOKEN`（兼容回退到旧的 `SAVFOX_TOKEN`）；`--format` 支持 `table` 或 `json`。

### `mcp-server`

以 MCP（模型上下文协议）服务器模式运行 Savfox。

```bash
savfox mcp-server
```

详见 [MCP 服务器](mcp-server.md)。

### `mcp`

管理 MCP 服务器配置。

```bash
savfox mcp
```

### `app-server`

运行应用服务器（通过 stdio 的 JSON-RPC），供 IDE 扩展使用。

```bash
savfox app-server
```

**代码生成子命令：**

```bash
savfox app-server generate-ts -o ./types           # 生成 TypeScript 类型绑定
savfox app-server generate-json-schema -o ./schema  # 生成 JSON Schema
```

### `sandbox`

在平台原生沙箱中运行命令。

```bash
savfox sandbox macos <command>    # macOS Seatbelt
savfox sandbox linux <command>    # Linux Landlock + seccomp
savfox sandbox windows <command>  # Windows 受限令牌
```

详见 [沙箱与安全](sandbox.md)。

### `features`

查看和管理特性标志。

```bash
savfox features              # 列出所有特性标志
savfox features enable <F>   # 启用特性
savfox features disable <F>  # 禁用特性
```

### `cloud`（别名：`cloud-tasks`）

**实验性功能。** 管理云端任务。

```bash
savfox cloud exec "<query>" --env <ENV_ID>  # 提交任务
savfox cloud status <TASK_ID>               # 查看任务状态
savfox cloud list                           # 列出任务
savfox cloud apply <TASK_ID>                # 本地应用 diff
savfox cloud diff <TASK_ID>                 # 查看 diff
```

### `completion`

生成 Shell 自动补全脚本。

```bash
savfox completion bash > ~/.bash_completion.d/savfox
savfox completion zsh > ~/.zfunc/_savfox
savfox completion fish > ~/.config/fish/completions/savfox.fish
savfox completion powershell > savfox.ps1
```
