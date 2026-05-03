# Savfox 项目深度分析报告

## 1. 分析范围与方法

本次分析基于静态阅读完成，未执行 `cargo build`、`cargo test`、`clippy` 或运行网关/前端。

本次重点阅读了以下文件与入口：
- `Cargo.toml`
- `Justfile`
- `README.md`
- `docs/en/SUMMARY.md`
- `docs/en/concepts/architecture.md`
- `scripts/build-web.ps1`
- `crates/savfox-cli/Cargo.toml`
- `crates/savfox-cli/src/main.rs`
- `crates/core/Cargo.toml`
- `crates/core/src/lib.rs`
- `crates/tui/Cargo.toml`
- `crates/tui/src/lib.rs`
- `crates/gateway-server/Cargo.toml`
- `crates/gateway-server/src/lib.rs`
- `crates/app-server/Cargo.toml`
- `crates/app-server/src/lib.rs`
- `crates/protocol/Cargo.toml`
- `crates/protocol/src/lib.rs`
- `crates/app-server-protocol/Cargo.toml`
- `crates/app-server-protocol/src/lib.rs`
- `crates/gateway-dioxus/Cargo.toml`
- `crates/gateway-dioxus/src/main.rs`
- `crates/gateway-shared/Cargo.toml`
- `crates/gateway-shared/src/lib.rs`
- `crates/channels/Cargo.toml`
- `crates/channels/src/lib.rs`
- `crates/mcp-server/Cargo.toml`
- `crates/mcp-server/src/lib.rs`
- `crates/savfox/Cargo.toml`
- `crates/savfox/src/lib.rs`
- `crates/browser-automation/Cargo.toml`
- `crates/browser-automation/src/lib.rs`

## 2. 总体结论

Savfox 目前已经不是单一 CLI 工具，也不是单一 SDK，而是一个多入口、多协议、多运行表面的 AI Agent 平台。它的核心竞争力在于：
- 以 `savfox-core` 为中心复用核心能力。
- 同时支持 CLI、TUI、MCP、app-server、gateway、web frontend 和多聊天渠道。
- 在配置、协议、沙箱、会话、模型接入、工具调用方面已经具备平台级雏形。

当前项目最明显的问题不是“功能不够”，而是“边界逐渐模糊”。随着能力增长，`core`、`gateway-server`、`savfox-cli` 正在承担越来越多职责，协议类型也在多个 crate 中分散演进。继续沿当前方向扩展，短期仍能工作，但中期会明显增加维护成本、文档漂移概率和回归风险。

## 3. 当前架构拆解

## 3.1 接入层

当前至少有 6 个主要入口：
- `savfox-cli`：统一命令入口，也是用户最直接的总分发器。
- `savfox-tui`：交互式终端产品面。
- `savfox-app-server`：IDE/编辑器集成的 stdio JSON-RPC 服务端。
- `savfox-mcp-server`：作为 MCP server 暴露能力。
- `savfox-gateway-server`：远程 HTTP/WebSocket 接入、会话、cron、channel bridge。
- `savfox-gateway-dioxus`：网关的 Web 前端。

这说明项目已经具备“同一核心，多前端/多传输”的平台特征。

## 3.2 核心能力层

`crates/core/src/lib.rs` 暴露了非常大的能力面，覆盖：
- auth / auth_profiles
- config / config_loader
- agent / delegate / subagent / spawn
- exec / shell / parse_command / sandboxing
- mcp / connectors / tools
- rollout / session 管理
- skills / custom_prompts / project_doc / memory
- models / provider / web_search / remote_models
- analytics / otel / updater / transcript_policy

这说明 `savfox-core` 已经是绝对的中台层，也是整个工作区的最高价值代码资产。

## 3.3 协议层

当前协议层至少分为 3 组：
- `savfox-protocol`：跨工作区的基础协议与共享模型。
- `savfox-app-server-protocol`：app-server/IDE 集成协议，带导出与 schema 生成能力。
- `savfox-gateway-shared`：gateway backend 与 Dioxus frontend 共享的 serde 类型。

这个拆分本身是合理的，但随着功能增长，类型所有权会变成治理重点。

## 3.4 集成与基础设施层

除核心之外，项目还维护了大量基础设施与集成模块：
- `savfox-channels`：16 个左右聊天渠道适配器。
- `savfox-browser-automation`：浏览器自动化能力。
- `savfox-memory`：长期记忆相关能力。
- `savfox-linux-sandbox` / `savfox-windows-sandbox`：平台沙箱。
- `savfox-api-client` / `savfox-http-client` / `savfox-model`：模型与网络层。

这类模块的数量说明项目能力面很广，也说明发布与测试矩阵天然复杂。

## 3.5 构建与交付层

根目录的 `Justfile` 和 `scripts/build-web.ps1` 体现出较成熟的开发体验设计：
- `just gateway` / `gateway-release` 统一前后端构建与启动。
- `build-web.ps1` 已做 fingerprint 缓存与静态资源同步，避免无谓重建。
- `gateway-server/static` 与 Dioxus 输出做同步，说明网关是“服务端嵌入静态前端”的交付模式。

这个方向是务实的，但也让 frontend build 和 backend 交付链路形成了较强耦合。

## 4. 项目的主要优点

### 4.1 中台化方向是正确的

`core` 被 CLI、TUI、app-server、MCP 等多处复用，说明项目没有走“每个入口各写一套逻辑”的路线。这是当前最重要的架构优点。

### 4.2 Workspace 治理意识较强

从根 `Cargo.toml` 可以看出：
- workspace dependency 管理集中。
- Rust edition、rust-version、lint policy 统一。
- `unsafe_code = deny`、`unreachable_pub = deny` 等规则明确。
- 很多库禁止直接 stdout/stderr 输出，边界意识清晰。

这对大型 Rust workspace 非常重要。

### 4.3 产品面覆盖广

项目已经覆盖：
- 终端交互
- 非交互执行
- IDE 集成
- MCP 集成
- 远程 gateway
- Web UI
- 多聊天渠道

这使得 Savfox 具备平台级扩展潜力，不局限于单一使用方式。

### 4.4 Web 构建脚本做得比较扎实

`scripts/build-web.ps1` 不是简单执行 `dx build`，而是处理：
- 输入指纹
- out_dir 检测
- static 目录同步
- copy 跳过策略
- 强制重建与 release 构建

这体现出对前端构建成本和开发体验的关注。

### 4.5 文档框架已经具备规模

`docs/en/SUMMARY.md` 覆盖了 CLI、gateway、channels、providers、security、automation、nodes、tools 等主题，说明项目已经具备较完整的文档骨架。

## 5. 核心问题与风险点

### 5.1 `savfox-core` 已接近“超大核心 crate”

`core/src/lib.rs` 的公开模块面非常大，当前的风险不在于它不强，而在于它过于强：
- 配置、会话、模型、工具、执行、代理、MCP、web search、sandbox、更新、记忆都在同一核心域里。
- 这会导致依赖方向更难收敛。
- 新功能很容易继续直接塞进 `core`，让边界进一步变弱。

判断：`savfox-core` 目前是必要中心，但已经需要“继续做中台”转向“开始治理边界”。

### 5.2 `savfox-gateway-server` 职责过宽，正向单体网关演化

`gateway-server/src/lib.rs` 中挂载了大量模块：
- auth
- cron_service
- memory_service
- media_store / media_understanding
- plugin
- provider_health
- security_audit
- session
- skills_api
- stt / tts / voice_wake / talk_mode
- webchat / ws / ws_rpc
- channel / channels / discovery / pairing_store

这说明它不只是 HTTP server，而是同时承担：
- 远程 API 网关
- 会话编排器
- 任务调度器
- 多渠道桥接层
- 语音/媒体入口
- 运维管理面

判断：这是当前最明显的复杂度热点。后续如果不做边界治理，网关会成为新的“第二核心”。

### 5.3 `savfox-cli` 的命令聚合已经很重

`savfox-cli/src/main.rs` 当前汇聚了 30+ 子命令方向，包括：
- exec / review / login / mcp / app-server / gateway
- sandbox / doctor / wizard / sessions / agents / memory / skills / plugins
- config / cron / daemon / docker / dns / dashboard / directory / security / status / uninstall / update

这带来两个问题：
- CLI 是非常强的统一入口，但认知负担很高。
- 命令组织会逐渐变成“继续堆子命令”而不是“定义产品边界”。

### 5.4 协议分层是优点，但也开始有漂移风险

当前至少存在：
- `savfox-protocol`
- `savfox-app-server-protocol`
- `savfox-gateway-shared`

三套协议/类型共享层。

这在多表面系统里很常见，但如果没有明确的所有权规则，后续会出现：
- 同一语义在多个 crate 重复定义。
- 迁移时不知道类型该归谁。
- web、IDE、CLI、gateway 之间的行为逐步分叉。

### 5.5 文档与元数据已经出现真实漂移

这个问题不是抽象担忧，而是已经可以从当前代码中直接观察到：
- 根 `Cargo.toml` 的 workspace description 仍是 `Experimental AI API client library for Rust.`，与当前“多入口 AI Agent 平台”的事实不匹配。
- `docs/en/concepts/architecture.md` 仍在使用 `codex-api` 这一旧名称，而仓库说明已经明确不应继续传播旧名。
- 同一文档中写的是 `Salvo (v0.89)`，而当前 workspace 依赖是 `salvo = 0.91`。

判断：文档和元数据已经开始滞后于当前项目形态，这会直接影响外部理解、贡献者上手和后续改造判断。

### 5.6 依赖面广且包含多处 git 依赖，供应链治理要提前做

根 `Cargo.toml` 中存在多处直接 git 依赖或 branch 依赖，例如：
- `eventsource-stream`
- `reqwest-eventsource`
- `opentelemetry*` 指向 git `main`
- `nucleo`
- `runfiles`
- `dingtalk-sdk`

这不是一定错误，但意味着：
- 可复现构建复杂度更高。
- 上游变动风险需要持续跟踪。
- 升级、排障和离线构建成本都会更高。

### 5.7 Web 交付链路可用，但耦合度偏高

当前前端构建模式是：
- Dioxus 构建输出。
- 同步到 `gateway-dioxus` 的 out_dir。
- 再同步到 `gateway-server/static`。
- 由网关内嵌静态资源对外服务。

优点是部署简单；缺点是：
- frontend/backend 发布节奏天然绑定。
- 构建、缓存、同步、嵌入链路中的任何一步出问题，都会影响最终网关行为。
- 在多人协作与 CI 中，构建产物责任边界需要更清楚。

### 5.8 渠道接入能力很强，但运维复杂度同样很高

`crates/channels/src/lib.rs` 暴露了大量渠道适配器，这说明项目已经具备很强的对外接入能力。但渠道越多，越需要：
- 更统一的 adapter contract
- 更清晰的配置校验
- 更稳定的 smoke test
- 更明确的“实验性/稳定性等级”标记

否则渠道数量本身会反向拖累整体可维护性。

## 6. 我对当前项目阶段的判断

Savfox 现在处于“从功能扩展期进入结构治理期”的阶段。

如果目标只是继续加能力，当前结构还能支撑一段时间；但如果目标是：
- 更稳定地对外发布
- 支撑更多客户端/渠道
- 降低新人接入成本
- 降低跨模块回归风险

那下一阶段的重点就不应该是继续横向加功能，而应该是：
- 收敛边界
- 固化协议所有权
- 修正文档与命名漂移
- 给 `core` 和 `gateway-server` 减压

## 7. 建议的改进方向

### 7.1 先修正“认知层”的错误信息

第一优先级不是大重构，而是先把会误导开发者的描述修正：
- workspace description
- 架构文档中的旧命名
- 版本描述错误
- 中英文文档不一致项

这是低风险高收益项。

### 7.2 给 `core`、`gateway-server`、协议层建立正式边界

建议尽快补一份“crate ownership + dependency rules”文档，至少回答：
- 什么能力必须进 `core`。
- 什么能力只能留在 gateway。
- `savfox-protocol`、`savfox-app-server-protocol`、`savfox-gateway-shared` 各自负责什么语义。
- 哪些 crate 允许直接依赖哪些 crate。

### 7.3 抽取重复的启动/运行时样板

从 `app-server`、`mcp-server`、`gateway-server` 可以看出，有一类重复模式持续出现：
- tracing 初始化
- config 加载
- stdin/stdout 或消息通道搭建
- processor + writer/read loop

建议识别能共享的 bootstrap/runtime 组件，减少每个入口各自维护一套启动骨架。

### 7.4 对网关做按领域的拆分试点

不建议一次性拆散整个 `gateway-server`，但建议先做 bounded-context 试点，例如把以下之一先抽清楚：
- session + ws_rpc
- media/stt/tts/voice
- plugin/skills/memory
- channel runtime

目标不是“拆 crate 数量”，而是“让网关内部责任边界更清楚”。

### 7.5 建立协议类型治理规则

建议对三层协议 crate 做一次类型盘点，输出：
- 哪些类型是真正的跨端通用模型。
- 哪些类型只属于 app-server。
- 哪些类型只属于 gateway web。
- 哪些命名或字段重复度高，应该收敛。

### 7.6 把依赖治理和测试矩阵前置

当前工作区很大，继续靠“全量跑一遍”会越来越重。建议明确：
- domain-based test matrix
- crate ownership
- git dependency 升级策略
- 稳定渠道与实验渠道的测试分层

## 8. 建议的执行顺序

建议按以下顺序推进：
1. 修正文档、命名、元数据漂移。
2. 产出架构边界与协议所有权清单。
3. 识别 `core` / `gateway-server` / 各入口的共享启动骨架。
4. 选一个网关领域做拆分试点。
5. 选一个 `core` 领域做模块化试点。
6. 补测试矩阵和依赖治理规则。

## 9. 最终判断

Savfox 的基础方向是对的，真正的问题不是缺能力，而是成功积累之后的结构压力已经开始显性化。

如果现在开始做边界治理，这个仓库完全有机会继续演进成一个稳定的、多入口、多协议、可扩展的 AI Agent 平台；如果继续只加功能不治理结构，未来的主要成本会从“开发功能”逐步转成“理解系统、避免回归、同步文档和修正边界”。
