# 代码审查 6：重复定义 / 可复用副本审计

> **处理状态（2026-06-03）**：第一部分 4 项（1.1–1.4）已全部修复并验证（`cargo check/test -p savfox-channels`、`cargo check -p savfox-core` 通过，触碰模块测试全绿，21 个 contrix::applet 失败为预先存在的在途 bug，与本次去重无关）。第二部分（2.x）为有意分离，确认保留，无需改动（2.5/2.6 列为可选后续）。详见文末「## 处理记录」。

仓库：`D:\Works\savfox-ai\savfox`
范围：类型/逻辑重复定义、存在可复用相似副本却未使用、copy-paste 逻辑块、重复常量/默认值。
方法：对每组疑似项均 Read 双方源码确认，分类为「完全重复可合并」与「相似但有意分离」。

---

## 一、确认重复 —— 建议合并 / 复用既有实现

### ✅ 1.1 `non_empty()` 在 9 个 channel 配置文件中逐字节重复（已存在公共实现）★ 高优先级 —— 已修复

`crates/channels/src/base.rs:107` 已经导出 **public** 的 `non_empty(map, keys)`，并带有 5 个单元测试（base.rs:245-288）：

```rust
pub fn non_empty(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|v| v.as_str())
            .map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
    })
}
```

然而以下 **9 个文件各自又私有定义了一份完全相同的 `non_empty`**（仅 `Value` 与 `serde_json::Value` 的导入别名不同，函数体逐字节一致）：

- `crates/channels/src/whatsapp/config.rs:6`
- `crates/channels/src/line/config.rs:6`
- `crates/channels/src/slack/config.rs:6`
- `crates/channels/src/telegram/config.rs:6`
- `crates/channels/src/mattermost/config.rs:6`
- `crates/channels/src/discord/config.rs:6`
- `crates/channels/src/googlechat.rs:7`
- `crates/channels/src/wechat.rs:8`
- `crates/channels/src/qq.rs:8`

**建议**：删除 9 处私有副本，统一改为 `use crate::base::non_empty;`（base 中已是 `pub`）。

**收益**：消除约 9×9 ≈ 80 行重复；`non_empty` 的语义（trim + 跳过空串 + 按 key 顺序回退）只在一处维护并已有测试覆盖，避免某处偷偷改动语义导致 channel 间行为漂移。

---

### ✅ 1.2 `whatsapp::verify_webhook_signature` 重新实现了已存在的 `http::verify_webhook_hmac` ★ 高优先级 —— 已修复

`crates/channels/src/http.rs:29` 提供公共的 `verify_webhook_hmac(secret, body, expected_hex)`：HMAC-SHA256 → hex → 去除 `sha256=` 前缀 → 常量时间比较（`subtle::ConstantTimeEq`）。

`crates/channels/src/whatsapp/client.rs:47` 的 `verify_webhook_signature(app_secret, body, signature)` 与之**逻辑完全相同**：同样 HMAC-SHA256 + hex + 去 `sha256=` 前缀 + `ct_eq`。属于把公共 helper 又抄了一遍。

**建议**：`whatsapp::verify_webhook_signature` 直接委托 `crate::http::verify_webhook_hmac(app_secret, body, signature)`（保留薄包装以维持公开 API 名），同时删去 whatsapp/client.rs:64-106 中与 http.rs 测试重复的那组单测，或至少不再重复维护算法本体。

**收益**：安全敏感的签名校验逻辑（常量时间比较）只在一处实现，杜绝两份实现日后发生细微差异（例如其中一处忘记常量时间比较）。

**注意区分**：其他 channel 的签名校验**不是**本项重复，属于有意分离 —
- `line/client.rs:80` 用 **base64** 编码（非 hex）且 trim 签名 —— 算法不同。
- `slack/client.rs:169` 用 `v0:{timestamp}:{body}` basestring 且加 `v0=` 前缀 —— 方案不同。
- `discord` 用 Ed25519，`telegram/client.rs:47` 是纯 token 常量时间比较 —— 均不同。
这些不应误判为重复，但它们都正确复用了 `subtle::ConstantTimeEq`，无问题。

---

### ✅ 1.3 channel 客户端内联重复「warn-on-error」响应检查块（已存在 `warn_on_error` helper）★ 中优先级 —— 已修复（9 处）

`crates/channels/src/http.rs:12` 提供 `warn_on_error(response, context)`，`base.rs` 文档也明确指向它。dingtalk、feishu、mattermost 已正确调用（如 `dingtalk/client.rs:42`）。

但以下位置仍**内联手写**了等价的 `if !response.status().is_success() { let status=...; let body=response.bytes().await...; warn!("...HTTP {status}: {}", String::from_utf8_lossy(&body)); }` 块：

- `crates/channels/src/line/client.rs:32` 与 `line/client.rs:67`（reply / push 两处）
- `crates/channels/src/whatsapp/client.rs:31`
- `crates/channels/src/msteams.rs:21`、`slack/client.rs:52`、`qq.rs:262`、`googlechat.rs:122`、`zalo.rs:28`、`irc.rs:26`、`wechat.rs:235`

已 Read 确认 line/whatsapp 两处与 `http::warn_on_error`（http.rs:12-21）函数体一致（context 字符串不同而已）。

**建议**：将这些内联块替换为 `crate::http::warn_on_error(response, "LINE API error").await;`。注意部分文件（如 dingtalk/feishu/telegram 内若干处）是 `if !status.is_success()` 形态、status 已被提前取出做了别的用途，需逐处判断是否适配；纯 fire-and-forget 的 line/whatsapp/msteams/qq 等可直接替换。

**收益**：每处省 ~7 行；统一日志格式；与 `check_response`（bail 版）配对，错误处理风格一致。

**附带观察**：`base.rs:180` 的 `check_response_status` 与 `http.rs:2` 的 `check_response` 按 `base.rs:177` 注释自承「functionally equivalent」，是为「就近发现性」刻意保留的别名。可考虑让 `base::check_response_status` 直接转调 `http::check_response`（目前是各写一遍函数体，base.rs:184-189 vs http.rs:3-8），减少一份函数体维护。

---

### ✅ 1.4 `slugify_account_id` 测试在 core 中重复（函数本身未重复）★ 低优先级 —— 已修复

函数本身无重复：`crates/core/src/config/provider_store.rs:6` 已 `pub use savfox_utils::string::slugify_account_id;` 复用 `crates/utils/src/string.rs:82` 的实现 —— 这是正确的复用。

但 **4 个单元测试被原样复制**：
- `crates/utils/src/string.rs:126-150`：`slugify_account_id_basic / _same_as_provider / _empty_name / _special_chars`
- `crates/core/src/config/provider_store.rs:471-495`：同名 4 个测试，断言完全相同。

**建议**：删除 `provider_store.rs` 中这 4 个测试（函数已在 utils 处测试覆盖）。core 侧只保留真正属于 provider_store 的测试（如 `account_id_exists_check`，provider_store.rs:498）。

**收益**：去掉一份会随实现变化而需要同步的冗余测试。

---

## 二、相似但有意分离 —— 不应误报（标注备查）

### 2.1 `ReasoningEffort` / `ReasoningEffortPreset` 双定义（已知有意设计）

- `crates/gateway-shared/src/models.rs:12`（`ReasoningEffort`）+ `:74`（`ReasoningEffortPreset`）：精简、wasm 友好，仅 serde derive。
- `crates/protocol/src/openai_models.rs:37` + `:82`：带 `schemars::JsonSchema` / `ts_rs::TS` / `strum` derive。

二者同为 lowercase wire format。gateway-shared 文件头注释（models.rs:5-9）明确「Mirrors `savfox_protocol::openai_models::ReasoningEffort`」，且据项目记忆有 round-trip 漂移测试守护。**属有意为之，不建议合并**（合并会把 schemars/ts-rs 重依赖引入 wasm 构建）。仅提示：`ReasoningEffortPreset` 这一对同样是手工镜像，需与 `ReasoningEffort` 一起在测试中守护漂移。

### 2.2 两个 `SavfoxErrorInfo` 枚举（有意分层，含 `From` 转换）

- `crates/protocol/src/protocol.rs:1079`：核心层，`rename_all = "snake_case"`。
- `crates/app-server-protocol/src/protocol/v1.rs:78`：v1 对外层，`rename_all = "camelCase"`，`httpStatusCode` 显式重命名。

二者变体集合相同，但 **wire 命名风格不同（snake vs camel）**，且 v1 处已有手写 `impl From<CoreSavfoxErrorInfo> for SavfoxErrorInfo`（v1.rs:116-145）做逐变体转换。这是「核心类型 + 对外协议视图」的有意分层。**不建议合并**，但属于「手工同步」热点：每新增一个变体需同时改两处枚举 + `From` 匹配，建议在 `From` impl 上用 `#[deny(...)]`/穷尽匹配（目前已是穷尽 match，编译器会在漏变体时报错，已有一定保护）。

### 2.3 `AvailableModel` vs `ModelInfo`（按任务说明，有意区分）

`gateway_shared::AvailableModel`（models.rs:44，含 api_key/base_url 的「已配置账号实例」）与 `protocol::openai_models::ModelInfo`（openai_models.rs:261，「模型预设/能力规格」）是不同概念，**不重复**。已按背景说明排除。

### 2.4 每 channel 的 `*ChannelConfig` 结构体（字段各异，非重复）

`LineChannelConfig` / `WhatsAppSavedConfig` / `SlackChannelConfig` 等（共 17 个，见 `grep "struct .*Config"` 结果）字段不同，属各 channel 的领域类型，**结构本身非重复**。

但**它们的解析与解析后 resolve 流程存在高度同构的脚手架**，见下条 2.5。

### 2.5 `from_channel_config` + `resolve_*_token/secret` 的同构脚手架（建议性，非严格重复）

line/whatsapp（及其他 channel）的 `resolve_*` 函数共享同一骨架：
`list_channel_configs(savfox_home).await? → .iter().filter(|c| c.enabled).filter_map(T::from_channel_config) [.filter(has_outbound_auth)] .find_map(...)`
（对比 `line/config.rs:58-82` 与 `whatsapp/config.rs:64-108`，骨架一致，仅最终 `find_map` 取的字段不同）。

`list_channel_configs(savfox_home)` 在 channels 下出现 26 次（15 文件）。

**建议（可选，收益中等）**：在 `base.rs` 提供一个泛型 helper，例如
`async fn resolve_enabled<T>(savfox_home, parse: fn(&ChannelConfig)->Option<T>, pick: fn(T)->Option<R>) -> Result<Option<R>>`，
让各 channel 的 `resolve_*` 收敛到「parse + pick」两个闭包。因各 channel 的 `from_channel_config` 字段映射确有差异，无法完全消除，但可消掉 list/filter/find_map 这层重复样板。**优先级低于 1.1/1.2**，可作为后续重构。

### 2.6 contrix `slugify`（私有，语义与 `normalize_slug` 略有差异）

`crates/channels/src/contrix/applet/ghost.rs:136` 的私有 `slugify` 与 `crates/utils/src/string.rs:48` 的 `normalize_slug` 相似但**不等价**：
- contrix 版把**所有**非 ASCII-alphanumeric 字符都视作分隔符；`normalize_slug` 仅把空白与 `- _ : / \ .` 视作分隔符（其它非字母数字被丢弃但不产生分隔）。
- contrix 版仅去除尾部 `-`（`while out.ends_with('-')`），`normalize_slug` 两端都 trim。

二者对边界输入会产生不同结果，**不能直接替换**。属「相似副本但语义有意/事实上不同」，列出供决策：若确认 contrix 只需 normalize_slug 的语义，可复用并删私有版；否则保留并加注释说明差异，避免后人误以为可替换。

---

## 汇总（按优先级）

| 项 | 类别 | 位置 | 处理 | 状态 |
|---|---|---|---|---|
| 1.1 `non_empty` ×9 副本 | 完全重复 | 9 个 channel config + base.rs:107 | 删副本，`use base::non_empty` | ✅ 已修复 |
| 1.2 whatsapp 签名校验 | 完全重复 | whatsapp/client.rs:47 ↔ http.rs:29 | 委托 `http::verify_webhook_hmac` | ✅ 已修复 |
| 1.3 warn-on-error 内联块 | 逻辑重复 | line/whatsapp/msteams/qq/... ↔ http.rs:12 | 调 `http::warn_on_error`（9 处） | ✅ 已修复 |
| 1.4 slugify 测试重复 | 测试重复 | provider_store.rs:471 ↔ string.rs:126 | 删 core 侧 4 个测试 | ✅ 已修复 |
| 2.1 ReasoningEffort(Preset) | 有意分离 | gateway-shared ↔ protocol | 保留，靠漂移测试守护 | ☑ 有意保留 |
| 2.2 SavfoxErrorInfo ×2 | 有意分层 | protocol ↔ app-server-protocol/v1 | 保留，有 From 转换 | ☑ 有意保留 |
| 2.5 resolve_* 脚手架 | 同构样板 | 各 channel config | 可选泛型 helper | ⬜ 可选后续 |
| 2.6 contrix slugify | 语义不同 | ghost.rs:136 vs normalize_slug | 不可直接替换，加注释 | ⬜ 可选后续 |

**最高 ROI**：1.1（删 80+ 行、统一已测试 helper）与 1.2（安全敏感、消除算法二次实现）。

---

## 复验记录

复验方法：对每条逐项 Read 双方实际源码，核对「逻辑等价性 + 文件:行号准确性 + 引用的 pub helper 是否存在且可见且签名兼容」。

### 第一部分（主张合并/复用）—— 全部成立，无误报

- **1.1 `non_empty` ×9 副本 —— 保留（成立）**
  - `base.rs:107` 确为 `pub fn non_empty`，可见，并带 5 个单测（base.rs:244-288）。
  - Grep 确认 9 处私有副本全部存在：whatsapp/config.rs:6、line/config.rs:6、slack/config.rs:6、telegram/config.rs:6、mattermost/config.rs:6、discord/config.rs:6、googlechat.rs:7、wechat.rs:8、qq.rs:8。
  - 抽查 whatsapp/config.rs:6 与 qq.rs:8 的函数体，与 base 版逐字节一致（仅 `Value` vs `serde_json::Value` 导入别名不同）。签名兼容，可直接 `use crate::base::non_empty`。
  - 补注：matrix/config.rs:299 的 `non_empty_trimmed(Option<&str>)` 是不同函数（签名不同），报告未将其列入，正确。

- **1.2 whatsapp 签名校验 —— 保留（成立）**
  - `http::verify_webhook_hmac`（http.rs:29）与 `whatsapp::verify_webhook_signature`（client.rs:47）逻辑等价：均 HMAC-SHA256 → hex → 去 `sha256=` 前缀 → `subtle::ConstantTimeEq`。唯一差异是 strip_prefix 执行顺序（whatsapp 先 strip 后算、http 先算后 strip），结果完全等价，可委托。
  - 「注意区分」核对无误：line/client.rs:88 用 base64 + `signature.trim()`（≠ hex）；slack/client.rs:169 用 `v0:{timestamp}:{body}` basestring + `v0=` 前缀。算法/方案确实不同，属有意分离，不应合并。

- **1.3 warn-on-error 内联块 —— 保留（成立）**
  - `http::warn_on_error`（http.rs:12-21）存在。逐处核对内联块与其函数体一致（仅 context 字符串不同）：line/client.rs:32、line/client.rs:67、whatsapp/client.rs:31、msteams.rs:21、slack/client.rs:52、zalo.rs:28、irc.rs:26。
  - 形态微差备注：slack/client.rs:55-56 先 `let body_str = ...` 再 `warn!("...{body_str}")`，逻辑仍等价，可替换。报告已提示「部分文件 status 被提前取作他用、需逐处判断」，结论稳妥，未一刀切。

- **1.4 slugify 测试重复 —— 保留（成立）**
  - 函数本身确为复用：provider_store.rs:6 是 `pub use savfox_utils::string::slugify_account_id;`。
  - provider_store.rs:470-495 的 4 个测试（_basic/_same_as_provider/_empty_name/_special_chars）与 string.rs:126-150 断言完全相同，确属冗余。删除安全，core 侧 `account_id_exists_check`(:498) 等本地测试应保留。

### 第二部分（有意分离）—— 标注准确，未被误当成"应合并"

- **2.1 `ReasoningEffort`/`ReasoningEffortPreset` 双定义 —— 标注正确（有意设计）**
  - gateway-shared/models.rs:12 仅 serde derive + `rename_all="lowercase"`；protocol/openai_models.rs:37 带 `strum`/`JsonSchema`/`TS`/`EnumIter`。两者同为 lowercase wire format。models.rs:5-9 文件头注释明确 "Mirrors savfox_protocol::openai_models::ReasoningEffort"。合并会把 schemars/ts-rs 重依赖引入 wasm。报告判定"保留、靠漂移测试守护"正确。

- **2.2 `SavfoxErrorInfo` ×2 —— 标注正确（有意分层）**
  - protocol/protocol.rs:1079（核心层）vs app-server-protocol/v1.rs:78（camelCase + JsonSchema/TS + `httpStatusCode` 显式重命名）。v1.rs:116-148 有手写 `impl From<CoreSavfoxErrorInfo>`，穷尽 match（漏变体会编译报错）。判定"保留、不合并"正确。

- **2.3 `AvailableModel` vs `ModelInfo` —— 标注正确（概念不同，不重复）**。

- **2.4 各 channel `*Config` 结构体 —— 标注正确（领域类型，字段各异，非重复）**。

- **2.5 `resolve_*` 脚手架 —— 标注正确（同构样板，建议性可选重构）**。base.rs:122 已有 `resolve_outbound_token<C>` 泛型雏形，进一步收敛 list/filter/find_map 样板的方向合理，优先级低于 1.1/1.2 的定位恰当。

- **2.6 contrix `slugify` —— 标注正确（语义不同，不可直接替换）**
  - ghost.rs:136 把**所有**非 ASCII-alphanumeric 字符当作分隔符并插入 `-`，仅 `while out.ends_with('-')` 去尾部；normalize_slug(string.rs:48) 仅把空白与 `- _ : / \ .` 当分隔符（其它非字母数字被丢弃但**不产生**分隔），且 `trim_matches('-')` 两端 trim。对含中文/emoji/其它符号的输入结果不同（如 `"a啊b"`：contrix→`a-b`，normalize_slug→`ab`）。判定"不可直接替换"成立。

### 复验结论

- 原有条目：第一部分 4 条主张合并/复用 + 第二部分 6 条有意分离，共 10 条。
- 删除/修正：0 条。所有"可合并/复用"判定均经双方源码确认为真重复或真冗余；所有"有意分离"判定均经源码确认语义/边界确不相同，且报告未将任何有意分离项误列为"应合并"。引用的文件:行号全部准确。
- 保留：全部 10 条原样保留。

---

## 处理记录（2026-06-03 修复落地）

第一部分 4 项全部修复，第二部分确认有意保留。

### ✅ 1.1 删除 9 个私有 `non_empty` 副本
9 个文件的私有 `non_empty` 均与 `base.rs:107` 逐字节一致，已删除并改为 `use crate::base::non_empty;`：
- whatsapp/config.rs、line/config.rs、slack/config.rs、telegram/config.rs、mattermost/config.rs：删函数 + 删随之无用的 `use serde_json::Map;`
- discord/config.rs：删函数，**保留** `Map`（`discord_inbound_mode` 仍用）
- googlechat.rs、wechat.rs、qq.rs：删函数 + 相应收敛 `serde_json` 导入

### ✅ 1.2 whatsapp 签名校验委托公共实现
`whatsapp/client.rs:47` `verify_webhook_signature` 改为薄包装 `crate::http::verify_webhook_hmac(...)`，删除手写 HMAC 实现体与内联 `use hmac/sha2/subtle/hex`；删除与 `http.rs` 完全重复的 4 个 HMAC 本体单测（包装层无 whatsapp 独有行为，前缀剥离已在公共实现内）。

### ✅ 1.3 内联 warn-on-error 改调 `http::warn_on_error`（9 处）
均确认为纯 fire-and-forget、response 之后未再使用后替换：line/client.rs（reply+push）、whatsapp/client.rs、msteams.rs、slack/client.rs（仅 `send_message` 那处，业务判断的 warn 未触碰）、qq.rs、googlechat.rs、zalo.rs、irc.rs、wechat.rs。相应清理变为无用的 `use tracing::warn;`（slack 因别处仍用而保留）。

### ✅ 1.4 删除 core 中重复 slugify 测试
`core/src/config/provider_store.rs` 删除与 `utils/src/string.rs:126-150` 逐字节重复的 4 个测试（`_basic/_same_as_provider/_empty_name/_special_chars`），保留 `account_id_exists_check` 等本地测试。

### 验证
- `cargo check -p savfox-channels` ✅ 通过（无警告）
- `cargo test -p savfox-channels`：触碰模块测试全绿；唯余 21 个 `contrix::applet::*` 失败 —— 经独立核实属**预先存在**的在途 contrix bug（namespaces 校验、ghost profile、outbound 事件、proof binding，对应审查报告 #2 的发现），与本次去重**无关**（去重改动在 contrix/applet 零留痕）。
- `cargo check -p savfox-core` ✅ 通过

### 第二部分处理结论
- 2.1 / 2.2 / 2.3 / 2.4：有意分离/分层，确认保留，无需改动。
- 2.5（resolve_* 泛型 helper）、2.6（contrix slugify 加注释）：列为**可选后续重构**，本次未动（2.6 涉及 contrix/applet，当前该模块有在途未通过测试，宜待其稳定后再加注释）。
