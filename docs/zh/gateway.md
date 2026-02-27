# 网关服务器

网关服务器提供 HTTP/WebSocket 远程访问能力，支持 Web 客户端、聊天平台集成和多设备使用。

## 启动网关

```bash
savfox gateway
savfox gateway --port 8080
savfox gateway --port 8080 --token my-secret-token
savfox gateway --host 0.0.0.0 --port 443 --tls-cert cert.pem --tls-key key.pem
```

**服务器选项：**

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--host <ADDR>` | `127.0.0.1` | 绑定地址 |
| `--port <PORT>` | `18881` | 监听端口 |
| `--token <TOKEN>` | 自动生成 | 认证用 Bearer 令牌 |
| `--tls-cert <PATH>` | — | HTTPS/WSS 所需的 TLS 证书 |
| `--tls-key <PATH>` | — | TLS 私钥 |

如果省略 `--token`，启动时会自动生成一个随机令牌并打印到控制台。

## 认证

所有 API 和 WebSocket 请求都需要 Bearer 令牌：

```
Authorization: Bearer <token>
```

令牌支持三种权限范围：
- **Operator** — 完全访问所有 API 和智能体执行
- **Viewer** — 只读访问状态、会话和日志
- **Chat** — 仅收发聊天消息

验证令牌：

```bash
curl -X POST http://localhost:18881/api/token/validate \
  -H "Authorization: Bearer <token>"
```

## REST API 端点

| 路径 | 方法 | 说明 |
|------|------|------|
| `/health` | GET | 健康检查 |
| `/api/status` | GET | 服务器状态信息 |
| `/api/config` | GET | 当前配置 |
| `/api/config/patch` | POST | 修补配置 |
| `/api/config/apply` | POST | 应用配置变更 |
| `/api/message` | POST | 向智能体发送消息 |
| `/api/sessions` | GET | 列出会话 |
| `/api/sessions/<id>/history` | GET | 会话对话历史 |
| `/api/restart` | POST | 重启服务器 |
| `/api/agent` | POST | 调用智能体 |
| `/api/agent/wait` | POST | 调用智能体并等待完成 |
| `/api/exec/approval/*` | GET/POST | 执行审批管理 |
| `/v1/chat/completions` | POST | OpenAI 兼容的 Chat API |
| `/v1/responses` | POST | OpenResponses API |
| `/tools/invoke` | POST | 工具调用端点 |

## WebSocket JSON-RPC

连接 WebSocket 端点进行实时双向通信：

```
ws://localhost:18881/ws
```

协议使用 JSON-RPC 2.0。请求示例：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "status",
  "params": {}
}
```

主要方法组：
- **Agent** — `agent`、`agent.identity`、`agent.wait`
- **Chat** — `chat.send`、`chat.history`、`chat.abort`
- **Sessions** — `sessions.list`、`sessions.preview`、`sessions.patch`、`sessions.reset`、`sessions.delete`
- **Config** — `config.get`、`config.set`、`config.apply`、`config.patch`
- **Cron** — `cron.list`、`cron.add`、`cron.update`、`cron.remove`、`cron.run`、`cron.runs`
- **Models** — `models.list`
- **System** — `connect`、`health`、`status`、`wake`、`send`

## 管理子命令

### 状态

```bash
savfox gateway status
```

### 日志

```bash
savfox gateway logs              # 查看最近日志
savfox gateway logs --follow     # 实时跟踪日志
savfox gateway logs --lines 100  # 最近 100 行
```

### 模型

```bash
savfox gateway models
```

### 审批

```bash
savfox gateway approvals list
savfox gateway approvals approve <ID>
savfox gateway approvals deny <ID> --reason "不安全"
```

### 设备

```bash
savfox gateway devices list
savfox gateway devices pair --name "我的手机"
savfox gateway devices revoke <ID>
```

### 通道

管理聊天桥接通道集成：

```bash
savfox gateway channels
```

### 节点

管理多节点配置中的已连接节点：

```bash
savfox gateway nodes
```

## 聊天平台桥接

网关可以将对话桥接到外部聊天平台。在 `config.toml` 中配置桥接：

```toml
[gateway.bridges.discord]
enabled = true
bot_token = "your-discord-bot-token"

[gateway.bridges.telegram]
enabled = true
bot_token = "your-telegram-bot-token"
webhook_secret_token = "可选的 telegram secret token"

[gateway.bridges.slack]
enabled = true
bot_token = "xoxb-your-slack-bot-token"
signing_secret = "your-signing-secret"

[gateway.bridges.webhook]
enabled = true
secret = "通用 webhook 的 HMAC 共享密钥"
```

**支持的平台：**

| 平台 | 配置键 | 必需字段 |
|------|--------|----------|
| Discord | `bridges.discord` | `bot_token` |
| Telegram | `bridges.telegram` | `bot_token` |
| Slack | `bridges.slack` | `bot_token`、`signing_secret` |
| Matrix | `bridges.matrix` | 服务器地址、凭证 |
| Mattermost | `bridges.mattermost` | 服务器地址、令牌 |
| Google Chat | `bridges.google_chat` | 服务账号 |
| Line | `bridges.line` | 频道令牌 |
| 飞书 | `bridges.feishu` | 应用凭证 |
| IRC | `bridges.irc` | 服务器、频道、昵称 |
| Webhook | `bridges.webhook` | URL/`secret`（建议配置） |

每个桥接在聊天平台和 Savfox 智能体之间双向路由消息。

### Webhook 安全校验

当配置了对应安全字段时，网关会强制校验请求来源：

- Discord：`x-signature-ed25519` + `x-signature-timestamp`
- Slack：`x-slack-signature` + `x-slack-request-timestamp`（5 分钟防重放窗口）
- Telegram：`x-telegram-bot-api-secret-token`
- 通用 Webhook：`x-signature` 或 `x-hub-signature-256`（HMAC-SHA256）

常见失败响应：

- `401 invalid_signature`：签名或 token 不匹配
- `401 missing_signature`：缺少必要签名头
- `401 stale_signature`：Slack 时间戳超出允许窗口

## 后台服务

网关运行以下后台服务：

- **会话清理** — 每 5 分钟清除过期会话
- **定时调度器** — 执行计划任务（每 60 秒检查一次）

### 定时任务服务

通过 WebSocket API 安排定期任务：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "cron.add",
  "params": {
    "name": "daily-report",
    "schedule": { "type": "cron", "expr": "0 9 * * *" },
    "payload": { "type": "agentTurn", "prompt": "生成每日状态报告" }
  }
}
```

调度类型：
- `at` — 一次性执行，在指定时间触发
- `every` — 按固定间隔重复
- `cron` — 标准 cron 表达式

## 速率限制

网关内置令牌桶速率限制器以防止滥用。速率限制按令牌粒度应用，可按需配置。
