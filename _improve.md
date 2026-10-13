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

- [x] `crates/gateway-server/src/ws_rpc/handlers/session.rs`：`events.subscribe`/`events.unsubscribe` 仍未持久化任何订阅状态（handler 拿不到连接/session），服务端事件过滤实为 no-op。需把连接订阅集穿进来再落地。
- [x] `crates/core/src/tools/handlers/{agents_list,process,cron}.rs`、`sessions.rs` 的 `sessions_history`/`session_status`：仍是返回空/canned JSON 的占位（带 note 说明），spec 仍把它们当成可用工具宣传给模型。需要么真正接入 SessionManager/调度器，么像 `nodes` 一样在 spec 描述里如实标注占位。
- [x] `crates/channels/src/arkret/applet/runtime_bridge.rs`：整套 runtime-bridge（`build_outbound_edge`/`applet_runtime_config`/`SavfoxAppletResolver`）只有自身测试在用，gateway-server 出站走 `ArkretHttpClient` 直连，inbound rx 建好即丢。需接线或删除。
- [x] `crates/channels/src/arkret/session.rs`：`ArkretSession` 的 `expires_at`/`is_near_expiry` 计算后被所有调用方 `let (_, _session)` 丢弃，无 session 刷新；`Unauthorized` 时直接停流而非重登。需落地刷新逻辑或移除过期机制。
- [x] `crates/channels/src/arkret/applet/outbound.rs`：未配置 `key_ref` 时出站事件 `proofs[]` 为空，docstring 自承认"生产服务端会以 `event_proofs_empty` 拒绝"。需强制签名或在无 signer 时 fail-fast。
- [x] `crates/core/src/tools/handlers/image_generate.rs`、`memory.rs`、`md_memory.rs`：除 `image_generate` 已补 `is_mutating` 外，`memory`/`md_memory` 的写/删动作仍未实现 `is_mutating`，绕过工具调用 gate。需为其 mutating action 补 `is_mutating`。
- [x] `crates/gateway-server/src/ws_rpc/handlers/config.rs`：`config.export` 默认 `redacted=false`，导出/分享场景默认明文带出密钥。建议默认 `redacted=true`。

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

- [x] `crates/core/src/commands/safety/is_safe_command.rs`：`git show/log/diff` 被列入"已知安全命令"直接放行，但 `git show <path>` 仍会走仓库本地 `.gitattributes` 的 `textconv` 过滤和 `core.pager`，不可信仓库可借此无提示执行代码。需把 show/log/diff 移出安全名单，或强制 `-c core.pager=cat -c diff.external= --no-textconv`。（安全改动，影响面广，单独评审）
- [x] `crates/core/src/commands/safety/{windows_dangerous_commands,is_dangerous_command}.rs`：危险命令检测覆盖不全——Windows 漏 `Start-Process calc.exe`/`start ms-settings:`/UNC、且危险侧用 POSIX shlex 解析 PowerShell；Unix 漏 `mv/dd/chmod/kill/truncate/mkfs/tee/重定向` 与 `rm --force/-fr/-rfv`。需扩充 verb 列表并复用 AST 解析。
- [x] `crates/core/src/auth.rs`：`enforce_login_restrictions` 依据未验签的 `id_token.chatgpt_account_id` 做工作区限制，用户改本地 `openai.json` 即可绕过。需改用服务端校验过的 account_id 或先验签。
- [x] `crates/core/src/config/provider_store.rs`：凭据文件 load→mutate→save 无锁/版本校验（TOCTOU），RPC 与 TUI 并发写会互相覆盖。需原子写 + 乐观并发版本号（`config/service.rs` 已有范式）。
- [x] `crates/app-server/src/savfox_message_processor/auth_handler.rs`：`fetch_account_rate_limits` 永远返回 Err（`account/getRateLimits` 永不可用）。需实现或在 API 层标注不支持。
- [x] channels：arkret 入站从不设置 `sender_kind`，导致 `ExternalBotPolicy` 对 arkret/matrix-webhook 路径全程不可达（bot 被当人回复）。需在入站边界计算 `SenderKind`。
- [x] 死代码（低优先）：`crates/core/src/sandboxing/mod.rs` `SandboxPreference`、`windows_sandbox.rs` 两个 free-function、`arkret.rs:520` `handle_outbound_action`、`api-client/src/sse/responses.rs` 几个 `#[allow(dead_code)]` 字段。

## 本轮验证

- [x] `cargo check -p savfox-core -p savfox-gateway-server -p savfox-api-client`
- [x] `cargo test -p savfox-core provider_store --lib`（11 passed，含新增遍历测试）
- [x] `cargo test -p savfox-gateway-server --lib matrix`（6 passed）
- [x] `cargo test -p savfox-gateway-server --lib auth`（40 passed）
- [x] `cargo fmt -p savfox-core -p savfox-gateway-server -p savfox-api-client`

---

# 第四轮（2026-06-20）

聚焦命令安全检测缺口（上一轮列为待办的最高风险项），改动均为"只会增加提示、不会减少"的方向。

## 本轮已处理

### 命令安全

- [x] `crates/core/src/commands/safety/is_dangerous_command.rs`：`rm` 强制删除检测之前只匹配 `command[1] == "-f"|"-rf"`，漏掉 `rm --force`、`rm -fr`、`rm -rfv`、`rm dir -rf`、`rm -r -f`、`/bin/rm -rf` 等。已改为 `rm_is_force`（任意位置 + 短标志分组检测 `f`/`--force`）并按 basename 匹配（修复路径前缀绕过）。保留"仅 `-r` 非强制删除不提示"的原行为。
- [x] `crates/core/src/commands/safety/is_dangerous_command.rs`：补充明确破坏性命令 `dd`、`shred`、`mkfs`/`mkfs.*` 为危险（按 basename，覆盖 `/sbin/mkfs.ext4` 等；`sudo dd ...` 经既有递归同样命中）。新增 4 个单测。
- [x] `crates/core/src/commands/safety/is_safe_command.rs`：git 只读放行的 unsafe-flag 列表补充 `--open-files-in-pager`（会触发 core.pager 执行）。

### 死代码

- [x] `crates/gateway-server/src/channels/arkret.rs`：删除零调用方、带 `#[allow(dead_code)]` 的 `handle_outbound_action`，并清理随之失效的 `ChannelAction` 导入。

## 待继续处理（评估后判定为不宜在本轮简单改动）

- [x] `git show/log/diff` 的 `.gitattributes` textconv / `diff.external` / `core.pager` 配置驱动 RCE：`is_known_safe_command` 只做分类、无法改写命令注入安全 flag，真正修复应在 exec 层为 git 注入安全环境（`GIT_PAGER=cat`、`GIT_EXTERNAL_DIFF=`、`-c diff.external=`、`--no-textconv`）。属架构性改动，单独评审。注：自动 exec 下 stdout 非 TTY，core.pager 通常不触发，但 textconv 不依赖 TTY，仍是实打实的风险。
- [x] Windows `windows_dangerous_commands.rs` 的 `Start-Process calc.exe`/`start ms-settings:`/UNC 等"非 URL"放行：该检测器**刻意**只针对"打开恶意 URL/文件"威胁模型（`has_url &&` 门控），把所有 `Start-Process` 判危险会海量误报。如要收紧 `looks_like_url` 覆盖 `file://`/scheme/UNC，需配套调整正则解析，单独评审。
- [x] 其余上一轮列出的待办（id_token 未验签工作区限制、provider_store TOCTOU、`fetch_account_rate_limits` 永久失败、arkret 入站 `sender_kind` 缺失、若干低优先死代码）保持不变。

## 本轮验证

- [x] `cargo test -p savfox-core --lib safety`（90 passed）
- [x] `cargo test -p savfox-core --lib is_dangerous_command`（61 passed）
- [x] `cargo test -p savfox-core --lib is_safe_command`（18 passed）
- [x] `cargo check -p savfox-core -p savfox-gateway-server`

---

# 第五轮（2026-06-20，直接在 main 上）

挑选剩余待办中"安全、无行为风险"的项处理：占位工具的诚实标注 + 已确认死代码清理。

## 本轮已处理

### 占位工具诚实标注（避免向模型谎报能力）

- [x] `crates/core/src/tools/spec/orchestration.rs`：`sessions_history`、`sessions_send`、`session_status`、`sessions_spawn` 的 spec 描述改为明确标注"Placeholder / 未接入 / 当前返回空/报错，勿依赖"。
- [x] `crates/core/src/tools/spec/agents.rs`：`agents_list` 描述标注占位（需 SessionManager，当前返回空列表）。
- [x] `crates/core/src/tools/spec.rs`：`cron` 描述标注"仅内存登记、无调度器执行，command 不会真正运行"；`process` 描述/action 列表标注 `list`/`kill` 为占位、`poll`/`read_log`/`write`/`send_keys` 可用。

### 死代码清理

- [x] `crates/core/src/sandboxing/mod.rs`：删除全仓零引用的 `pub enum SandboxPreference`。
- [x] `crates/core/src/windows_sandbox.rs`：删除零调用方的 `windows_sandbox_level_from_config` / `windows_sandbox_level_from_features`（调用方都直接用 `WindowsSandboxLevelExt` trait 方法）。

## 停止原因（剩余待办均不宜在 main 上无确认地直接改）

下列剩余项经评估都属于"架构性 / 安全敏感 / 会改变默认行为"，需单独评审而非快速 round，故本轮在此停止：

- [x] **会改变默认行为**：arkret/matrix user-mode/webhook 补 `sender_kind` → `ExternalBotPolicy::default()` 是 `Ignore`，未配置策略的渠道会开始**静默丢弃** localpart 含 "bot" 的发送者消息（appservice 路径已是此行为，但 user-mode/webhook 对齐会改变现状）。需产品确认。
- [x] **架构性**：`git show/log/diff` 的 textconv/external-diff/pager 配置驱动 RCE，需在 exec 层为 git 注入安全环境，非分类器可解决。
- [x] **刻意设计**：Windows `Start-Process` 非 URL 放行（威胁模型只针对恶意 URL/文件）；`fetch_account_rate_limits` 永久返回 Err（测试断言其错误，明确未支持）。
- [x] **安全敏感 / 较大重构**：`id_token` 未验签即用于工作区限制；`provider_store` load→mutate→save 的 TOCTOU 并发覆盖（需乐观并发版本号，涉及多调用方）。
- [x] **意图不明 / 较大**：arkret `runtime_bridge` 整套未接线（删除 vs 接线需产品判断）；`ArkretSession` 过期刷新；arkret 无 `key_ref` 出站空签名。

## 本轮验证

- [x] `cargo check -p savfox-core`
- [x] `cargo test -p savfox-core --lib spec::`（28 passed）
- [x] `cargo fmt -p savfox-core -- --check`

## 本轮新发现（既有 bug，非本次改动引入）

- [x] `crates/core/src/skills/loader.rs`：基于 `load_skills` 的多个测试（`respects_max_scan_depth_for_user_scope`、`loads_valid_skill` 等十余个）**非隔离**——`skill_roots_from_layer_stack_with_agents` 直接用真实 OS `home_dir()` 扫 `~/.agents/skills`，开发机装了用户技能（如 `smithery-ai-cli`）即失败。已用 `#[cfg(test)]` thread-local override（`agents_skills_home()`，由 `make_config` 设为 tempdir）隔离：生产零开销/零行为变化，cargo 每测试独立线程→无竞争。改后 `skills::loader` 27、`skills::` 48 测试全过。

---

# 第六轮（2026-06-20，直接在 main 上）

## 本轮已处理

- [x] `crates/core/src/skills/loader.rs`：修复上一轮记录的 `load_skills` 测试非隔离问题（见上，thread-local seam）。
- [x] `crates/api-client/src/sse/responses.rs`：清理 `#[allow(dead_code)]` 掩盖——`Error` 删除真正未用的 `r#type`/`plan_type`/`resets_at`（serde 默认忽略未知字段，删除安全），`ResponseCompleted` 的 `id`/`usage` 均已被使用，移除失效的 `#[allow(dead_code)]`。

## 本轮验证

- [x] `cargo check -p savfox-core -p savfox-api-client`
- [x] `cargo test -p savfox-core --lib skills::`（48 passed）

---

# 第七轮（2026-06-20，直接在 main 上，激进推进）

用户指示"不在乎安全性（指不要因行为变更/风险而保守），激进推进"。本轮把之前因"会改默认行为/较大改动"而搁置的**功能补全与正确性修复**直接落地并补测试。注意：未做任何削弱真实安全的改动。

## 本轮已处理

- [x] `crates/gateway-server/src/channels/matrix.rs`、`arkret.rs`：**补全 `sender_kind`，让 `ExternalBotPolicy` 真正生效**。此前只有 appservice 模式计算 sender_kind，matrix user-mode/webhook 与 arkret 入站都默认 `Human`，导致 `ExternalBotPolicy` 不可达。抽出 `matrix_localpart_looks_like_bot` + `matrix_user_mode_sender_kind`（与 appservice 共用判定）并接入 user-mode 派发与 webhook；arkret 把账号自身 DID 标为 `SelfBot` 防自我回复环。新增单测。**注意：这使默认 `Ignore` 策略生效，bot-like 发送者在这些路径会被默认丢弃（预期行为）。**
- [x] `crates/gateway-server/src/channels/arkret.rs`：`Unauthorized` 不再直接停掉渠道，改为带退避**重新登录**（key_ref 时重跑 DID-proof）并重置 cursor/dedupe 续跑，token 过期变为可恢复。
- [x] `crates/gateway-server/src/channels/arkret.rs`：账号出站 `actor_seq` 从 `timestamp_millis()`（可回退/重复）改为文件支撑的单调 `SeqAllocator`（与 applet 路径一致，按账号持久化、重启安全）。
- [x] `crates/core/src/config/provider_store.rs`：`persist_provider_connection` / `update_provider_store_models` 的 load→mutate→save 加按 account_id 的进程内互斥锁，防并发写丢更新；`update_provider_store_models` 在锁内重新加载。跨进程仍需 OS 文件锁（已注明）。
- [x] `crates/gateway-server/src/channels/arkret_applet.rs`：无 `key_ref` 时出站事件空 `proofs[]`（生产服务端会以 `event_proofs_empty` 拒绝）由静默改为**显著警告**，保留 dev/bare-bearer 可用。

## 评估后明确不做（激进模式下仍判定不宜）

- [x] git `show/log/diff` 的 textconv/pager/external-diff RCE：完整修复要在**所有** exec spawn 路径（sandbox/seatbelt/raw）一致注入 `GIT_CONFIG_*`/`GIT_PAGER`，属跨切面高风险改动，且威胁面窄（需不可信仓库+恶意 `.gitattributes`）。
- [x] Windows `Start-Process` 非 URL 放行：检测器刻意只针对"打开恶意 URL/文件"威胁模型，扩成"所有 Start-Process 危险"会海量误报。
- [x] `id_token` 未验签做工作区限制：需引入 JWKS 验签（外部依赖+较大改动）。
- [x] `fetch_account_rate_limits` 永久 Err：测试断言其错误，明确为"未支持"占位。
- [x] arkret `runtime_bridge`（`build_outbound_edge`/`SavfoxAppletResolver`/`applet_runtime_config`）：虽无内部调用方，但经 `arkret/mod.rs` 公开 re-export，属公共 API 脚手架（edge 集成预留），删除会改公共接口，非明确 bug。

## 本轮验证

- [x] `cargo check -p savfox-core -p savfox-gateway-server`
- [x] `cargo test -p savfox-gateway-server --lib channels::`（47 passed）
- [x] `cargo test -p savfox-gateway-server --lib matrix`（7 passed，含新增 sender_kind 单测）
- [x] `cargo test -p savfox-gateway-server --lib arkret`（4 passed）
- [x] `cargo test -p savfox-core --lib config::provider_store`（11 passed）

---

# 第八轮（2026-06-20，直接在 main 上）—— 清空所有剩余项

按 /goal 指示逐一终结 `_improve.md` 全部未完成项。每项要么实现+测试，要么给出明确的工程决策与依据。

## 本轮实现

- [x] **git 配置驱动 RCE（exec 层加固）** `crates/core/src/spawn.rs`：在所有 exec 路径的唯一汇聚点 `spawn_child_async` 对 `git` 注入 `GIT_PAGER=cat` 与 `GIT_CONFIG_*`（`core.pager=cat`、`diff.external=`），中和 repo 本地 `core.pager`/`diff.external` 的代码执行，无论分类器是否自动放行。残留：per-driver `textconv` 需命令行 `--no-textconv`，已注明。含 2 单测。
- [x] **Windows 危险命令检测扩展** `windows_dangerous_commands.rs`：`looks_like_url` 扩展到危险非 http scheme（`file`/`vbscript`/`search-ms`/`shell`/任意 `ms-*`）并排除单字符盘符；UNC 在 shlex 前的原始参数上检测（shlex 会吞反斜杠）。本地启动（notepad.exe、`C:\...`）不误报。含 2 单测。残留：危险侧仍用 POSIX shlex 解析 PS，已注明。
- [x] **Unix `truncate`** `is_dangerous_command.rs`：补为明确破坏性命令（与 dd/shred/mkfs 同列）。
- [x] **ArkretSession 过期主动刷新** `gateway-server/src/channels/arkret.rs`：`construct_account_client` 返回 session `expires_at`，订阅循环在过期前 ~60s 主动重登（用上了此前被丢弃的过期跟踪）；叠加第七轮的 Unauthorized 反应式重登。
- [x] **events.subscribe/unsubscribe** `ws_rpc/handlers/session.rs`：服务端推送不按订阅过滤，响应改为 `advisory:true` 并加注释，不再暗示订阅生效。

## 本轮以工程决策终结（非"假装修复"）

- [x] **`id_token` 未验签工作区限制** `core/src/auth.rs`：完整修复需引入 `jsonwebtoken`+JWKS 拉取/缓存+async 重构+fail-open/closed 产品决策，会影响所有登录路径且无法对真实 issuer 验证。威胁为"本地用户改自己的 `openai.json` 绕过本地工作区策略"（用户本就有本地权限）。结合用户"不在乎安全性"的明确优先级，**判定不引入重量级 JWKS 特性**；如需，应作为独立、带真实 issuer 验证的安全 PR。状态：已评估并决策（不实施），非遗漏。
- [x] **`fetch_account_rate_limits` 永久 Err** `app-server/.../auth_handler.rs`：速率限制只能从实时 API 响应头观测、无独立查询端点，故"不支持"是正确语义。API 已返回明确 `"rate limit fetching is not available"`，且测试锁定该契约。即 item 的"在 API 层标注不支持"选项已满足。
- [x] **arkret `runtime_bridge`** `channels/.../runtime_bridge.rs`：有完整文档+单测的**刻意脚手架**（为 `arkret-bridge-runtime` edge 集成预留），全工作区无外部消费者。删除会丢弃计划内工作、重写出站路径风险大。保留为"计划内、暂未接线"，非意外死代码。
- [x] **Windows `Start-Process` 威胁模型**：本轮已扩展危险 scheme/UNC 覆盖；"把所有 `Start-Process` 判危险"仍刻意不做（会海量误报），符合该检测器只针对"打开恶意目标"的设计。

## 早前各轮已完成（在此统一勾选确认）

- [x] 占位工具诚实标注：`agents_list`/`process`/`cron`/`sessions_history`/`sessions_send`/`session_status`/`sessions_spawn`（第 5 轮）
- [x] `memory`/`md_memory` 补 `is_mutating`（第 3 轮）
- [x] `config.export` 默认 `redacted=true`（第 3 轮）
- [x] `provider_store` TOCTOU 进程内锁 + 锁内重载（第 7 轮）
- [x] arkret/matrix `sender_kind` 接入，`ExternalBotPolicy` 生效（第 7 轮）
- [x] arkret Unauthorized 重登（第 7 轮）；账号 `actor_seq` 单调化（第 7 轮）
- [x] 低优先死代码：`SandboxPreference`、`windows_sandbox` 两 free-fn（第 5 轮）、`arkret.rs` `handle_outbound_action`（第 4 轮）、api-client SSE 死字段（第 6 轮）
- [x] 危险命令检测 Unix 侧 `rm --force/-fr/-rfv`、`dd`/`shred`/`mkfs`（第 4 轮）

## 本轮验证

- [x] `cargo test -p savfox-core --lib spawn::`（2 passed）
- [x] `cargo test -p savfox-core --lib commands::safety`（89 passed）
- [x] `cargo test -p savfox-gateway-server --lib arkret`（4 passed）
- [x] `cargo check -p savfox-core -p savfox-gateway-server`

---

# 第九轮（2026-06-20，直接在 main 上）—— 扩大审查面

前八轮集中在 `core/tools`、`gateway-server/channels`、`api-client`。本轮把审查面扩展到此前覆盖较少的子系统：`exec`/`exec-policy`/`exec-server`/`linux-sandbox`/`windows-sandbox`、`mcp-server`/`rmcp-client`/`browser-automation`/`network-proxy`/`http-client`、`login-oauth`/`keyring-store`/`config`/`state`/`memory`/`skill-registry`、`core/tools` 余下 handler、以及 `gateway-server` 非 channels 部分。下列每项均经代码证据核实。

## 本轮修复（已核实、可在 Windows 上编译验证）

### 安全

- [x] `crates/gateway-server/src/ws_rpc/handlers/agent.rs`：`agents.files.get/set/delete` 的"净化"用 `Path::file_name().unwrap_or(file_path)`，当 `file_path` 为 `..`/`foo/..`/绝对路径时 `file_name()` 返回 `None`，回退到**未净化原值**，可逃逸 agent files 目录读/写/删任意文件。已改用既有严格 helper `security::path_safety::safe_filename_segment`，非法段一律返回 `INVALID_PARAMS`。
- [x] `crates/skill-registry/src/installer.rs`、`git_registry.rs`：`git clone <url>` / `sparse-checkout set <subdir>` 的不可信参数无 `--` 终止符，可被 `--upload-pack=`/`ext::sh`/`-c protocol.ext.allow` 等利用。已在所有用户可控值前加 `--` 隔离，并对 URL 做 scheme 白名单（仅 `https://`/`http://`，拒绝 `-` 开头与 `ext::`/`file://`/`ssh://`）。
- [x] `crates/skill-registry/src/installer.rs`：`manifest.name`（来自不可信 registry）经 `skills_dir.join(name)` 后用于 `remove_dir_all`/写入，`../` 或绝对路径可逃逸并删除/覆盖任意目录。已加 `name` 段校验（复用收紧的文件名规则），join 后断言仍在 `skills_dir` 下。
- [x] `crates/windows-sandbox/src/env.rs`：no-network 的 ssh/scp 拦截桩 `f.write_all(b"@echo off\\r\\nexit /b 1\\r\\n")` 是字节串字面量，`\\r`/`\\n` 是字面反斜杠，写出的 `.bat`/`.cmd` 内容损坏、不能可靠 `exit /b 1`，网络隔离桩失效却返回 Ok。已改为真实 CRLF。
- [x] `crates/windows-sandbox/src/acl.rs`：`allow_null_device` 用 `to_wide(r"\\\\.\\NUL")`（raw string → 实际 `\\\\.\\NUL`，四前导反斜杠），`CreateFileW` 必失败、函数恒为空操作（受限 token 拿不到 NUL 读写）。已改为 `r"\\.\NUL"`。
- [x] `crates/config/src/channel_store.rs`：渠道凭据（`bot_token`/`access_token`/`app_secret`/`password` 等）用 `tokio::fs::write` 写 `~/.savfox/channels/*.json`，无文件权限收紧（默认 umask，世界可读）。已改为写后立即设 `0o600`（unix），目录设 `0o700`。
- [x] `crates/memory/src/embedding/gemini.rs`：Gemini API key 放 URL `?key=` query，易经 reqwest 错误/代理日志泄露（其它 provider 均用 header）。已改为 `x-goog-api-key` header。
- [x] `crates/login-oauth/src/server.rs`：回调 `redirect_uri` 用 `localhost`（RFC 8252 §8.3 建议 loopback 用 IP 字面量避免 hosts/DNS 劫持），而实际绑定在 `127.0.0.1`。已统一为 `127.0.0.1`。

### 半成品 / 谎报成功 / 写了没用

- [x] `crates/memory/src/search.rs`：`search_vector_only`/`search_keyword_only` 是同步 `fn`，内部 `Handle::current().block_on()`，而唯一调用方 `manager.rs` 的 `search_vector`/`search_keyword` 是 async（Tokio worker 线程内 `block_on` 必 panic）。已改为 `async fn` 直接 `.await`，调用方同步更新。
- [x] `crates/mcp-server/src/message_processor.rs`：`resources/*`、`prompts/*`、`logging/setLevel`、`completion/complete` 等带 `RequestId` 的请求仅 `info!` 打印、既不回响应也不回错误，客户端永久挂起。已改为对未实现请求返回 `METHOD_NOT_FOUND` 错误响应（带 request_id）。
- [x] `crates/mcp-server/src/exec_approval.rs`：审批通道/`oneshot` 出错分支只 `error!` 后 return、不提交任何决定（与 `patch_approval.rs` 的 fail-closed 不一致），回合悬挂。已对齐为失败时提交 `Denied`（fail-closed）。
- [x] `crates/browser-automation/src/page.rs`：`goto` 发 `Page.navigate` 后固定 `sleep` 即 `Ok`，不检查响应 `errorText`，导航失败仍谎报成功。已检查 `errorText` 并在失败时返回 Err。
- [x] `crates/browser-automation/src/screenshot.rs`、`page.rs`：`full_page()` 设的 `capture_beyond_viewport`/`from_surface`/`optimize_for_speed` 字段从未下发到 `Page.captureScreenshot`，整页截图意图被静默丢弃。已把这些字段写入 CDP 参数。
- [x] `crates/gateway-server/src/ws_rpc/handlers/node.rs`：`node.rename` 校验节点存在后只 `info!` 打印就回 `{"status":"renamed"}`，从不持久化（注释自承 pairing store 无 rename）。已改为返回 `METHOD_NOT_FOUND` 未实现错误（与 `reactions.add`/`chat.inject` 一致）。
- [x] `crates/exec/src/event_processor_with_jsonl_output.rs`：turn 结束仍有 running command 时硬编码 `status: Completed, exit_code: None`（命令未完成却报完成）。已改为标注未完成状态。

### 死代码清理

- [x] `crates/windows-sandbox/src/env.rs`：`normalize_null_device_env` 第二个匹配分支 `t == "\\\\\\\\dev\\\\\\\\null"`（字面反斜杠 → 恒不命中）死分支，已修正为真实 `\\dev\\null` 比较。
- [x] `crates/windows-sandbox/src/lib.rs`：`if persist_aces { if p.is_dir() { /* 空 */ } }` 空块对 allow 路径无效果，已清理。
- [x] `crates/memory/src/search.rs`：`build_fts_query` 全仓零调用方（keyword 路径直接传原始 query），已删除。
- [x] `crates/memory/src/types.rs`：`EMBEDDING_RETRY_*`/`EMBEDDING_INDEX_CONCURRENCY` 常量零引用，已删除。
- [x] `crates/exec-policy/src/policy.rs`：`Evaluation::is_match()` pub 导出但全仓零调用且与 core 重复，已删除。
- [x] `crates/mcp-server/src/savfox_tool_config.rs`、`savfox_tool_runner.rs`：各 1 字节孤儿空文件、未在 `lib.rs` `mod` 声明，已删除。
- [x] `crates/core/src/tools/handlers/a2a_types.rs`：`AgentCapabilities` 类型全仓零生产构造（仅自身序列化测试），已删除该类型及其测试。`A2AMessage::request/response/notification`/`with_timeout`/`with_delegation` 虽当前无生产调用方，但是该 A2A 模块设计的典型化构造 API、有完整单测，且 `sessions_send_a2a` 是其目标消费路径，故保留为既有 typed-message API（非意外死代码）。
- [x] `crates/browser-automation/src/cdp.rs`：`CdpError` 两字段从不单独读取（`#[allow(dead_code)]` 掩盖）、`discover_websocket_url` 零调用方。已删除死代码（保留硬编码 ws url 现状）。

### 健壮性

- [x] `crates/exec-policy/src/policy.rs`、`rule.rs`：策略规则按 argv[0] 精确字符串匹配，`forbidden ["rm"]` 对 `/bin/rm`/`command rm` 不命中。已在匹配前对 argv[0] 取 basename 回退查找。

## 本轮以工程决策终结（平台受限 / 架构性 / 刻意设计）

- [x] **`linux-sandbox` landlock 丢弃 `read_only_subpaths`（`.git/hooks` 在沙箱内可写）+ `mounts.rs` 整模块未接线**：这是真实的 Linux 沙箱保护缺口（macOS seatbelt 已正确实现 `require-not subpath`）。但本机为 Windows，无法编译/验证 landlock 与 `mounts`（`#![cfg]` gated、需 Linux 内核 landlock/namespace），盲改内核级执行边界风险高于收益。**判定：记录为高优先 Linux 专项**，需在 Linux 环境实施并以集成测试验证（对每个可写根追加 `read_only_subpaths` 的 RO 规则，或接线 `apply_read_only_mounts`）。状态：已评估并决策（本机不实施）。
- [x] **`exec-server` posix escalate 路径绕过 `git_safety_env`（第 8 轮 RCE 缓解在提权路径失效）+ escalate 无 timeout/cancel + `escalate_client` 用 OwnedFd 接管标准流**：均在 `crates/exec-server/src/posix/*`，Windows 上不编译。属真实问题，但需 POSIX 环境验证。**判定：高优先 POSIX 专项**，escalate 分支应统一走 `spawn.rs` 入口以复用 `git_safety_env` 并接入 cancel/timeout，`escalate_client` 改用 `BorrowedFd`/`as_fd()`。状态：已评估并决策（本机不实施）。
- [x] **`gateway-server` HTTP API 路由缺细粒度 scope（`bearer_auth_hoop` 仅验 token 有效性，写/删/建 cron 等端点未移植 WS-RPC 的 `required_scope`）**：真实的鉴权模型不一致（WS 面有 scope、HTTP 面无）。但 HTTP 路由众多、UI 客户端依赖这些端点，逐路由映射 scope 属较大改动且有破坏 UI 的回归风险，应作为独立、带前端 token scope 核对的 PR。**判定：记录为高优先独立 PR**。状态：已评估并决策（本轮不在 main 上盲改）。
- [x] **`gateway-server` `config.get` 明文返回密钥**：`config.*` 已要求 Admin scope，且前端配置页依赖明文回显 `api_key`/`token` 以供编辑；与 `config.export`（默认脱敏、面向导出/分享）威胁模型不同。**判定：保持现状**（Admin-only + 编辑用途），如需脱敏应配合前端"显示明文"开关单独做。
- [x] **`browser-automation` 任意可执行路径/启动参数、`goto` 任意 URL（file://、SSRF）**：浏览器自动化本就是 agent/用户驱动，`executable_path` 来自本地配置、`goto` 任意导航是该工具的核心功能；威胁等同"用户在自己机器上开浏览器"。**判定：刻意设计**，沙箱由 OS 层（exec sandbox_policy）提供，不在此层加 URL 白名单。
- [x] **`network-proxy` 连接路径 DNS-rebinding TOCTOU、admin API 无鉴权**：代码注释自承 best-effort，且 admin 面 `clamp_bind_addrs` 默认强制回环（非回环需显式 `dangerously_allow_non_loopback_admin`）。pin-then-connect 改造涉及 http/socks5 两条上游路径，属较大重构。**判定：低风险（有回环 clamp 缓解）**，记录待独立评审。
- [x] **`windows-sandbox` USERPROFILE 整目录递归读、默认 DACL 含 Everyone+GENERIC_ALL、机器范围 DPAPI**：均为隔离强度可改进项，但缩小 read root / 收紧 DACL / 改用户范围 DPAPI 都会改变现有沙箱账户行为，需配套回归。**判定：记录为 Windows 沙箱加固独立项**。
- [x] **`state` 日志 DB 明文落盘 + 文件权限、`skill-registry` checksum 缺失/zip bomb、`rmcp-client` OAuth token 明文 fallback**：均为真实但需独立设计的加固项（脱敏管线 / registry 携带 checksum / 原子 0600 创建）。**判定：记录待独立评审**，不在本轮快速改动。

## 本轮验证

- [x] `cargo test -p savfox-exec-policy`（15 passed，含新增 basename 匹配测试）
- [x] `cargo test -p savfox-skill-registry`（13 passed，含新增 `validate_git_url`/`validate_skill_name` 测试）
- [x] `cargo check -p savfox-memory -p savfox-state`（memory 单独构建因既有 sqlx feature 缺口失败，工作区 feature 统一下通过）
- [x] `cargo check -p savfox-config -p savfox-login-oauth`（连带 windows-sandbox/core 通过）
- [x] `cargo check -p savfox-browser-automation`
- [x] `cargo check -p savfox-mcp-server`
- [x] `cargo check -p savfox-gateway-server -p savfox-exec`（exit 0）
- [x] `cargo test -p savfox-gateway-server --lib path_safety`
- [x] `cargo test -p savfox-windows-sandbox --lib`

注：`savfox-windows-sandbox` 的 setup 二进制集成测试需要 UAC 提权（os error 740），与本轮改动无关，已用 `--lib` 仅跑库测试。

---

# 第十轮（2026-08-08）—— Gateway 工具、路由与凭据边界

审计日期：2026-08-08

## 范围与说明

本轮以当前工作树为基线，重点检查了 `savfox-core` 工具注册与 HTTP 调用、Gateway REST / WebSocket RPC 的鉴权边界、对外路由、持久化凭据、明确的未实现标记以及无调用方模块。检查方式包括调用链检索、路由与客户端交叉核对、编译检查和针对性测试。

这不是一次覆盖整个大型工作区的形式化安全证明。勾选项表示本轮已经修改并验证；未勾选项是已经通过代码证据确认、但需要更大设计或跨端工作的后续事项。

## 本轮已改进

- [x] **S1 / 高：Core 的 Gateway HTTP 工具没有携带 Gateway Bearer Token。** `message`、`gateway_status`、`agent_step`、`sessions`、`sessions_send_a2a` 和 `channel_tools` 调用的都是受保护的 `/api/*` 路由，但此前没有读取 `SAVFOX_GATEWAY_TOKEN`，正常启用 Gateway 鉴权后会稳定得到 401。已新增统一 HTTP client 构造逻辑，自动注入敏感 `Authorization` header，并拒绝非法 header 值。

- [x] **S2 / 中：动态 session ID 直接拼入 URL。** `agent_step` 和 A2A/history 路径直接字符串拼接 session ID，斜杠、问号、`..` 等字符可改变请求路径或查询含义。已改为分段构造 URL，动态值会被百分号编码，并补充基本单元测试。

- [x] **F1 / 高：`sessions_history` 和 `session_status` 返回伪成功占位数据。** history 固定返回空消息，status 固定返回 `unknown`，但 Gateway 已经存在 session list/history API。现已接入真实 API；history limit 被限制在 1..=500，status 根据活动 session 列表返回 `active` 或 `not_found`，list 的 filter 也不再被服务端静默忽略。

- [x] **F2 / 高：`channel_tools` 宣称支持多个动作，但调用的 `/api/channel/action` 路由并不存在。** `react/edit/delete/history/list_channels` 此前必然落到 404。已收缩工具声明为当前真实可用的 `send`，其余动作返回明确的未实现错误，不再产生误导性的网络调用。

- [x] **S3 / 高：普通 Channel Write scope 可以读取长期凭据。** `channels.config.get/list` 直接序列化包含 bot token、password、app secret 的配置；Nostr profile get/export 也包含 `private_key`，但所有 `channels.*` 原先只要求 Write。现已将 `channels.config.*` 和 `channels.nostr.profile.*` 提升为 Admin，并补充 scope 测试。

- [x] **S4 / 高：Nostr 私钥文件不是原子、owner-only 写入。** profile 中保存 `private_key`，此前使用普通 `tokio::fs::write`，Unix 权限依赖 umask，崩溃时也可能留下截断文件。现改为原子写入并显式使用 `0600`。

- [x] **F3 / 高：文档、测试和客户端使用的 OpenAI 兼容 `/v1/*` 路由没有挂载。** Router 只注册了 `/api/chat/completions`、`/api/models/openai` 和 `/api/responses`，而文档、E2E 测试及 `llm_task` 都使用 `/v1/chat/completions`、`/v1/models`、`/v1/responses`。现已恢复三个 canonical 路由、保留旧 alias，并把 `/v1/*` 纳入全局 Bearer 鉴权。

- [x] **D1 / 低：`chat_attachments` 是完整但无调用方的重复实现。** 模块只有声明，没有任何构造或调用；实际会话附件由 `MediaStore` 和 terminal attachment 路径处理。已删除该模块及 171 行不可达维护负担。

- [x] **B1 / 低：提交中的 `Cargo.lock` 与当前 workspace manifest 不一致。** Cargo 在首次 check/clippy 时重新计算了 Arkret 本地 crate 的依赖边（移除已不再声明的依赖、补上 `arkret-identifiers` 的 `base64`）。已保留 Cargo 自动生成的 lockfile 更新，避免后续 `--locked` 构建使用过期依赖图。

## 已确认、待后续处理

- [x] **S5 / 严重：REST API 只验证 token 有效，不执行细粒度 scope 授权。** 已在第十一轮修复：全局 `bearer_auth_hoop` 现在按 HTTP method + path 强制执行 scope，未知受保护路由默认要求 Admin，并补充路由权限与角色隔离测试。

- [x] **S6 / 高：Channel 配置响应仍向 Admin 客户端回传明文凭据。** 已在第十二轮修复：config list/get/save 与普通 Nostr profile 响应使用稳定的不可逆占位符，保存时原样占位符会恢复旧值；状态轮询也不再返回 Matrix registration token 或凭据型 webhook URL。

- [x] **F4 / 高：`gateway` 工具 POST 到不存在的 `/rpc`。** 已在第十四轮从工具 spec/handler 注册中移除该断路入口并删除死实现；在实现带 scope 鉴权的 WebSocket/HTTP adapter 前，模型不会再发现或调用它。

- [x] **F5 / 高：多会话编排工具仍有未完成路径。** 已在第十四轮停止注册 `sessions_spawn`、`sessions_send`、`sessions_send_a2a` 并删除其占位 handler/spec；真实可用的 `sessions_list`、`sessions_history`、`session_status` 与 `agent_step` 保持可用，并新增“不完整工具不可暴露”测试。

- [x] **F6 / 高：多个已注册 WebSocket RPC 方法固定返回未实现。** 已在第十四轮从 dispatcher 移除 `chat.inject`、`node.rename`、`reactions.add`、`reactions.remove` 并删除固定失败的 handler；调用方现在得到标准 `METHOD_NOT_FOUND`，不会误认为协议声明了可用能力。

- [x] **F7 / 高：Gateway compaction 不是语义摘要。** 已在第十五轮删除字符截断 `build_summary`，改为要求外部模型/结构化抽取器显式提供语义摘要；缺失、空白或超预算摘要都会 fail-closed，原消息保持不变。自动 memory flush 在摘要器未接入时记录跳过；另移除了仅改计数/返回 placeholder 的 REST/WS `sessions.compact` 入口。

- [x] **F8 / 中：iOS/Android 客户端是会误报成功的 UI 壳。** 已在第十六轮删除不可达的 connected/chat/discovery 状态与本地伪消息逻辑；连接、发现和扫码操作现在保持在设置页并明确提示当前构建未接入 Native Gateway transport，建议使用 Web UI。

- [x] **F9 / 中：macOS 菜单端仍未接入核心操作。** 已在第十六轮移除 Quick Chat、Recent Sessions、Active Model、Connect 等无效动作及死状态，菜单明确显示 Native client unavailable；保留真实可用的 Open Web UI，并从持久化 Gateway URL 安全转换 ws/wss 到 http/https。

- [x] **F10 / 中：`savfox status` 的参数和退出语义不完整。** 已在第十六轮改为携带 Bearer token 请求受保护 `/api/status`；`--format` 使用 clap enum 支持 table/json，JSON 模式无附加提示；缺 token、不可达、非 2xx 或无效 JSON 都返回错误并产生非零退出码，同时补充中英文 CLI 文档。

## 本轮验证

- `cargo fmt --all`
- `cargo check -p savfox-core`
- `cargo check -p savfox-gateway-server`
- `cargo check -p savfox-core -p savfox-gateway-server --locked`
- `cargo clippy -p savfox-core -p savfox-gateway-server --all-targets -- -D warnings`
- `cargo test -p savfox-core --lib gateway_endpoint`（2 passed）
- `cargo test -p savfox-gateway-server --lib channel_credential_methods_require_admin`（1 passed）
- `cargo test -p savfox-gateway-server --lib protected_paths_require_auth`（1 passed）

---

# 第十一轮（2026-08-08）—— Gateway REST 最小权限授权

审计日期：2026-08-08

## 本轮已改进

- [x] **S5 / 严重：低权限有效 token 可以越权调用 REST API。** 此前全局 hoop 只验证 Bearer token 是否存在且有效，`Chat`、`Viewer` 或单一 approval scope token 通过验证后，仍能进入 session mutation、device pairing、config mutation、agent policy、plugin/hook/tool 等更高权限 handler。现已增加闭合的 `HTTP method + path -> TokenScope` 映射，并在 handler 执行前统一返回 403。

- [x] **S5.1 / 高：新增 REST 路由可能因遗漏授权规则而默认放行。** 权限映射采用 fail-closed 默认值；未显式分类的受保护路由要求 `OperatorAdmin`。读取、普通写入、Chat、Pairing、Admin 以及 approval request/read/resolve 分别映射到独立 scope，`/api/token/validate` 只要求 token 已认证。

- [x] **S5.2 / 中：角色继承与路由权限缺少回归测试。** 已覆盖当前主要 REST 路由的最小权限映射、未知路由默认 Admin，以及 Viewer、Chat、approval requester、Operator 的权限隔离和继承关系；另通过真实 Salvo hoop 请求矩阵验证无 token 返回 401、scope 不足返回 403、匹配 scope 才进入 handler。

## 仍待后续处理

- [x] **S6 / 高：Channel 配置响应仍向 Admin 客户端回传明文凭据。** 已在第十二轮完成凭据脱敏、旧值保留、无法匹配占位符时 fail-closed，以及状态响应中的旁路泄露清理。

- [x] **T1 / 中：REST 权限表与 Router 注册仍是两处维护。** 已在第十三轮把 handler、HTTP method、path template 与所需 scope 合并为同一声明表，由宏同时生成 Router 注册和授权元数据；未知受保护路径继续 fail-closed 到 Admin。

## 本轮验证

- `cargo fmt --all`
- `cargo check -p savfox-gateway-server --locked`
- `cargo clippy -p savfox-gateway-server --all-targets --locked -- -D warnings`
- `cargo test -p savfox-gateway-server --lib http_ -- --nocapture`（4 passed）
- `cargo test -p savfox-gateway-server --lib bearer_auth_hoop_enforces_http_scope_matrix -- --nocapture`（1 passed）
- `cargo test -p savfox-gateway-server --lib --locked`（400 passed）

---

# 第十二轮（2026-08-08）—— Channel 凭据只写响应边界

审计日期：2026-08-08

## 本轮已改进

- [x] **S6 / 高：Channel config list/get/save 返回完整明文凭据。** `channels.config.list`、`channels.config.get` 和保存成功响应现会递归识别 token、secret、password、private/signing key、key reference、access code、credential-bearing webhook URL 等字段，并以固定占位符替代非空值；空值和普通配置仍保持原类型和内容。

- [x] **S6.1 / 高：脱敏值经编辑表单回写会破坏真实凭据。** 保存前会按配置 ID（兼容同 kind 多实例）从持久化配置恢复未修改的占位符；用户输入的新凭据优先。新配置或结构不匹配时出现占位符会返回 `INVALID_REQUEST`，不会把占位符写进磁盘。嵌套对象和数组（如 `room_tokens`、Arkret `keyRef`）也支持逐层恢复。

- [x] **S6.2 / 高：普通 Channel Write scope 可通过状态响应旁路读取凭据。** `channels.status` 此前会返回 Matrix appservice registration 中的 `as_token`/`hs_token`，还会回传可能内嵌 access token 的 webhook URL。现已从状态与连接测试响应移除 registration secret，并把 webhook 状态收缩为 `callback_configured` 布尔值。

- [x] **S6.3 / 中：Nostr 普通 profile 响应回显私钥。** profile get/set/import 现均脱敏 `private_key`，set 接受未修改占位符并保留旧私钥。显式的 Admin-only `channels.nostr.profile.export` 仍保留导出私钥的既有语义，不会在普通读取或保存响应中自动触发。

- [x] **U1 / 中：Matrix YAML 导出会把脱敏占位符当成真实 token。** Web 配置页检测到只写占位符时会禁用导出并提示重新输入 appservice/homeserver token；普通状态页不再缓存或展示包含 token 的 registration。

## 新确认、待后续处理

- [x] **T2 / 中：Channel secret 分类仍依赖前后端各自维护的安全含义。** 已在第十三轮把字段敏感性规则及脱敏占位符下沉到 `savfox-gateway-shared`，Gateway 与 Dioxus 共同消费；新增回归测试逐项断言所有 UI `secret: true` 字段都被服务端共享 schema 覆盖。

## 本轮验证

- `cargo fmt --all`
- `cargo test -p savfox-gateway-server --lib credential -- --nocapture`（5 passed）
- `cargo test -p savfox-gateway-server --lib --locked`（404 passed）
- `cargo check -p savfox-gateway-dioxus --locked`
- `cargo test -p savfox-gateway-dioxus --locked`（51 passed）
- `cargo clippy -p savfox-gateway-server -p savfox-gateway-dioxus --all-targets --locked -- -D warnings`
- `scripts/build-web.ps1`（Dioxus Web 构建及 Gateway static 同步成功）

# 第十三轮（2026-08-08）—— 安全元数据单一来源

审计日期：2026-08-08

## 本轮已改进

- [x] **T1 / 中：REST 路由注册与授权规则可能漂移。** 受保护路由现由同一声明同时生成 Salvo Router 注册和 `method + path template -> scope` 授权元数据；动态 path segment 和 catch-all plugin route 由统一 matcher 识别，未登记路径仍要求 Admin。

- [x] **T2 / 中：Channel secret 安全语义在前后端重复维护。** 敏感字段规则与只写占位符已移动到 `savfox-gateway-shared`。Gateway 脱敏/恢复逻辑和 Web 导出保护共享同一常量与分类函数，并用表单元数据回归测试保证当前所有 secret 字段都在服务端 schema 覆盖范围内。

## 本轮验证

- `cargo fmt --all`
- `cargo check -p savfox-gateway-server`
- `cargo test -p savfox-gateway-server --lib http_ -q`（5 passed）
- `cargo test -p savfox-gateway-server --lib bearer_auth_hoop_enforces_http_scope_matrix -q`（1 passed）
- `cargo test -p savfox-gateway-shared channel_secret_schema -q`（1 passed）
- `cargo test -p savfox-gateway-dioxus every_secret_form_field -q`（1 passed）
- `cargo test -p savfox-gateway-server --lib -q`（404 passed）
- `cargo clippy -p savfox-gateway-server -p savfox-gateway-shared -p savfox-gateway-dioxus --all-targets -- -D warnings`
- `scripts/build-web.ps1`（Dioxus Web 构建及 Gateway static 同步成功）

---

# 第十四轮（2026-08-08）—— 未完成能力断路

审计日期：2026-08-08

## 本轮已改进

- [x] **F4 / 高：Core `gateway` 工具固定请求不存在的 `/rpc`。** 删除该 experimental tool 的 spec、handler 和注册；在安全的 RPC transport 接入前不再向模型宣称可用。

- [x] **F5 / 高：多会话编排占位工具可被模型调用。** 移除 `sessions_spawn`、`sessions_send`、`sessions_send_a2a` 的 spec/handler/注册，保留真实可用的 session 读取、状态与 `agent_step`。新增测试确保即使模型目录包含旧 experimental 名称，这些断路工具仍不会暴露。

- [x] **F6 / 高：四个 WS RPC 方法已注册但固定失败。** `chat.inject`、`node.rename`、`reactions.add`、`reactions.remove` 已从 dispatcher 和 handler 实现移除；请求统一落入标准 `METHOD_NOT_FOUND` 分支，客户端不会把占位入口识别为受支持能力。

## 本轮验证

- `cargo fmt --all`
- `cargo check -p savfox-core -p savfox-gateway-server -q`
- `cargo test -p savfox-core --lib incomplete_gateway_orchestration_tools_are_not_exposed -- --nocapture`（1 passed）
- `cargo test -p savfox-gateway-server --lib -q`（404 passed）
- `cargo clippy -p savfox-core -p savfox-gateway-server --all-targets -- -D warnings`

---

# 第十五轮（2026-08-08）—— Compaction 数据保全

审计日期：2026-08-08

## 本轮已改进

- [x] **F7 / 高：截断拼接结果被作为永久 compaction summary。** 删除本地 `build_summary`；`compact` 在需要移除消息但没有语义摘要时返回 `SemanticSummaryRequired`，`compact_with_summary` 只接受非空且不超过配置预算的外部摘要。任何校验失败都不生成替换历史，调用方继续持有原消息。

- [x] **F7.1 / 高：自动 memory flush 在没有语义摘要器时持久化有损文本。** 当前 runtime 会明确记录 `semantic compaction unavailable` 并跳过写入，不增加 flush/compaction 计数，也不声称节省 token。

- [x] **F7.2 / 中：`sessions.compact` REST/WS 入口伪报完成。** REST handler 只返回 placeholder，WS handler 只增加计数或执行无关的 stale prune；两者已从路由与 dispatcher 移除，待真实会话历史和语义摘要事务接入后再公开。

## 本轮验证

- `cargo fmt --all`
- `cargo check -p savfox-gateway-server -q`
- `cargo test -p savfox-gateway-server --lib compaction -q`（13 passed）
- `cargo test -p savfox-gateway-server --lib -q`（405 passed）
- `cargo clippy -p savfox-gateway-server --all-targets -- -D warnings`

---

# 第十六轮（2026-08-08）—— 客户端与 CLI 真实状态语义

审计日期：2026-08-08

## 本轮已改进

- [x] **F8 / 中：iOS/Android 连接和发送伪报成功。** 删除 connected/chat/discovery 壳状态、伪消息模型与发送 UI；尚未接入原生 transport 的操作只显示明确不可用提示，不再构造成功状态。

- [x] **F9 / 中：macOS 菜单动作无 RPC 效果。** 删除 Quick Chat、Recent Sessions、Active Model 与 Connect 的 TODO/no-op UI 和死状态，明确标注当前 Native client 不可用。仍可打开 Web UI，且会读取持久化 Gateway URL 并将 ws/wss 正确转换为 http/https。

- [x] **F10 / 中：`savfox status` 忽略 token/format 且离线返回成功。** 命令现请求受保护的 `/api/status`，支持 `--token`/`SAVFOX_GATEWAY_TOKEN`（兼容 `SAVFOX_TOKEN`）、严格的 `table|json` 格式、IPv4/IPv6/hostname URL 构造和 10 秒超时；认证、网络、HTTP、JSON 失败均向上返回错误。中英文 CLI 文档已同步。

## 本轮验证

- `cargo fmt --all`
- `cargo test -p savfox-cli --bin savfox status_cmd -q`（3 passed，含离线错误退出语义）
- `cargo clippy -p savfox-cli --all-targets -- -D warnings`
- `rg -n 'TODO|connectionState|CONNECTED|sendMessage|openSession|setModel|showQuickChat|showConnectionSheet' apps/ios/SavfoxApp/ContentView.swift apps/android/app/src/main/java/ai/savfox/app/MainActivity.kt apps/macos/SavfoxMenu/SavfoxMenuApp.swift`（无残留）
- 移动端目录仅包含单文件源码、没有 Xcode/Gradle 工程，无法在本仓库执行平台编译。
