# Gateway 配置参考

Savfox gateway 通过 CLI 参数、`config.toml` 中的 `[gateway]` 段以及环境变量共同配置。

## 配置来源优先级

1. CLI 参数
2. `config.toml` 中的 `[gateway]`
3. 环境变量
4. 内置默认值

## CLI 启动参数

```bash
savfox gateway [OPTIONS] [SUBCOMMAND]
```

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--host` | `127.0.0.1` | 绑定地址 |
| `--port` | `18881` | 监听端口 |
| `--token` | 自动生成 | 静态 bearer token |
| `--tls-cert` | 无 | TLS 证书路径 |
| `--tls-key` | 无 | TLS 私钥路径 |

## `config.toml`

典型配置如下：

```toml
[gateway]
host = "127.0.0.1"
port = 18881

[gateway.channels.discord]
enabled = true
bot_token = "your-discord-bot-token"

[gateway.channels.telegram]
enabled = true
bot_token = "123456:ABC..."

[gateway.channels.slack]
enabled = true
bot_token = "xoxb-your-token"
signing_secret = "your-signing-secret"
```

## `[gateway]` 常见字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `host` | `IpAddr` | 绑定地址 |
| `port` | `u16` | 监听端口 |
| `token` | `Option<String>` | 静态认证 token |
| `tls_cert` | `Option<String>` | TLS 证书路径 |
| `tls_key` | `Option<String>` | TLS 私钥路径 |
| `channels` | `ChannelsConfig` | 各聊天渠道配置 |

## 渠道配置

常见渠道包括：

- Discord
- Telegram
- Slack
- Microsoft Teams
- Webhook
- WhatsApp
- Signal
- iMessage
- Zalo

这些渠道通常至少包含：

- `enabled`
- 平台凭据（如 `bot_token` / `access_token`）
- 可选的 webhook / 签名验证字段

## 环境变量

如果配置文件中没有对应值，很多渠道也支持从环境变量读取凭据，例如：

- `DISCORD_BOT_TOKEN`
- `TELEGRAM_BOT_TOKEN`
- `SLACK_BOT_TOKEN`
- `MATRIX_HOMESERVER`
- `MATRIX_ACCESS_TOKEN`

## 运行时更新

gateway 支持在运行时通过 API 修改配置，而不一定要重启：

- merge patch：只修改指定字段
- full apply：整体替换配置

## 建议

- 长期配置放在 `config.toml`
- 敏感凭据优先放环境变量或安全密钥管理系统
- 若对外暴露服务，务必设置 `token` 或更完整的认证机制
- 需要公网访问时再配置 `TLS`

更细的字段定义请以当前代码和 schema 为准。

## 本地 Claude terminal agent

实验性纯 Rust `claude_stream` 模式使用独立模型/设置、显式版本门槛、按凭证隔离的审批及持久会话恢复。配置、停止、权限边界和旧 one-shot 迁移说明见[本地 Claude Code](claude-code.md)。


### 公共 Webhook 鉴权

所有 `/webhooks/<platform>` 回调必须具有明确启用的已保存 Channel，或旧版 `[gateway.channels.<platform>]` 配置。未配置或已禁用时返回 503；已保存 Channel 的禁用状态优先于旧版启用配置。验证凭据不得为空；缺少凭据或签名、签名错误、签名后修改请求体均返回 401，不会启动 Agent。Gateway Bearer Token 不能代替平台回调鉴权。

现有平台验证格式保持兼容，包括 Discord 公钥、Slack 签名密钥、Telegram Secret Token、LINE/WhatsApp 签名密钥、Zalo 的 `x-zalooa-signature` 以及其他集成的验证 Token。飞书 HTTP 回调必须配置 Verification Token；WhatsApp 初始化验证 GET 也必须配置并匹配 Verify Token。轮询、长连接及经过鉴权的 Matrix Appservice 路由继续采用原有传输层鉴权。

本地 IRC、Signal、钉钉及旧版 `/webhooks/matrix` HTTP 转发桥须在各自已保存 Channel 的 `config` 对象内配置独立的 `webhook_secret`，或配置 `IRC_WEBHOOK_SECRET`、`SIGNAL_WEBHOOK_SECRET`、`DINGTALK_WEBHOOK_SECRET`、`MATRIX_WEBHOOK_SECRET` 环境变量，同时明确启用对应 Channel。转发桥必须发送 `x-hub-signature-256: sha256=<HMAC-SHA256(secret, 原始请求体)的十六进制值>`。原生 Matrix Appservice 事务仍走现有 Homeserver Token 路由。

MS Teams HTTP 转发桥除 Authorization 请求头外，必须使用已配置 App Password 对请求体生成 HMAC，并放入 `x-ms-signature`。仅带未经验证的 Bot Framework JWT 会被拒绝。转发原生 Teams 回调的部署须由转发桥验证后补充此 HMAC；当前端点尚不支持直接验证 Bot Framework JWT。

迁移时须启用目标 Channel、配置验证凭据，并使本地转发桥对未经修改的原始请求体签名后再公开回调。以前的无签名回调会被明确拒绝，回环地址也不例外。只发送出站消息的 Channel 在不使用入站回调时无须配置回调验证器。
