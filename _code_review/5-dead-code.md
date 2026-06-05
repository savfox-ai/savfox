# 死代码 / 陈旧代码审核报告

审核范围：历史陈旧、已废弃、应当删除的代码（不考虑向后兼容）。
判定原则：**宁可漏报不可误报** —— 每条均用 Grep 在整个 workspace 验证引用情况；序列化字段、trait impl、cfg 平台分支、pub 库 API、测试引用均谨慎排除。

约定的风险等级：
- **低**：纯死代码，删除无任何行为影响。
- **中**：涉及 pub API / 可能有外部或未来用途 / 需删除连带的 re-export 与测试。
- **高**：不建议在本轮删除（属于平台 cfg 分支、序列化目标、运行时仍调用等，仅为澄清避免误删）。

---

## 一、确认可删除（高置信度）

### 1.1 `build_ghost_profile` —— 已标注 `#[deprecated]` 的废弃函数

- **文件:行号**：`crates/channels/src/contrix/applet/ghost.rs:98-120`（函数本体）
- **为何判定废弃**：源码显式标注 `#[deprecated(note = "use build_ghost_profile_event (S-9, returns full Event)")]`，文档注释写明 "**Deprecated**: prefer [`build_ghost_profile_event`]"。它只返回 `content` JSON，新函数返回完整 `Event` Envelope。
- **验证依据**（`Grep "build_ghost_profile\b"` 全仓库结果）：
  - `ghost.rs:100` 定义
  - `ghost.rs:244` 仅在本文件 `#[cfg(test)]` 内、且测试自身打了 `#[allow(deprecated)]`（`ghost.rs:242`）调用
  - `crates/channels/src/contrix/applet/mod.rs:36` `#[allow(deprecated)] pub use ghost::build_ghost_profile;`
  - `crates/channels/src/contrix/mod.rs:38` `#[allow(deprecated)] pub use applet::build_ghost_profile;`
  - **无任何业务代码调用**，全部引用都是 re-export + 它自己的测试。
- **删除影响评估**：删除函数本体 + 两处 `#[allow(deprecated)] pub use`（ghost mod.rs:35-36、contrix mod.rs:37-38 各一行 allow 一行 use）+ 对应测试 `ghost_profile_marks_actor_kind_and_accountability`（ghost.rs:241-约260）。`build_ghost_profile_event`、`build_external_ref` 等新 API 不受影响。
- **风险等级**：**中**（属 channels 库 crate 的 `pub` API，理论上可被外部 crate 引用；但已 deprecated 且任务声明"不考虑向后兼容"，本仓库内部零调用，可安全删除）。

### 1.2 `handle_outbound_action` —— 被 `#[allow(dead_code)]` 掩盖的未接入函数

- **文件:行号**：`crates/gateway-server/src/channels/contrix.rs:513-524`
- **为何判定废弃**：标 `#[allow(dead_code)]`，注释 "Convenience wrapper for the channel registry's `ChannelAction::SendToThread`"。是一个从未接线的便利包装。
- **验证依据**（`Grep "handle_outbound_action"` 全仓库）：唯一命中就是 `contrix.rs:515` 的定义本身，**无任何调用点**。
- **删除影响评估**：删除整段 `pub(crate) async fn handle_outbound_action`（约 12 行）。内部直接调用 `send_to_contrix_account`，删后该函数仍被其它路径正常使用。无连带影响。
- **风险等级**：**低**。

### 1.3 rollout `collect_*` 死代码簇 —— sqlite 迁移残留

- **文件:行号**：
  - `crates/core/src/rollout/list.rs:418-440` `collect_dirs_desc`
  - `crates/core/src/rollout/list.rs:443-463` `collect_files`
  - `crates/core/src/rollout/list.rs:503-518` `collect_rollout_day_files`
- **为何判定废弃**：三者均 `#[allow(dead_code)]`。结合 `crates/core/src/rollout/recorder.rs:127,180` 的 `// TODO(jif): drop after sqlite migration phase 1`，这是旧的"按天目录扫描"路径，已被扁平文件扫描 `collect_flat_rollout_files`（list.rs:465，无 dead_code 标注、正常使用）取代。
- **验证依据**（`Grep "collect_dirs_desc|collect_rollout_day_files|collect_files\b"`）：
  - `collect_dirs_desc`：仅定义处一处命中，**零调用**。
  - `collect_rollout_day_files`：仅定义处一处命中，**零调用**。
  - `collect_files`：仅被 `collect_rollout_day_files`（list.rs:507）调用，而后者本身是死代码 —— 因此整簇互相引用但无外部入口，构成**死代码闭环**。
- **删除影响评估**：可整簇删除三个函数（约 80 行）。`collect_flat_rollout_files` 是活跃路径，不受影响。删除后需确认无遗留 `use Reverse` 等仅被它们使用的导入（删除时一并清理）。
- **风险等级**：**低**。

---

## 二、显式 deprecated 但仍在使用 / 需保留（澄清，勿删）

### 2.1 模型迁移提示配置常量 —— 仍被 TUI 使用，**勿删**

- **文件:行号**：`crates/core/src/providers/manager/model_presets.rs:9-19`
  - 注释 "The legacy migration prompt config keys below are kept for backward compatibility"
  - `HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG`、`HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG`
- **验证依据**（`Grep`）：被 `crates/tui/src/app/mod.rs:410,3190` 与 `crates/tui/src/app/model_migration.rs:10,58,62` 实际引用。
- **结论**：虽注释含 "legacy"，但是**活跃功能**（模型升级提示），不可删除。风险等级 **高**（误删会破坏 TUI 模型迁移提示）。

### 2.2 `migrate_channel_configs` —— 启动时仍调用，**勿删**

- **文件:行号**：`crates/config/src/channel_store.rs:368`（注意：该函数实际在 `crates/core` 通过 re-export 暴露，被网关调用）
- **验证依据**：`crates/gateway-server/src/lib.rs:299` 在启动流程中 `savfox_core::config::channel_store::migrate_channel_configs(&config.savfox_home).await;` 主动调用，用于回填 `slug` 字段。
- **结论**：活跃的一次性数据迁移，保留。风险等级 **高**。

---

## 三、`#[allow(dead_code)]` 经核查为"非死代码"（误报排除，记录以备查）

以下项虽带 `#[allow(dead_code)]`，但经 Grep 验证**不应删除**，列出以避免后续误判：

| 文件:行号 | 符号 | 实际用途 | 风险 |
|---|---|---|---|
| `crates/api-client/src/sse/responses.rs:90,100` | `Error`、`ResponseCompleted` | serde **反序列化目标**，字段被 serde 读取（allow 仅压制"字段未读"警告） | 高（勿删） |
| `crates/gateway-dioxus/src/pages/overview.rs:14,21` | `HealthResponse`、`SnapshotData` | 反序列化目标，`overview.rs:185,196` 实际 `fetch_json`/`ws.call` 使用 | 高（勿删） |
| `crates/core/src/session_manager.rs:60,290` | `ops_log` / `captured_ops` | `#[cfg(any(test, feature="test-support"))]` 测试支撑，测试中使用 | 高（勿删） |
| `crates/gateway-server/src/runtime/agent_routing.rs:27` | `is_dm` 字段 | 路由结构序列化字段，可能由配置反序列化 | 高（谨慎） |
| `crates/gateway-server/src/agent_terminal_launcher.rs:377,440,479` | `build_posix_command_line`、`which_exists`、`file_exists` | **平台 cfg 分支**：在 `#[cfg(unix)]` 路径下被 launcher.rs:162,226,270,289,460,470 调用；dead_code 仅因当前编译目标（Windows）未走该分支 | 高（勿删，跨平台必需） |
| `crates/core/src/tools/sandboxing.rs:235,237` | —— | 注释 "Will be used by later tools" | 中（占位，暂留） |

---

## 四、占位 / 空实现（未完成，非历史残留，单独评估）

以下为"尚未实现"的占位/stub，**不是历史废弃代码**，删除会丢功能骨架，建议保留或单独立项，**不在本轮删除范围**：

- `crates/core/src/tools/handlers/agents_list.rs`、`nodes.rs`、`telegram_actions.rs`：三个实验性工具 handler，`#[allow(dead_code)]` 的是 Args 结构（仍经 `parse_arguments` 校验，字段未读）。Handler 本身**已在 `spec.rs:1085,1105,1106` 注册为 experimental 工具**，返回 placeholder。属未完成而非废弃。风险 **中**。
- `crates/gateway-server/src/voice/tts_edge.rs:243+`、`voice/stt.rs:255`、`voice/voice_wake/*`：标注 stub / "not yet implemented / wired"。属未完成功能。风险 **中**。
- `crates/gateway-server/src/otel.rs:224+`：OpenTelemetry 导出 stub。属未完成功能。风险 **中**。
- `crates/gateway-server/src/runtime/routing/{usage,tts,sessions}.rs`：返回 "placeholder" 消息的占位 RPC。属未完成功能。风险 **中**。

---

## 五、被 crate 级 `#![allow(...)]` 掩盖的大面积死代码（需独立 PR）

### 5.1 gateway-server 全局 `#![allow(unreachable_pub, dead_code)]`

- **文件:行号**：`crates/gateway-server/src/lib.rs:1-14`
- **现状**：crate 顶部注释（中文）记录了已删除约 5000 行的清理历史（r6~r9），并明确写道：
  > 剩余 ~79 处 dead_code 警告分布在 OTel-config / WebChat / canvas-host / 部分 RPC handler 占位字段，需独立 PR 逐位评估删除还是接入。
  > `#![allow(unreachable_pub, dead_code)]`
- **影响**：这一行 crate 级 allow **掩盖了整个 gateway-server crate 约 79 处死代码**，使 Grep 无法靠编译器精确定位。建议：临时移除该 allow，跑 `cargo build -p savfox-gateway-server`（或对应 crate 名）收集完整 dead_code 列表，再逐项评估。**本轮静态审核无法穷尽**。
- **风险等级**：**中**（清理工作量大，需独立 PR；移除 allow 后 workspace 全局 `deny` 会导致编译失败，必须配套逐项删除）。

### 5.2 ws_rpc handlers 的 `#![allow(unused_imports)]`

- **文件:行号**（11 处模块顶部）：`crates/gateway-server/src/ws_rpc/handlers/{system,skill,session,node,model,cron,config_core,config,channel,browser,agent}.rs:1`
  - 另有 `crates/gateway-server/src/web/mod.rs:7`、`agent_terminal_launcher.rs:12,17` 行内 `#[allow(unused_imports)]`。
- **为何关注**：`#![allow(unused_imports)]` 掩盖了未使用的 `use`。这些 handler 模块经历过大重构（按域拆分），很可能残留迁移后不再需要的导入。
- **验证局限**：静态 Grep 无法判定具体哪条 `use` 失效（需编译器）。建议逐文件临时移除该 allow 后 `cargo check`，清理真正未用的导入。
- **风险等级**：**中**（低危但量多；逐文件移除 allow + cargo fix 即可）。

---

## 六、其它陈旧标记（信息性，多数为活跃兼容逻辑，勿轻删）

以下含 `legacy` 字样但经核查为**有效的向后兼容运行时逻辑**（处理旧磁盘格式 / 旧 wire 字段），删除会破坏对存量数据的兼容，**不建议在"宁缺毋滥"前提下删除**：

- `crates/gateway-server/src/security/auth/auth.rs:171-195`：legacy SHA-256 密码哈希校验 + 自动升级 bcrypt —— 活跃兼容逻辑。
- `crates/gateway-server/src/ws_rpc/handlers/config.rs:196,233`：兼容 `"channels"` 与旧 `"bridges"` 别名 —— 活跃。
- `crates/gateway-server/src/ws_rpc/handlers/config_core.rs:452-458`：暴露 "Model (legacy)" flat 字段供旧配置读取 —— 活跃。
- `crates/gateway-server/src/ws_rpc/mod.rs:291`：deprecated RPC 别名，注释 "kept as deprecated aliases so existing [clients work]" —— 需确认无旧客户端后才可删，风险 **高**。
- `crates/core/src/providers/info.rs:39` `CHAT_WIRE_API_DEPRECATION_SUMMARY`：面向用户的弃用提示文案，仍在显示，保留。
- `crates/core/src/features.rs:31-32,383-389` `Stage::Deprecated` + 两个 deprecated feature 定义：是 feature 弃用机制本身，活跃。

> 注：`apply-patch/src/invocation.rs:92`、`savfox/mod.rs:413`、`app/mod.rs:2652` 等 `TODO: remove/Remove this` 是**重构意向标记**，对应代码仍在使用，非死代码，勿动。

---

## 总结：本轮建议实际删除清单

| # | 位置 | 内容 | 行数估计 | 风险 |
|---|---|---|---|---|
| 1.1 | `channels/src/contrix/applet/ghost.rs:92-120` + 两处 re-export(`applet/mod.rs:35-36`、`contrix/mod.rs:37-38`) + 测试(`ghost.rs:241-~260`) | 废弃的 `build_ghost_profile` | ~50 | 中 |
| 1.2 | `gateway-server/src/channels/contrix.rs:513-524` | 未接线的 `handle_outbound_action` | ~12 | 低 |
| 1.3 | `core/src/rollout/list.rs:418-440,443-463,503-518` | `collect_dirs_desc`/`collect_files`/`collect_rollout_day_files` 死簇 | ~80 | 低 |

**需独立 PR（量大、需编译器辅助）**：
- 移除 `gateway-server/src/lib.rs:14` 的 `#![allow(dead_code)]` 并逐项清理约 79 处死代码（5.1）。
- 逐文件移除 11 处 ws_rpc handler 的 `#![allow(unused_imports)]` 并清理失效导入（5.2）。

**不建议删除**（误报排除）：第二、三、四、六节所列各项均为序列化目标、平台 cfg 分支、test-support、未完成 stub、或活跃的向后兼容逻辑。

---

## 复验记录（独立复核员严格证伪）

复核范围：第一节三条"确认可删除"条目，逐条在**整个 workspace** 重新 Grep 证伪，并排查 re-export / 测试 / trait impl / 宏 / serde / cfg 平台分支 / feature-gate 等豁免情形。第二～六节本身为"勿删/排除"清单或方向性建议，不构成误删风险，未改动。

### 1.1 `build_ghost_profile` —— 复核结论：**判定成立，保留删除建议**

- `Grep "build_ghost_profile\b"`（词边界，排除 `_event`）全仓库命中文件：仅 `crates/channels/src/contrix/{mod.rs, applet/mod.rs, applet/ghost.rs}` 三个文件（外加两份 review md，非代码）。
- 引用性质逐一确认：
  - `applet/ghost.rs:98-100` 定义，源码确有 `#[deprecated(note = "use build_ghost_profile_event (S-9, returns full Event)")]`。
  - `applet/ghost.rs:244` 仅本文件 `#[cfg(test)]` 测试调用（测试自身 `#[allow(deprecated)]`）。
  - `applet/mod.rs:35-36`：`#[allow(deprecated)]` + `pub use ghost::build_ghost_profile;`（行号经核实精确）。
  - `contrix/mod.rs:37-38`：`#[allow(deprecated)]` + `pub use applet::build_ghost_profile;`（行号经核实精确，报告原文写 36/38 略有偏移，实际 allow 在 35/37、use 在 36/38）。
- 豁免排查：`Grep "channels::.*build_ghost_profile" / "contrix::.*build_ghost_profile"` 零命中；`crates/channels/src/lib.rs` 不含该符号 re-export（`Grep` 零命中）—— 即未向 crate 顶层进一步暴露，workspace 内**无任何外部 crate 消费**。`build_ghost_profile_event`（活跃替代）与 `build_external_ref`/`mint_ghost_did` 走独立 `pub use`，不受影响。
- 唯一保留点：属 `channels` 库 crate 的 `pub` API（理论外部可引用）。报告已正确标"中"风险并附"不考虑向后兼容"前提，故**保留**。

### 1.2 `handle_outbound_action` —— 复核结论：**判定成立，保留删除建议**

- `Grep "handle_outbound_action"` 全仓库代码命中唯一一处：`crates/gateway-server/src/channels/contrix.rs:515` 定义本身，**零调用点**。无宏 / registry 字符串 / trait 注册引用。
- 连带影响排查：其内部调用的 `send_to_contrix_account` 在 `crates/gateway-server/src/channel/credential_manager.rs:1230` 有活跃调用点，删除 wrapper 不影响该函数。**保留**。

### 1.3 `collect_dirs_desc` / `collect_files` / `collect_rollout_day_files` —— 复核结论：**判定成立，保留删除建议**

- `Grep "collect_dirs_desc|collect_rollout_day_files"` 代码命中仅 `crates/core/src/rollout/list.rs` 一文件（定义处），**零外部调用**。
- **同名陷阱排查**：`Grep "collect_files"` 另命中 `crates/app-server-protocol/src/schema_fixtures.rs` 的 `collect_files_recursive`（:18,21,170）—— 经确认是**不同函数**（不同 crate、不同签名），与本簇无关，不受删除影响。
- 闭环确认：core 内 `collect_files`（list.rs:444）唯一调用者为 `collect_rollout_day_files`（list.rs:507），而后者零外部入口 —— 三者构成死代码闭环。活跃替代 `collect_flat_rollout_files`（list.rs:465，无 `#[allow(dead_code)]`）不受影响。删除时需一并清理仅本簇使用的 `use ...Reverse` 等导入（`Reverse` 仍被 `collect_flat_rollout_files` 使用，**不可删 `Reverse` 导入**）。**保留**。

### 复验汇总

- 原有删除条目：**3 条**（1.1 / 1.2 / 1.3）。
- 删除（误报，从报告剔除）：**0 条**。
- 保留：**3 条**，均经全 workspace 独立 Grep 证伪为零有效引用，且不属于 serde / 宏 / cfg 平台分支 / feature-gate / 外部 pub 消费 等豁免情形。
