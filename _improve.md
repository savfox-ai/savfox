# 代码审查与改进记录

日期：2026-06-20

## 本轮已处理

- [x] `crates/core/src/tools/handlers/web_fetch.rs`：`web_fetch` 的 SSRF 校验存在 DNS TOCTOU 风险。代码先用 `tokio::net::lookup_host` 校验目标地址，实际请求时 reqwest 会再次解析；DNS rebinding 可能让校验和连接命中不同地址。已改为每一跳重定向都复检，并用 reqwest `resolve_to_addrs` 固定到已校验地址，同时禁用环境代理参与该 fetch。
- [x] `crates/core/src/tools/handlers/web_fetch.rs`：缓存 key 之前会把完整 URL 转小写，导致大小写敏感的 path/query 发生缓存串用，例如 `/Readme` 和 `/readme`。已改为只 trim，不改写大小写，并补充单测。
- [x] `crates/core/src/tools/handlers/web_fetch.rs`：重定向后返回内容仍用初始 URL 包装，来源标识不准确。已改为用最终 URL 包装返回内容。
- [x] `crates/core/src/tools/handlers/telegram_actions.rs`：Telegram bot token 位于 URL 路径中，reqwest 错误文本可能带出完整 URL。已在请求/读取错误回传中使用 `without_url()`，并对 `getChat` 的 `chat_id` query 参数做 URL 编码。
- [x] `crates/core/src/tools/handlers/telegram_actions.rs`：参数结构里保留了 spec 没暴露、实现也不读取的字段，并用 `allow(dead_code)` 掩盖。已移除死字段和 allow。
- [x] `crates/core/src/tools/handlers/nodes.rs`、`crates/core/src/tools/spec.rs`：`nodes` 工具只是占位，却向模型暴露 `run_command`、`camera_capture`、`get_location`、`send_notification` 等未接入能力，并保留未使用字段。已收窄 tool spec，只声明当前 discovery/status 占位能力，并移除死字段。

## 待继续处理

- [x] `crates/core/src/tools/handlers/discord_actions.rs`、`crates/core/src/tools/handlers/slack_actions.rs`：多个 path/query 参数直接拼进 URL。已对 Discord path segment 和 Slack history query 参数做 percent-encoding，并对列表/history `limit` 做上限 clamp。
- [x] `crates/core/src/tools/handlers/discord_actions.rs`、`crates/core/src/tools/handlers/slack_actions.rs`、`crates/core/src/tools/handlers/whatsapp_actions.rs`：第三方 API 错误响应体会原样回传给模型。已增加共享错误清洗 helper，去掉 reqwest 错误 URL，截断第三方错误体，并对已知 token/API URL 做替换脱敏。
- [x] `crates/core/src/tools/handlers/web_fetch.rs`：仍可进一步限制可接受 content-type。已拒绝明显二进制 media type，仅允许空类型、`text/*`、JSON/XML/JS/SVG/RSS/Atom 等文本型响应。
- [x] `crates/gateway-server/src/ws_rpc/handlers/*.rs`：多个 handler 文件使用 `#![allow(unused_imports)]`。已移除这些 allow，仅保留必要的 `clippy::module_inception` allow，并清理 `model.rs` 中实际未使用的导入。
- [x] 全仓库格式基线：`cargo fmt --all -- --check` 当前会在 `crates/gateway-dioxus`、`crates/gateway-server` 的既有文件上报格式差异。已运行 `cargo fmt --all` 吸收基线格式差异。

## 验证

- [x] `cargo test -p savfox-core web_fetch --lib`
- [x] `cargo check -p savfox-core`
- [x] `cargo fmt -p savfox-core -- --check`
- [x] `cargo check -p savfox-gateway-server`
- [x] `cargo fmt --all`
- [x] `cargo fmt --all -- --check`

---

# 第二轮（2026-06-20）

对 `channels`、`gateway-server` RPC handlers、`core/tools` 其余部分、`gateway-dioxus` 四个子系统做了一次新审查。下面按"已处理/待继续"分组。

## 本轮已处理

### 安全

- [x] `crates/core/src/tools/handlers/image_generate.rs`：图片生成结果直接 `std::fs::write(Path::new(&output_path))` 写盘，未经 turn 沙箱解析，模型可传 `../../` 或绝对路径越权覆盖工作区外文件，且未实现 `is_mutating` 而绕过工具调用 gate。已改为 `turn.resolve_path(...)` 解析路径，并实现 `is_mutating -> true` 接入审批 gate，返回值改用解析后路径。
- [x] `crates/gateway-server/src/security/auth/auth.rs`：`security.rotate`（轮换网关 bearer token + webhook 签名密钥并重写配置）只要求 `Scope::Write`，与其它改密/改配置一律 Admin 的策略不一致，普通 OperatorWrite token 即可轮换网关自身认证。已提升为 `Scope::Admin`。
- [x] `crates/core/src/tools/handlers/sessions.rs`：`sessions_list` 把 `filter` 原样拼进 `?filter=` query，存在 query 注入。已补本地百分号编码 helper（与 `browser.rs` 一致）。

### 半成品 / success-shaped 谎言（向模型/调用方谎报成功）

- [x] `crates/gateway-server/src/ws_rpc/handlers/session.rs`：`chat.inject` 校验后只 `touch()` 时间戳、从不写入任何历史，却回 `{"status":"injected"}`。已改为显式返回 `METHOD_NOT_FOUND` 未实现错误（仍校验 session 存在以给出可读报错）。
- [x] `crates/core/src/tools/handlers/sessions_spawn.rs`：返回伪造的 `agent_id` 和 `"status":"spawned"`，并指示模型用同样未实现的 `sessions_send`/`session_status`，把模型引向无效流程。已改为 `model_err` 显式报"未实现"。
- [x] `crates/core/src/tools/handlers/sessions.rs`：`sessions_send` 谎称 `"Message queued for session ..."` 实则丢弃消息。已改为 `model_err` 显式报"未实现"。
- [x] `crates/gateway-server/src/ws_rpc/handlers/config.rs`：`reactions.add` / `reactions.remove` 从不向任何渠道投递表情却回 `"status":"ok"`。已改为返回 `METHOD_NOT_FOUND` 未实现错误。

### 死代码 / 写了没用

- [x] `crates/gateway-server/src/ws_rpc/handlers/session.rs`：`handle_events_subscribe` 计算 `valid`/`invalid` 后进入空 `if` 块、结果全丢弃。已清理为 `unknown` 列表并在响应中回传（保留向前兼容的"接受未知事件"语义）。
- [x] `crates/gateway-server/src/ws_rpc/handlers/system.rs`：`usage.export` CSV 表头与每行都重复输出 `session_id` 两列。已去掉重复列。

### 前端（gateway-dioxus）

- [x] `crates/gateway-dioxus/src/components/layout.rs`：`more_sheet_link` 接收 `more_open: Signal<bool>` 但从不使用，移动端"More"底部弹层导航后不关闭、盖住整屏。已在 `Link` 上加 `onclick` 关闭弹层。
- [x] `crates/gateway-dioxus/src/pages/settings.rs`：日志的下拉 `<select>` 和"Custom filter"输入框绑定到同一个 `log_level` signal，互相串改导致下拉空值或自定义文本被覆盖。已为自定义输入框拆分独立 `custom_filter` signal。
- [x] `crates/gateway-dioxus/src/pages/settings.rs`：键盘快捷键卡片宣传 `Ctrl+N`（无此绑定）和 `Ctrl+/ Focus chat input`（实际是跳转 Sessions）。已对照 `layout.rs` 实际绑定改为 `Ctrl+/`→Sessions、`Ctrl+,`→Config、`Ctrl+Shift+L`→Logs。

## 待继续处理（需更大改动或属已知占位，本轮未动）

- [ ] `crates/gateway-server/src/ws_rpc/handlers/session.rs`：`events.subscribe`/`events.unsubscribe` 仍未持久化任何订阅状态（handler 拿不到连接/session），服务端事件过滤实为 no-op。需把连接订阅集穿进来再落地。
- [ ] `crates/core/src/tools/handlers/{agents_list,process,cron}.rs`、`sessions.rs` 的 `sessions_history`/`session_status`：仍是返回空/canned JSON 的占位（带 note 说明），spec 仍把它们当成可用工具宣传给模型。需要么真正接入 SessionManager/调度器，么像 `nodes` 一样在 spec 描述里如实标注占位。
- [ ] `crates/channels/src/cokret/applet/runtime_bridge.rs`：整套 runtime-bridge（`build_outbound_edge`/`applet_runtime_config`/`SavfoxAppletResolver`）只有自身测试在用，gateway-server 出站走 `CokretHttpClient` 直连，inbound rx 建好即丢。需接线或删除。
- [ ] `crates/channels/src/cokret/session.rs`：`CokretSession` 的 `expires_at`/`is_near_expiry` 计算后被所有调用方 `let (_, _session)` 丢弃，无 session 刷新；`Unauthorized` 时直接停流而非重登。需落地刷新逻辑或移除过期机制。
- [ ] `crates/channels/src/cokret/applet/outbound.rs`：未配置 `key_ref` 时出站事件 `proofs[]` 为空，docstring 自承认"生产服务端会以 `event_proofs_empty` 拒绝"。需强制签名或在无 signer 时 fail-fast。
- [ ] `crates/core/src/tools/handlers/image_generate.rs`、`memory.rs`、`md_memory.rs`：除 `image_generate` 已补 `is_mutating` 外，`memory`/`md_memory` 的写/删动作仍未实现 `is_mutating`，绕过工具调用 gate。需为其 mutating action 补 `is_mutating`。
- [ ] `crates/gateway-server/src/ws_rpc/handlers/config.rs`：`config.export` 默认 `redacted=false`，导出/分享场景默认明文带出密钥。建议默认 `redacted=true`。

## 本轮验证

- [x] `cargo check -p savfox-core`
- [x] `cargo check -p savfox-gateway-server`
- [x] `cargo check -p savfox-gateway-dioxus`
- [x] `cargo fmt -p savfox-core -p savfox-gateway-server -p savfox-gateway-dioxus -- --check`
- [x] `cargo test -p savfox-gateway-server --lib auth`

---

# 第三轮（2026-06-20）

新审查覆盖 `core` 的 exec/safety/auth/config、`app-server`、`api-client`、以及 channels runtime/matrix。下面是本轮处理结果。

## 本轮已处理

### 安全

- [x] `crates/core/src/config/provider_store.rs`：`provider_id`/`account_id`（部分来自不可信 RPC `models.import`）直接拼成 `{account_id}.json` 文件名，`../../` 或绝对路径可越权读写凭据文件。已加 `sanitize_account_component`（收敛到末段路径分量 + 仅保留文件名安全字符），并补单测 `provider_store_path_cannot_escape_models_dir`。
- [x] `crates/core/src/config/provider_store.rs`：凭据文件用 `std::fs::write` 写盘（默认 0644，世界可读）。已改用 `savfox_utils::fs::write_atomically(.., Some(0o600))`，与 `auth/storage.rs` 一致（原子 + 0600）。
- [x] `crates/gateway-server/src/channels/matrix.rs`：**`allowed_senders` 允许名单只在 user-mode sync 强制**，appservice 模式 `handle_transaction` 与公开 `/webhooks/matrix` 路由完全不校验，任意房间成员都能驱动 agent。已在两条路径补齐过滤（appservice 给 `MatrixAppserviceInner` 加 `allowed_senders` 字段并在派发循环拦截；webhook 用 `resolve_matrix_outbound_config` 取回 config 后过滤）。
- [x] `crates/core/src/auth.rs`：刷新 token 失败时在 `error!`/`warn!` 原样打印响应体。已改为只记录解析后的错误消息 / 错误码，不再 dump 原始 body。
- [x] `crates/api-client/src/endpoint/responses_websocket.rs`：连接成功在 `info!` 打印完整响应 header（可能含 set-cookie 等敏感头）。已改为只记录 header 名称。
- [x] `crates/gateway-server/src/ws_rpc/handlers/config.rs`：`config.export` 默认 `redacted=false`，导出默认带出明文密钥。已改为默认 `redacted=true`（确认前端无调用方依赖旧默认）。

### 半成品 / 写了没用

- [x] `crates/core/src/tools/handlers/memory.rs`、`md_memory.rs`：写/删动作未实现 `is_mutating`，绕过工具调用 gate。已分别为 `add|delete` 和 `create|update|delete|promote` 实现 `is_mutating`（读操作仍为非 mutating）。

## 待继续处理（确认存在但需更大改动 / 单独评审）

- [ ] `crates/core/src/commands/safety/is_safe_command.rs`：`git show/log/diff` 被列入"已知安全命令"直接放行，但 `git show <path>` 仍会走仓库本地 `.gitattributes` 的 `textconv` 过滤和 `core.pager`，不可信仓库可借此无提示执行代码。需把 show/log/diff 移出安全名单，或强制 `-c core.pager=cat -c diff.external= --no-textconv`。（安全改动，影响面广，单独评审）
- [ ] `crates/core/src/commands/safety/{windows_dangerous_commands,is_dangerous_command}.rs`：危险命令检测覆盖不全——Windows 漏 `Start-Process calc.exe`/`start ms-settings:`/UNC、且危险侧用 POSIX shlex 解析 PowerShell；Unix 漏 `mv/dd/chmod/kill/truncate/mkfs/tee/重定向` 与 `rm --force/-fr/-rfv`。需扩充 verb 列表并复用 AST 解析。
- [ ] `crates/core/src/auth.rs`：`enforce_login_restrictions` 依据未验签的 `id_token.chatgpt_account_id` 做工作区限制，用户改本地 `openai.json` 即可绕过。需改用服务端校验过的 account_id 或先验签。
- [ ] `crates/core/src/config/provider_store.rs`：凭据文件 load→mutate→save 无锁/版本校验（TOCTOU），RPC 与 TUI 并发写会互相覆盖。需原子写 + 乐观并发版本号（`config/service.rs` 已有范式）。
- [ ] `crates/app-server/src/savfox_message_processor/auth_handler.rs`：`fetch_account_rate_limits` 永远返回 Err（`account/getRateLimits` 永不可用）。需实现或在 API 层标注不支持。
- [ ] channels：cokret 入站从不设置 `sender_kind`，导致 `ExternalBotPolicy` 对 cokret/matrix-webhook 路径全程不可达（bot 被当人回复）。需在入站边界计算 `SenderKind`。
- [ ] 死代码（低优先）：`crates/core/src/sandboxing/mod.rs` `SandboxPreference`、`windows_sandbox.rs` 两个 free-function、`cokret.rs:520` `handle_outbound_action`、`api-client/src/sse/responses.rs` 几个 `#[allow(dead_code)]` 字段。

## 本轮验证

- [x] `cargo check -p savfox-core -p savfox-gateway-server -p savfox-api-client`
- [x] `cargo test -p savfox-core provider_store --lib`（11 passed，含新增遍历测试）
- [x] `cargo test -p savfox-gateway-server --lib matrix`（6 passed）
- [x] `cargo test -p savfox-gateway-server --lib auth`（40 passed）
- [x] `cargo fmt -p savfox-core -p savfox-gateway-server -p savfox-api-client`
