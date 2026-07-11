# 代码审计报告 2 —— 自相矛盾点（Contradictions）

审计范围：`crates/channels/src/contrix/*`、`crates/gateway-server/*`、`crates/gateway-shared/*`、`crates/core/src/providers/*`、`crates/config/*`。

每条发现均已 Read 矛盾的双方代码确认，附文件:行号与证据。仅收录确凿的矛盾，并标注严重程度。

---

## 1. 【高】Applet DID-proof 登录把"自己的 service_id"当作 audience，与 account 模式、与 session 文档相互矛盾

**位置：**
- `crates/gateway-server/src/channels/contrix_applet.rs:958`（applet 模式）
- `crates/gateway-server/src/channels/contrix.rs:376-386`（account 模式）
- `crates/channels/src/contrix/session.rs:35-39`（语义文档）

**矛盾描述：**

`session.rs` 对 `login_with_signer` 的 `audience` 参数有明确定义：

```rust
// session.rs:37
/// `audience` is the Contrix server's service DID.
```

account 模式严格遵守该语义——audience 取"Contrix 服务器 DID"：

```rust
// contrix.rs:377-386
let audience = account
    .contrix_server_did
    .clone()
    .or_else(|| channel.service_id.clone())   // 服务器的 service_id
    .ok_or_else(|| { ... "no contrix_server_did or channel.service_id for login audience" })?;
```

但 applet 模式却把 **applet 自己的 service_id** 当作 audience：

```rust
// contrix_applet.rs:958
let audience = cfg.service_id.clone();   // 这是 applet 自身身份 DID，不是服务器 DID
```

`cfg.service_id` 在 `config.rs:25` 的文档里写明是"Applet service DID (e.g. `did:web:slack-bridge.example`)"，即 applet 自己的身份，而非它要登录的 Contrix 服务器。把自己的 DID 当作登录受众与文档定义和 account 模式直接冲突，真实服务器很可能拒绝该 DID-proof（aud 不匹配）。

**哪边是错的：** applet 模式（`contrix_applet.rs:958`）。应使用 Contrix 服务器 DID（类似 account 模式从 `contrix_server_url` 派生服务器 DID，或新增一个独立配置字段），而不是 `cfg.service_id`。

**严重程度：高**（key_ref 配置下 applet 出站登录会失败/语义错误）。

---

## 2. 【中】`gateway_shared::ReasoningEffort` 文档声称"与 protocol 版本镜像一致"，但记忆/约定里两者枚举顺序及默认值需一致——此处 `#[default] = Medium`，需与 protocol 端核对

**位置：**
- `crates/gateway-shared/src/models.rs:10-20`

**矛盾描述：** 该枚举文档（`models.rs:7-9`）声明"Mirrors `savfox_protocol::openai_models::ReasoningEffort` so the JSON wire format is identical"。本文件 `#[default]` 标在 `Medium`。这是**潜在**漂移点而非已确证矛盾——本文件本身内部一致（`as_wire_str` 与 serde `rename_all="lowercase"` 一致，round-trip 测试存在）。

**结论：本条仅为提示，非确凿矛盾**，未发现两端 wire 字符串冲突的硬证据，故不计入严重缺陷清单。（保留以提醒后续核对 protocol 端默认值。）

---

## 3. 【中】Auth 文档声称"Viewer 角色拥有 Read + Chat"，但 `Viewer::implies` 实现只给 Read，不给 Chat

**位置：**
- `crates/gateway-server/src/security/auth/auth.rs:524-527`（文档）
- `crates/gateway-server/src/security/auth/auth.rs:56`（实现）

**矛盾描述：**

`has_scope` 自由函数的文档明确写：

```rust
// auth.rs:524-527
/// - **Operator** role has all scopes (via `TokenScope::implies`).
/// - **Viewer** role has `Read` + `Chat`.
/// - **Chat** role has `Chat` only.
```

但 `TokenScope::Viewer` 的 `implies` 实现里 Viewer 只蕴含 `Viewer | OperatorRead`，**完全没有 Chat**：

```rust
// auth.rs:56
Self::Viewer => matches!(other, Self::Viewer | Self::OperatorRead),
```

而且测试 `viewer_implies_read_and_self_only`（auth.rs:577-584）显式断言 `assert!(!v.implies(TokenScope::Chat));`，证明实现行为是"Viewer 不含 Chat"。即**文档与实现 + 测试三方中，文档是错的一方**。

**哪边是错的：** 文档（auth.rs:526）。应改为 "Viewer role has `Read` only"，否则会误导调用方以为 Viewer 令牌能发消息。

**严重程度：中**（误导性文档，可能导致权限设计误判；实现本身是安全的更严格方向）。

---

## 4. 【中】Applet `describe` 对外宣告 `max_events_per_transaction: 100`，但 `transactions` 处理器从不强制该上限

**位置：**
- `crates/gateway-server/src/channels/contrix_applet.rs:292-294`（宣告上限）
- `crates/gateway-server/src/channels/contrix_applet.rs:451-463`（实际处理，无数量校验）

**矛盾描述：**

`applet_describe` 向 Contrix 服务器宣告能力上限：

```rust
// contrix_applet.rs:292-294
limits: json!({
    "max_events_per_transaction": 100,
    "max_body_bytes": 65_536,
}),
```

但 `applet_transactions` 在处理时只对 **body 字节大小** 做了限制（`MAX_APPLET_TRANSACTION_BODY_BYTES = 65_536`，contrix_applet.rs:82/325），对 `body.events` 的**数量没有任何校验**，直接全量遍历分发：

```rust
// contrix_applet.rs:453
for event in body.events.iter() {   // 没有 "events.len() > 100" 检查
    ...
}
```

`max_body_bytes` 与宣告一致（65536），但 `max_events_per_transaction: 100` 这条契约在实现里从未被执行——只要 body ≤ 64KB，事件数可远超 100。这是"宣告的限制"与"实际执行"之间的矛盾。

**建议：** 在分类循环前加入 `if body.events.len() > 100 { 返回 413/400 }`，或下调宣告值与实际一致。

**严重程度：中**（契约不一致；潜在资源滥用面，但有 64KB body 上限兜底）。

---

## 5. 【中】出站 `send_to_contrix_account`：能力授权（grant）的 realm 作用域用 `default_realm_id` 校验，而消息实际发往的是参数 `realm_id`

**位置：**
- `crates/gateway-server/src/channels/contrix.rs:452-456`（grant 校验用 `default_realm_id`）
- `crates/gateway-server/src/channels/contrix.rs:417-447`（消息实际发往参数 `realm_id`）
- `crates/channels/src/contrix/config.rs:104-115`（`select_send_account` 可能选中 default_realm 不匹配的账号）

**矛盾描述：**

消息要发到的目标是函数入参 `realm_id`：

```rust
// contrix.rs:440-443
let request = MessageCreateRequest {
    realm_id: realm_id.to_owned(),   // 真正的目标 realm
    ...
```

但加载/校验能力授权时，`expected_realm` 传的却是账号的 `default_realm_id`，而非这个 `realm_id`：

```rust
// contrix.rs:452-456
let grant = load_and_verify_grant(
    grant_path,
    &account.principal_id,
    account.default_realm_id.as_deref(),   // ← 校验的是 default realm，不是目标 realm
).await ...
```

`select_send_account`（config.rs:105-115）的第二优先级是"任意 `send==true` 的账号"，此时被选中账号的 `default_realm_id` 可能与目标 `realm_id` 不同。于是：grant 通过了对 `default_realm_id` 的作用域校验，却被附加到一条发往不同 `realm_id` 的事件上——授权作用域校验与实际写入目标不一致。

**哪边是错的：** grant 校验应针对实际目标 `realm_id`（即把 `Some(realm_id)` 传入 `load_and_verify_grant`），否则作用域校验形同虚设。

**严重程度：中**（安全/正确性：授权作用域校验绕过实际目标）。

---

## 6. 【中】Ghost profile：模块文档要求 profile "MUST carry … `operator_actor_ids[]`"，但首选的 `build_ghost_profile_event` 未设置该字段，只有已废弃的 `build_ghost_profile` 设置了

**位置：**
- `crates/channels/src/contrix/applet/ghost.rs:12-13`（规范要求）
- `crates/channels/src/contrix/applet/ghost.rs:58-84`（首选实现 `build_ghost_profile_event`，未设 operator_actor_ids）
- `crates/channels/src/contrix/applet/ghost.rs:100-120`（废弃实现 `build_ghost_profile`，设了 operator_actor_ids）

**矛盾描述：**

模块文档（ghost.rs:12-13）明确：

```
//! * Profile MUST carry `actor_kind = "ghost"`, `managed_by_applet`, `external_ref`, and
//!   `accountability { mode, responsible_actor_id, operator_actor_ids[] }`.
```

废弃函数 `build_ghost_profile` 严格满足，含 `operator_actor_ids`：

```rust
// ghost.rs:114-118
"accountability": {
    "mode": "applet_managed",
    "responsible_actor_id": controller_did,
    "operator_actor_ids": [service_id],   // ← 有
},
```

但被推荐使用的 `build_ghost_profile_event`（非废弃，ghost.rs:58）只补了 `mode` 与 `responsible_actor_id`，**没有写 `operator_actor_ids`**：

```rust
// ghost.rs:80-82
event.content["actor_kind"] = json!("ghost");
event.content["accountability"]["mode"] = json!("applet_managed");
event.content["accountability"]["responsible_actor_id"] = json!(controller_did);
// 缺 operator_actor_ids
```

对应测试 `ghost_profile_event_uses_sdk_builder`（ghost.rs:200-225）也只断言 `responsible_actor_id`，从不检查 `operator_actor_ids`，而废弃版的测试（ghost.rs:259-263）专门断言了 operators 数组。也就是说"首选实现"违反了它自己模块文档声明的 MUST 字段，而"废弃实现"反而满足。

**注意：** 无法 100% 排除 SDK 的 `ProfileCreateBuilder::with_ghost_kind` 内部已注入 `operator_actor_ids`（SDK 源不在本仓库）；但从模块文档显式列出该字段、且本函数显式逐字段补写 accountability 子字段却独漏此项、测试也不覆盖来看，与文档要求确属不一致。

**严重程度：中**（规范合规性；接收方若依赖 operator_actor_ids 做问责将拿不到值）。

---

## 7. 【低】Applet grant 校验主体用 `bot_actor_id`，但出站消息的实际 author 是 `ghost_actor_did`

**位置：**
- `crates/gateway-server/src/channels/contrix_applet.rs:1017`（grant 校验 subject = `bot_actor_id`）
- `crates/gateway-server/src/channels/contrix_applet.rs:838-850`（消息 author = `ghost_actor_did`）
- `crates/channels/src/contrix/grant.rs:32-34`（grant subject 文档：必须匹配 writer 的 actor_id）

**矛盾描述：**

`grant.rs` 文档规定 grant 的 subject "must match the writer's `actor_id`"：

```rust
// grant.rs:33-34
/// Grant `subject` field — must match the writer's `actor_id`.
pub subject: String,
```

applet 出站事件的 actor（writer）是 ghost actor：

```rust
// contrix_applet.rs:841,848 (AppletMessageRequest)
ghost_actor_did: ghost_actor_did.to_owned(),  // 事件的 actor_id 即此 ghost DID
```

但校验/加载 grant 时，期望 subject 传的却是 `cfg.bot_actor_id`：

```rust
// contrix_applet.rs:1017
load_and_verify_grant(path, &cfg.bot_actor_id, None)   // ← subject 期望 = bot，而非 ghost writer
```

按 grant 语义，subject 应与写事件的 actor（ghost）一致；这里却拿 bot 去校验。是否真冲突取决于运营方如何签发 grant（若 grant 故意签给 bot 作为代理人，则属另一种模型），但与 `grant.rs` 自身文档"subject = writer's actor_id"存在表述冲突。

**严重程度：低**（取决于授权模型；列为需澄清的不一致）。

---

## 8. 【低】`thread_root_id` 在出站构造与入站解析的对象层级不一致（靠 fallback 兜住，未致错但易踩坑）

**位置：**
- `crates/channels/src/contrix/outbound.rs:57-64` 与 `applet/outbound.rs:95-99`（写入 **外层** content）
- `crates/channels/src/contrix/parse.rs:57-61`（解析时**优先读内层** inner，再 fallback 外层）
- `crates/channels/src/contrix/applet/transaction.rs:108-111`（解析时**只读外层** content）

**矛盾描述：**

两个出站构造器都把 `thread_root_id` 放在外层 content（与 `flow_id`、`content` 同级）：

```rust
// outbound.rs:60-63 / applet/outbound.rs:98
obj.insert("thread_root_id".into(), ... );   // obj = 外层 content
```

但 account 模式解析 `parse.rs` 却**先从内层** `inner`（即 `content.content`）取，再 fallback 到外层：

```rust
// parse.rs:57-61
let thread_root_id = inner.get("thread_root_id")
    .and_then(Value::as_str)
    .or_else(|| content.get("thread_root_id").and_then(Value::as_str))  // fallback 外层
    .map(str::to_owned);
```

而 applet 模式解析 `transaction.rs` 只读外层：

```rust
// transaction.rs:108-111
let thread_root_id = content.get("thread_root_id")...   // 仅外层
```

三处对同一字段的层级假设不统一：写在外层；account 解析"内层优先+外层兜底"；applet 解析"仅外层"。当前因为写在外层、且 parse 有 fallback，功能上不出错；但这是同一概念在不同处的不一致处理，未来若有人按 `parse.rs` 的"内层优先"直觉把字段挪到内层，applet 入站会丢失它。

**严重程度：低**（当前不致错，但属脆弱的不一致约定）。

---

## 9. 【降级·非矛盾】`builtin_model_presets(_auth_mode)` 参数被完全忽略，与"按 auth 模式提供预设"的历史签名意图不符

> 复验降级：本条不构成"自相矛盾"，仅为遗留无效形参 / 签名误导（代码与文档之间无对立断言）。归类为可清理项，保留供参考但不计入矛盾清单。

**位置：**
- `crates/core/src/providers/manager/model_presets.rs:27-29`

**矛盾描述：**

```rust
pub(super) fn builtin_model_presets(_auth_mode: Option<AuthMode>) -> Vec<ModelPreset> {
    PRESETS.iter().cloned().collect()   // 完全忽略 auth_mode
}
```

函数保留 `Option<AuthMode>` 形参却以 `_` 前缀彻底忽略，返回与 auth 无关的全量预设。形参的存在暗示"应按 auth 模式过滤"，但实现不再过滤。这是签名意图与实现行为之间的轻微不一致（重写为 JSON 目录后过滤逻辑被移除却未清理参数）。

**严重程度：低**（无功能 bug，仅遗留无效参数/误导签名；建议删除该参数或恢复过滤）。

---

## 已核查但**未发现**确凿矛盾的点（供参考，避免重复排查）

- `crates/gateway-server/src/security/ssrf.rs:144-148 / 192-200`：`validate_ip` 中对 `169.254.169.254` 的二次显式判断与 `is_private_ip`（已含 `is_link_local`，覆盖 169.254/16）有**冗余**，但二者结论一致（都 block），非矛盾——属可清理的重复，不是逻辑冲突。
- `crates/gateway-server/src/config/validator.rs`：`workspace_auto_create` / `_confirmed` 双布尔的默认值（均默认 `false`）与校验分支一致，无冲突。
- `crates/channels/src/contrix/config.rs:138-143`：`send==true` 必须有 `default_realm_id` 的校验，与 `select_send_account` 的偏好逻辑一致。
- `crates/channels/src/contrix/applet/config.rs`：`rate_limited` 默认 `true`，字段文档"server is permitted to rate-limit"与默认值/registration 透传一致（registration.rs:59）。
- `crates/gateway-shared/src/sessions.rs`：`SessionEntry` 的 `session_id/id`、`message_count/messages` 双字段由 `display_id`/`display_count` 统一收敛，行为一致。
- `crates/gateway-server/src/terminal_agent.rs`：`spawn_count` 在 `record_terminal_runtime_metrics`（退出时调用）里自增，名为 spawn 实则按"运行完成"计数，属命名不够精确，但每次运行恰好 record 一次，计数语义自洽，不计为矛盾。

---

## 汇总

| # | 位置 | 严重 | 类型 |
|---|------|------|------|
| 1 | contrix_applet.rs:958 vs session.rs:37 / contrix.rs:377 | 高 | 登录 audience 语义反了 |
| 3 | auth.rs:526 vs :56 (+test) | 中 | 文档与实现不符（Viewer/Chat）|
| 4 | contrix_applet.rs:293 vs :453 | 中 | 宣告上限未执行 |
| 5 | contrix.rs:454 vs :441 | 中 | grant 作用域校验对错 realm |
| 6 | ghost.rs:13 vs :80-82 | 中 | 首选实现违反文档 MUST 字段 |
| 7 | contrix_applet.rs:1017 vs grant.rs:33 | 低 | grant subject 主体不一致 |
| 8 | outbound vs parse vs transaction | 低 | 同字段层级约定不统一 |
| 9 | model_presets.rs:27 | 降级·非矛盾 | 形参被忽略（非对立断言，清理项）|

最值得优先修的是 **#1（applet 登录 audience）** 与 **#5（grant realm 作用域）**：前者影响 applet 带密钥出站可用性，后者是授权作用域校验的正确性问题。

---

## 复验记录

复核员逐条 Read 矛盾双方源码后的结论（以代码证据为准）。

- **#1 applet 登录 audience —— 保留（高）**。`session.rs:37` 文档明确 audience = "Contrix server's service DID"；account 模式（`contrix.rs:376-386`）正确取 `contrix_server_did` 并 fallback `channel.service_id`，而 `channel.service_id` 经 `config.rs:24-26`/测试值（`did:webvh:contrix.example.org`）确认确为**服务器** DID。applet 模式（`contrix_applet.rs:958`）却用 `cfg.service_id`，经 `applet/config.rs:25-26` 确认是 **applet 自身**身份 DID（`did:web:slack-bridge.example`），且 applet config 无 `contrix_server_did` 字段。矛盾成立。

- **#2 ReasoningEffort 默认值 —— 保留原样（提示项）**。报告自身已声明"非确凿矛盾，仅提示"，未列入缺陷清单，无需改动。

- **#3 Viewer 文档 vs 实现 —— 保留（中）**。`auth.rs:526` 文档"Viewer has Read + Chat"；`auth.rs:56` 实现仅 `Viewer | OperatorRead`；测试 `auth.rs:583` 显式断言 `!v.implies(Chat)`。文档与实现+测试三方对立，文档错。矛盾成立。

- **#4 max_events_per_transaction 宣告 vs 执行 —— 保留（中）**。`contrix_applet.rs:293` 宣告上限 100；Grep 全文件确认只有 `MAX_APPLET_TRANSACTION_BODY_BYTES`（字节）校验，`events.iter()`（:453）前无 `events.len()` 检查。宣告契约未执行，成立（有 64KB body 兜底，报告已注明）。

- **#5 grant realm 作用域 —— 保留（中）**。`load_and_verify_grant` 的 `expected_realm`（`grant.rs:71/140-155`）校验 grant 的 `space_id`；`contrix.rs:455` 传 `account.default_realm_id`，消息却发往参数 `realm_id`（:441）。`select_send_account`（`config.rs:114`）的 fallback 分支 `find(|a| a.send)` 可选中 `default_realm_id != realm_id` 的账号，二者确可不一致。成立。

- **#6 ghost operator_actor_ids —— 保留（中）**。模块文档 `ghost.rs:12-13` 列 `operator_actor_ids[]` 为 MUST；首选 `build_ghost_profile_event`（:80-82）显式逐字段补写 accountability 却独漏此项，废弃版（:117）反而有。报告已诚实声明无法排除 SDK `with_ghost_kind` 内部注入。文档与显式实现不一致，成立（保留其不确定性表述）。

- **#7 grant subject = bot vs writer = ghost —— 保留（低）**。`grant.rs:33` 文档"subject must match writer's actor_id"；出站事件 actor 经 `applet/outbound.rs:82` 确认是 `ghost_actor_did`，签名（`contrix_applet.rs:853`）也用 ghost；但 grant 校验（:1017）传 `cfg.bot_actor_id`。表述层不一致成立，报告已注明取决于授权模型，定级低恰当。

- **#8 thread_root_id 层级不统一 —— 保留（低）**。两出站构造器（`outbound.rs:60-63`、`applet/outbound.rs:98`）写在**外层**；account 解析（`parse.rs:57-61`）"内层优先+外层 fallback"；applet 解析（`transaction.rs:108-111`）仅外层。三处层级假设不一，靠 fallback 当前不致错。脆弱不一致成立，低。

- **#9 builtin_model_presets 形参被忽略 —— 降级（非矛盾）**。`model_presets.rs:27-28` 确实忽略 `_auth_mode`。但这是遗留无效形参/签名误导，代码与文档之间不存在对立断言，**不构成"自相矛盾"**。已在正文与汇总表标注降级，保留供清理参考。

**复验统计：原有 9 条编号发现** → 删除 0 条、降级 1 条（#9）、保留 8 条（其中 #2 为报告自评的提示项、非缺陷）。"已核查未发现矛盾"区（ssrf 冗余等）抽查 `ssrf` 与 `config.rs` 后确认归类正确，无误报混入缺陷清单。
