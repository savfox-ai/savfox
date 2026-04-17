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
