# Savfox 结构审核报告（模块/crate 划分、职责、依赖方向）

> 审核范围：crates/channels（contrix）、crates/gateway-server、crates/gateway-shared、crates/gateway-dioxus、crates/core、crates/config，以及整体 crate 依赖方向。
> 每条结论均附实际读过的文件:行号 / 行数证据。

---

## 一、最严重：channel adapter 的「双 crate 职责劈裂」

### 现状
同一个 IM 平台的逻辑被切成两半，分属两个 crate：

- `crates/channels/src/<platform>/`（savfox-channels）：声明的职责是 `client.rs`（无状态 API 调用）+ `parse.rs`（inbound payload → `ChannelAction`）+ `config.rs`（typed config wrapper）。见 `crates/channels/src/lib.rs:1-24` 的模块注释。
- `crates/gateway-server/src/channels/<platform>.rs`：HTTP webhook 路由、OAuth 回调、运行时编排。例如 `discord.rs` 直接 `use savfox_channels::discord::{parse_message_with_resolver, ...}`（`crates/gateway-server/src/channels/discord.rs:4-10`），自己再实现 callback HTML、salvo handler、与 runtime 的对接。

两边都有 14+ 个平台同名文件（`channels/src/*` 与 `gateway-server/src/channels/*` 几乎一一对应）。

### 问题
1. **职责边界靠约定维系，没有编译期约束**。"client/parse 放 channels crate，HTTP/runtime 放 gateway-server" 完全是口头约定，新增平台时极易放错层；实际已经出现 contrix 在两个 crate 都铺了完整子树（见第二节）。
2. **修改一个平台要跨 crate 跳转**，定位成本高；`gateway-server/src/channels/` 目录顶层 23 个 `.rs` 文件（含 runtime 子目录共 30 个）、`matrix.rs` 单文件 1969 行（`crates/gateway-server/src/channels/matrix.rs`），与 `channels/src/matrix/` 又是另一套。
3. **gateway-server 这个 crate 已经无所不包**（见第四节），channel runtime 本可下沉到 savfox-channels 让 gateway-server 只做"装配"。

### 建议
把"平台运行时编排（webhook handler 抽象、inbound→agent、outbound 投递）"也下沉到 savfox-channels，gateway-server 只保留 salvo 路由注册这一薄层（通过 trait 对象 `dyn ChannelRuntime` 装配）。即：定义 `trait ChannelAdapter { fn parse(...); async fn deliver(...); fn webhook_routes(...) }`，每个平台一个实现，gateway-server 遍历注册。

### 收益
单平台逻辑收敛到一处；gateway-server 体积大幅下降；新增平台只实现一个 trait，不会放错层。

### 成本
**高**。涉及 ~14 个平台、salvo Depot/State 的解耦，需要 2-3 个迭代分平台搬迁。建议先抽 trait + 迁移 1 个简单平台（如 line）验证，再批量。

---

## 二、contrix 子模块：结构本身合理，但被复制到两个 crate

### 现状（结构本身是好的）
`crates/channels/src/contrix/` 分层清晰、注释到位（`mod.rs:1-58`）：
```
contrix/
  client.rs(155) config.rs(426) grant.rs(397) outbound.rs(176)
  parse.rs(249) session.rs(111) signer.rs(286)
  applet/{config,ghost,namespace,outbound,registration,transaction}.rs(43~463)
```
每个文件 100~463 行，单一职责，applet 子系统独立成目录——**这是全仓库分层做得最好的模块之一**，可作为其他 channel 重构的模板。

### 问题
1. **gateway-server 侧又有一套 contrix**：`gateway-server/src/channels/contrix.rs`(524 行) + `contrix_applet.rs`(1180 行)，合计 1704 行。`contrix_applet.rs` 单文件 1180 行已超出健康阈值，而 channels crate 的 applet 已经拆成 6 个文件——**同一子系统两种切分粒度并存**，是第一节"双 crate 劈裂"最严重的体现。
2. **contrix SDK 依赖是仓库外相对路径，且在两个 crate 重复声明**：
   `crates/channels/Cargo.toml:30-33` 与 `crates/gateway-server/Cargo.toml:45-48` 各写了一遍
   ```
   contrix = { path = "../../../../contrix-dev/contrix-rust-sdk/..." }
   ```
   `../../../../` 指向仓库**外部**的兄弟目录，任何 CI / 新 clone 在没有 contrix-dev 同级目录时直接 `optional` 关闭——但路径硬编码进两个 Cargo.toml，是脆弱的依赖管理。

### 建议
- 将 `gateway-server/src/channels/contrix*.rs` 中的纯逻辑（applet 事务、ghost 编排）下沉合并进 `channels/src/contrix/applet/`，gateway-server 只留 webhook 装配。
- 4 个 contrix path 依赖收敛到 workspace 的 `[workspace.dependencies]`（`Cargo.toml` 根），两个 crate 改为 `contrix = { workspace = true, optional = true }`，路径只写一处。

### 收益
消除 1180 行上帝文件；依赖路径单点维护；contrix 逻辑全部在 savfox-channels 内自洽。

### 成本
**中**。path 依赖收敛是低成本（半天）；逻辑下沉中等。

---

## 三、命名冲突：gateway-server 的 `channel`（单数） vs `channels`（复数）

### 现状
`crates/gateway-server/src/lib.rs` 同时声明两个顶级模块：
- `pub mod channel;`（`channel/mod.rs:18-21` → auth / credential_manager / router / session_bridge）——这是**核心 bridge / GatewayChannel** 编排层。
- `pub mod channels;`（`#[path="channels/mod.rs"]`，lib.rs:55-57）——这是**各 IM 平台适配器**集合。

`server.rs:14` 同时 `use crate::channel::GatewayChannel` 和 `use crate::{channels, ...}`（server.rs:27-30）。

### 问题
`channel` 与 `channels` 只差一个 `s`，但语义完全不同（一个是会话桥接核心对象，一个是平台适配集合）。阅读 `use crate::channel::...` vs `use crate::channels::...` 极易看错，是典型的模块边界命名泄漏 / 认知负担。

### 建议
重命名为语义清晰的名字，如 `channel` → `bridge`（`GatewayBridge`），`channels` → `platforms` 或 `channel_adapters`。

### 收益
消除 50+ 处 `use crate::channel(s)` 的歧义；降低新人误用风险。

### 成本
**低**。纯重命名 + 全局 import 替换，一次 PR 可完成（注意 lib.rs 的 re-export `pub use channel::...`）。

---

## 四、gateway-server 是「上帝 crate」——159 个源文件、职责无所不包

### 现状证据
`crates/gateway-server/src/` 顶级模块（`lib.rs:48-103`）就有 50+ 个，覆盖：
- 通信：ws / ws_rpc / webchat / webhooks / web
- 业务：voice（STT/TTS/talk_mode/voice_wake，独立子树 14 文件）、media_understanding、memory_service、canvas_host、browser
- 存储：json_store / log_store / media_store / cached_db / pairing_store / skills_store / wizard_store / approval_policy_store
- 安全：security/（auth/ssrf/rate_limit/redaction/path_safety/security_audit）
- 平台：channel + channels（见第三节）
- 运行时：runtime/{routing/*, session/*}、terminal_*（见第五节）

依赖（`Cargo.toml`）引入了 image、lopdf、sqlx、salvo、matrix-bot-sdk、feishu-sdk、rust-embed、bcrypt…——一个 crate 同时是 Web 服务器、PDF 解析器、图像处理器、语音引擎、数据库层。

### 问题
1. **编译单元巨大**：任何 voice 改动都要重编整个 gateway-server。
2. **`lib.rs:1-33` 的 allow 列表**：`#![allow(unreachable_pub, dead_code)]` + 一长串 clippy allow，注释里写明"剩余 ~79 处 dead_code 警告"——结构臃肿到无法逐一治理，只能整体 allow，这本身就是上帝 crate 的征兆。
3. **`browser.rs` handler 3096 行**（`ws_rpc/handlers/browser.rs`）：浏览器自动化的 RPC 编排塞进 gateway-server，而真正的 browser 引擎在独立 crate `savfox-browser-automation`。这层 3096 行编排应独立。

### 建议
按"可独立编译的子系统"抽出 crate：
- `savfox-voice`（voice/ 整个子树 + tts/stt deps）
- `savfox-media`（media_understanding + media_store + image/lopdf deps）
- `savfox-channel-runtime`（配合第一节，承载平台 runtime）
gateway-server 退化为"HTTP/WS 装配 + RPC dispatch"。

### 收益
增量编译大幅加速；dead_code 可按 crate 治理；voice/media 可被其他二进制复用。

### 成本
**高**，但可渐进。voice 子树边界相对清晰，建议**首先抽 savfox-voice**作为试点（依赖单向、内聚高）。

---

## 五、terminal 相关 4 文件散落 lib 根目录，命名职责不清

### 现状
平铺在 `gateway-server/src/` 根下（非子目录）：
- `terminal_agent.rs`(1707) — terminal agent 运行时
- `terminal_pty.rs`(993) — PTY 管理
- `agent_terminal_delegate.rs`(1704) — agent→terminal 委派
- `agent_terminal_launcher.rs`(615) — 启动器

合计 5019 行，4 个文件命名前缀在 `terminal_*` 与 `agent_terminal_*` 之间摇摆。对应的 RPC 入口 `agent.terminal.*`（`ws_rpc/mod.rs:97-108`，12 个方法）。

### 问题
1. **命名不一致**：`terminal_agent` vs `agent_terminal_delegate` 词序相反，无法一眼判断从属关系。
2. **未归组**：4 个强相关文件散在 ~50 个根模块里，且 `terminal_agent.rs` 1707 行 / `agent_terminal_delegate.rs` 1704 行都是巨型文件。
3. RPC handler 入口在 `handlers/agent.rs`(2421 行)，逻辑在根目录 4 文件，又一次"入口与实现分离"。

### 建议
归入 `gateway-server/src/terminal/` 子目录：`terminal/{agent.rs, pty.rs, delegate.rs, launcher.rs}`，统一前缀；两个 1700 行文件按内部职责（state machine / IO / 协议）再拆。配合第四节可整体作为 `savfox-terminal` crate 候选。

### 收益
相关代码内聚；命名一致；为后续抽 crate 铺路。

### 成本
**低**（归目录 + rename）到中（拆大文件）。

---

## 六、Dioxus 前端：超大 page 文件 + 缺少组件拆分

### 现状证据（`crates/gateway-dioxus/src/pages/`）
- `agents.rs` 5572 行，仅 10 个 `#[component]`（`grep -c` 实测），平均 **557 行/组件**；前 600 行全是 `json_terminal_*` / `detail_terminal_*` 等手写 JSON↔表单字段转换辅助函数（agents.rs:283-608）。
- `channels/mod.rs` 4514 行，5 个组件，主组件 `Channels` 从 1532 行起（channels/mod.rs:1531-1532），单组件可能上千行；前 1500 行是 `*_key` / `build_*_value` / `restore_*` 等表单↔JSON 胶水函数。
- `config.rs`(2434) / `overview.rs`(1865) / `sessions.rs`(1856) 同样过大。

### 问题
1. **业务字段映射逻辑（JSON Value ↔ 表单 String）大量手写、内联在 page 文件**。`agents.rs` 和 `channels/mod.rs` 各自有几十个 `fn xxx_key(channel_id)` / `fn build_xxx_value`，这类逻辑应是类型化的、可测试的、与 UI 渲染分离的。
2. **page 同时承担：数据获取、表单状态、JSON 编解码、渲染**——上帝组件。
3. 对照已有的 `src/utils/`（debounce/storage/text/time/provider_catalog 等 13 个工具模块）和 `src/api/ws.rs`（RPC client 已抽象，agents 缺）——说明前端**已有抽层意识，但 agents/channels 两个最复杂页面没跟上**。

### 建议
- 每个大 page 拆为目录：`pages/agents/{mod.rs(渲染), form.rs(字段映射), detail.rs}`，参照已有的 `pages/models/`（已是目录：`mod.rs + connect_provider.rs`）。
- 字段↔JSON 映射抽成强类型 struct + `From`/`TryFrom`，移出组件函数，单测覆盖。

### 收益
组件可读、字段映射可单测；与后端 wire 类型漂移可被测试捕获。

### 成本
**中**。机械拆分为主，无架构风险，可逐页进行。

---

## 七、core/config/mod.rs 4548 行——上帝模块 + 与 config crate 边界模糊

### 现状
- `crates/core/src/config/mod.rs` 4548 行，承载 `Config`、`ConfigBuilder`、`ConfigToml`、`ProjectConfig`、`ToolsToml`、`AgentsToml`、`ConfigOverrides`、`SandboxPolicyResolution` 等 10+ 个核心 struct（grep `^pub struct` 实测，251~1329 行）。
- 已有独立 `savfox-config` crate（`crates/config/src/lib.rs`），注释说明其定位是"提取自 core 的 leaf types + 纯工具，不依赖 core 运行时"。
- 但 `core/config/mod.rs:6-15` 大量 `use savfox_config::types::{...}` 再 `pub use savfox_config::{...types}` **整体转发**——core 把 config crate 的类型又包了一层重新导出。

### 问题
1. **拆分只做了一半**：leaf types 进了 savfox-config，但 `Config`/`ConfigBuilder` 这种"重对象"仍是 4548 行巨型文件，且通过 `pub use` 把 config crate 类型透传，使用方分不清类型究竟住在哪个 crate（MEMORY 已记录过 ModelInfo 同名混淆的历史教训，这里是同类风险）。
2. `ConfigBuilder`(510 行起) 与 `Config`(251) 在同一文件，builder 模式与目标类型未分离。

### 建议
- `core/config/mod.rs` 按 struct 拆文件：`config.rs`(Config) / `builder.rs`(ConfigBuilder) / `toml.rs`(ConfigToml 系列) / `overrides.rs`，mod.rs 只留组装。
- 明确 savfox-config（leaf types）与 core::config（运行时聚合）的边界：能下沉到 config crate 的纯类型继续下沉，core 侧停止"整体 re-export"，改为显式按需导入，减少类型住址歧义。

### 收益
4548 行可维护化；类型归属清晰；config crate 真正成为可被非 core 使用的 leaf crate。

### 成本
**中**。core/config 被全仓库引用，拆文件需谨慎处理可见性，但不改公共 API 即可。

---

## 八、ws_rpc/handlers 拆分情况——方向正确，但 dispatch 仍是巨型 match

### 现状（好的部分）
handlers 已按域拆 12 个文件（`ws_rpc/handlers/`，不含 mod.rs）：agent/browser/channel/channel_management/config/config_core/cron/model/node/session/skill/system。这是**正确的拆分方向**。

### 问题
1. **dispatch 巨型 match**：`ws_rpc/mod.rs:83` 起的 `match method.as_str()` 手工列举 96+ 方法（注释 mod.rs:43-45 自述），每个 handler 的参数（`&params, channel, session_mgr, ...`）在每个 arm 手工传递。新增方法要改这个中心 match + scope 表，是中心化耦合点。
2. **handler 文件仍偏大**：`browser.rs` 3096 / `agent.rs` 2421 / `channel.rs` 2310 / `session.rs` 2248。browser 见第四节应独立。
3. dispatch 的依赖参数列表（mod.rs:46-54）共 8 个参数，其中 5 个为 `&Arc<...>`（`_auth` / `session_mgr` / `channel` / `session_store` / `cron_service`），是参数过多的上帝函数签名。

### 建议
- 用 `HashMap<&str, Handler>` 注册表或 per-domain 的 sub-dispatch（如 `agent::dispatch(method, ctx)`）替代单层 match；把这一组 Arc 依赖收进一个 `RpcContext` struct 传递。
- scope 表（`required_scope`）与方法注册放一起，避免新增方法两处改。

### 收益
新增方法只改对应域文件；dispatch 签名收敛；降低中心耦合。

### 成本
**中**。引入 RpcContext 是机械重构，96 个 arm 需逐个适配但风险低。

---

## 九、整体 crate 依赖方向——基本健康，但有两处倒置风险

### 观察（约 44 个 crate）
- 分层基本是单向的：`gateway-server → {core, channels, gateway-shared, model, memory, ...}`，`gateway-dioxus → gateway-shared`（只依赖 wire types，wasm 友好，`gateway-shared/Cargo.toml` 仅 serde/serde_json，零平台依赖——**这条边界守得很好**）。
- savfox-config 作为 leaf crate 被 core 依赖，方向正确。

### 风险点
1. **savfox-channels → savfox-core**（`channels/Cargo.toml`: `savfox-core = { workspace = true }`）：channels 依赖整个 core（重运行时），只为用 `savfox_core::channel::{Channel, RichMessage, ChannelAction}`。这几个是 leaf trait/类型，**应下沉到一个 `savfox-channel-types` 轻 crate**（或 savfox-config），让 channels 不必拖入整个 core。否则 channels 永远无法独立于 core 编译。
2. **两个 protocol crate 命名易混**：`savfox-protocol`（`protocol/src/lib.rs`：通用协议/openai_models）与 `savfox-app-server-protocol`（`app-server-protocol`：JSON-RPC v1 wire）。职责确实不同，但 core 同时依赖两者（`core/Cargo.toml` 都有），命名只差 `app-server-` 前缀，建议文档/命名上更清晰区分（如 `savfox-wire-protocol`）。

### 建议
抽 `savfox-channel-types`（Channel trait + RichMessage + ChannelAction），channels 与 gateway-server 都依赖它而非 core 全量。

### 收益
打断 channels→core 的重依赖，channels 可独立编译/测试；为第一节的 runtime 下沉扫清依赖障碍。

### 成本
**中**。需确认 `Channel` trait 不依赖 core 其他重类型。

---

## 优先级汇总（投入产出比排序）

| 优先级 | 项 | 成本 | 收益 |
|---|---|---|---|
| P0 | §3 `channel`/`channels` 重命名 | 低 | 立刻消除高频歧义 |
| P0 | §2 contrix path 依赖收敛到 workspace | 低 | 依赖单点、CI 稳定 |
| P1 | §5 terminal 4 文件归目录 + 统一命名 | 低 | 内聚、为抽 crate 铺路 |
| P1 | §9 抽 savfox-channel-types 打断 channels→core | 中 | 解依赖倒置、解锁后续重构 |
| P1 | §6 Dioxus agents/channels 大 page 拆分 | 中 | 可测试、可读 |
| P2 | §7 core/config/mod.rs 拆文件 | 中 | 4548 行治理 |
| P2 | §8 ws_rpc dispatch 注册表化 + RpcContext | 中 | 解中心耦合 |
| P2 | §4 抽 savfox-voice 试点（上帝 crate 拆分起点）| 高 | 编译加速、可复用 |
| P3 | §1/§2 channel runtime 下沉 savfox-channels | 高 | 终结双 crate 劈裂（依赖 P1 §9）|

**建议落地顺序**：先做 P0 两项（半天、零风险）→ §9 抽 channel-types（解锁后续）→ §5 terminal 归组 →（试点）§4 savfox-voice → 再推进 §1 的 channel runtime 下沉。

---

## 复验记录

> 复核员逐条核实硬事实（行号、文件数、依赖关系、命名）。验证基线：`crates/` 实际文件。

### 修正的事实错误

- **§1 修正**：`gateway-server/src/channels/` 原称「24 个文件」，实测顶层 23 个 `.rs`（含 `runtime` 子目录递归共 30 个）。已改为 23。其余事实（matrix.rs 1969 行、discord.rs `use savfox_channels::discord::{...parse_message_with_resolver}`、channels/src/lib.rs 模块注释）核实属实。
- **§4 修正**：标题与正文「111 个源文件」与实测不符，`find crates/gateway-server/src -name '*.rs'` 实测 **159** 个，已修正。voice 子树原称「12 文件」，实测 **14** 个（含 talk_mode/voice_wake 子目录），已修正。lib.rs 的 `#![allow(unreachable_pub, dead_code)]` 与「剩余 ~79 处 dead_code」注释、browser.rs 3096 行均属实。
- **§5 修正**：`agent.terminal.*` RPC 方法原称「11 个」，`ws_rpc/mod.rs:97-108` 实测 **12** 个（含 `pty.close_idle`），已修正。4 个 terminal 文件行数（1707/993/1704/615，合计 5019）核实精确无误。
- **§8 修正**：dispatch 签名原称「8 个 `&Arc<...>`」，实测 8 个参数中只有 **5 个**为 `&Arc<...>`（`_auth`/`session_mgr`/`channel`/`session_store`/`cron_service`），其余为 `&str`/`&TokenInfo`，已修正。handlers 原称「11 个文件」却列出 12 个域，实测确为 **12** 个文件（不含 mod.rs），已修正。「96+ 方法」源自 mod.rs 文档注释自述，属实保留。
- **§9 修正**：原称「约 47 个 crate」，实测 `crates/` 下含 Cargo.toml 的目录 **44** 个，已改为「约 44」。

### 确认成立、保留的事实

- **§2**：contrix 各文件行数（client 155 / config 426 / grant 397 / outbound 176 / parse 249 / session 111 / signer 286，applet 子文件 43~463）逐一核实精确。`contrix.rs`(524)+`contrix_applet.rs`(1180)=1704 行属实。两个 Cargo.toml 各自硬编码 `../../../../contrix-dev/...` path 依赖、且根 `Cargo.toml` 无 `[workspace.dependencies]` contrix —— 全部属实。（注：path 依赖每个 Cargo.toml 实为 4 条 × 2 文件 = 8 条；报告「4 个 contrix path 依赖收敛」指单文件视角，表述无误。）
- **§3**：`pub mod channel;`（lib.rs:58）与 `pub mod channels;`（lib.rs:60）并存属实；`channel/mod.rs` 确为 auth/credential_manager/router/session_bridge 编排层；`server.rs:14 use crate::channel::{GatewayChannel, ...}` 属实。命名冲突客观存在，建议成立。
- **§6**：`agents.rs` 5572 行 / 10 个 `#[component]`、`channels/mod.rs` 4514 行 / 5 组件（`Channels` 起于 1532 行）、config 2434 / overview 1865 / sessions 1856 —— 全部精确。`pages/models/` 确为目录、`utils/` 12 个非 mod 模块（报告称「13 个」含 mod.rs，可接受）、`api/ws.rs` 存在 —— 属实。
- **§7**：`core/src/config/mod.rs` 4548 行精确；`^pub struct` 实测 10 个（Config@251 … ConfigOverrides@1329），范围 251~1329 与报告一致；`use savfox_config::types::{...}` + `pub use savfox_config::{...}` 整体转发属实。
- **§9**：`savfox-core = { workspace = true }`（channels/Cargo.toml:42）属实，channels 确实依赖整个 core；`Channel`/`ChannelAction` 来自 `savfox_core::channel`，`RichMessage` 定义于 `core/src/channel.rs:19` 且为 `Channel` trait 的 `send_rich_message` 入参（channels 实现该 trait），故报告将其列为 leaf 类型成立。`savfox-protocol` 与 `savfox-app-server-protocol` 双 crate 并存、core 同时依赖二者 —— 属实。

### 主观建议（保留但属偏好/收益存疑）

- §1「定义 `trait ChannelAdapter` 下沉 runtime 到 savfox-channels」、§3 重命名 `channel→bridge`/`channels→platforms`、§4 抽 `savfox-voice`/`savfox-media`、§5 归 `terminal/` 目录、§8 注册表化 + `RpcContext`、§9 抽 `savfox-channel-types`：事实依据均成立，但具体拆分方式与收益评估带主观性，列为**主观架构建议**，由维护者按迭代节奏取舍。

### 汇总

原有 **9** 条结构建议（§1–§9）。本次复核发现 **6 处硬事实错误**（§1 文件数、§4 源文件总数、§4 voice 文件数、§5 RPC 方法数、§8 Arc 参数数、§8 handlers 文件数）+ **1 处需收敛的近似值**（§9 crate 数），均已 Edit 修正。**0 条**整条建议因事实不成立而删除（所有 9 条的核心事实依据经核实均能成立，仅部分计数有偏差）。保留全部 **9** 条建议，其中纯主观部分已在上文标注。
