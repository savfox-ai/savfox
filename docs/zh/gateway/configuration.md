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

## 系统配置与管理入口

`/config` 默认展示配置概览，分为网关与安全、执行与工具、会话与上下文、运行环境及
高级配置。搜索覆盖字段名、中文标签和说明。Schema 由运行时的 `ConfigToml` 和
`GatewayConfig` 类型生成。无效旧字段不再显示，保存时直接删除，不向其他配置位置迁移。

模型与账号、智能体、渠道、技能和定时任务使用各自管理页及存储。渠道凭据保存在
渠道页和 `channels/*.json`；不再读取 `[gateway.channels]`、`[gateway.bridges]`。
语音合成、对话与唤醒统一到 `/speech`；节点、在线连接与配对统一到 `/devices`。
`/settings` 只管理当前浏览器的个人偏好，服务器日志设置移到日志页。
高级配置提供诊断与手动 RPC 入口，不再占用主导航。

```toml
[gateway]
host = "127.0.0.1"
port = 18881
trust_x_forwarded_for = false
# token 未设置时复用或生成
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"

[gateway.rate_limit]
max_requests = 100
window_secs = 60
max_connections_per_ip = 10

[gateway.response_footer]
enabled = true
max_length = 240
```

显式 CLI 参数优先于文件；未提供参数时读取文件，再使用默认值。TLS 证书与私钥必须
一起设置，缺少其中之一会拒绝保存。

## 环境变量

如果配置文件中没有对应值，很多渠道也支持从环境变量读取凭据，例如：

- `DISCORD_BOT_TOKEN`
- `TELEGRAM_BOT_TOKEN`
- `SLACK_BOT_TOKEN`
- `MATRIX_HOMESERVER`
- `MATRIX_ACCESS_TOKEN`

## 保存与生效

可选配置明确区分“使用默认值”与显式启用、禁用，不再把尚未配置的字段显示为已禁用。

编辑器只有一份草稿。JSON 修改直接更新草稿，格式错误时禁止保存，不再需要点击
“Update field”。放弃修改恢复加载的文档；重新加载前会确认是否丢弃未保存内容。
高级 JSON 编辑器同时承担导入功能：粘贴完整文档、检查差异、保存一次即可。
从该文档删除字段会同时从配置文件删除字段。

`config.get` 返回 `config`、`revision`、`removed_fields`。`config.set` 接受完整
`config` 和读取时的 `revision`，校验有效字段与类型、拒绝并发覆盖、先备份原文件
到 `config.toml.bak`，再原子替换 `config.toml`。成功返回 `status: saved` 和
`restart_required: true`；没有变化则返回 `unchanged`，不重新写入。
实验功能开关及低频核心选项保留在高级文档，不再自动展开成大量默认控件。

系统配置在重启网关后生效，`config.apply` 与 `config.reload` 不再声称即时生效。
即时操作（例如调整当前进程日志级别）由对应功能 API 负责。

`config.patch` 的参数为 `{ "patch": { ... }, "revision": "..." }`，采用 JSON merge
patch 语义，`null` 删除字段。HTTP `/api/config/patch` 的 `config` 是补丁，
HTTP `/api/config/apply` 的 `config` 是完整文档；两者共用上述保存、备份与校验路径。

导出通过 `config.export` 下载已保存、已脱敏的系统配置，不包含未保存草稿，
也不是账号、渠道、语音、任务、会话及密钥的完整备份。

`system.connections` 只读取在线连接，返回 `session_id`、`name`、`connected_at` 和
`is_current`。管理员使用 `system.disconnect` 与 `{ "session_id": "..." }` 通知并关闭
对应 WebSocket。浏览器收到主动断开通知后停止自动重连，重新加载页面可再次连接。

## 建议

- 长期配置放在 `config.toml`
- 敏感凭据优先放环境变量或安全密钥管理系统
- 若对外暴露服务，务必设置 `token` 或更完整的认证机制
- 需要公网访问时再配置 `TLS`

更细的字段定义请以当前代码和 schema 为准。

## 本地 Claude terminal agent

实验性纯 Rust `claude_stream` 模式使用独立模型/设置、显式版本门槛、按凭证隔离的审批及持久会话恢复。配置、停止、权限边界和旧 one-shot 迁移说明见[本地 Claude Code](claude-code.md)。

### 公共 Webhook 鉴权

所有 `/webhooks/<platform>` 回调必须具有明确启用的已保存 Channel。未配置或已禁用时返回 503。验证凭据不得为空；缺少凭据或签名、签名错误、签名后修改请求体均返回 401，不会启动 Agent。Gateway Bearer Token 不能代替平台回调鉴权。

现有平台验证格式保持兼容，包括 Discord 公钥、Slack 签名密钥、Telegram Secret Token、LINE/WhatsApp 签名密钥、Zalo 的 `x-zalooa-signature` 以及其他集成的验证 Token。飞书 HTTP 回调必须配置 Verification Token；WhatsApp 初始化验证 GET 也必须配置并匹配 Verify Token。轮询、长连接及经过鉴权的 Matrix Appservice 路由继续采用原有传输层鉴权。

本地 IRC、Signal、钉钉及旧版 `/webhooks/matrix` HTTP 转发桥须在各自已保存 Channel 的 `config` 对象内配置独立的 `webhook_secret`，或配置 `IRC_WEBHOOK_SECRET`、`SIGNAL_WEBHOOK_SECRET`、`DINGTALK_WEBHOOK_SECRET`、`MATRIX_WEBHOOK_SECRET` 环境变量，同时明确启用对应 Channel。转发桥必须发送 `x-hub-signature-256: sha256=<HMAC-SHA256(secret, 原始请求体)的十六进制值>`。原生 Matrix Appservice 事务仍走现有 Homeserver Token 路由。

MS Teams HTTP 转发桥除 Authorization 请求头外，必须使用已配置 App Password 对请求体生成 HMAC，并放入 `x-ms-signature`。仅带未经验证的 Bot Framework JWT 会被拒绝。转发原生 Teams 回调的部署须由转发桥验证后补充此 HMAC；当前端点尚不支持直接验证 Bot Framework JWT。

迁移时须启用目标 Channel、配置验证凭据，并使本地转发桥对未经修改的原始请求体签名后再公开回调。以前的无签名回调会被明确拒绝，回环地址也不例外。只发送出站消息的 Channel 在不使用入站回调时无须配置回调验证器。
