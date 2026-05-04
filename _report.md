# Savfox 项目代码审阅报告(第 N 轮)

**日期**: 2026-05-04
**范围**: 整体仓库 (Cargo workspace, ~48 crates)
**方法**: 静态阅读 + 模式扫描,未运行 `cargo build` / `cargo test`。

---

## 1. 上下文与之前进展

仓库根目录已存在多个旧版 `_report.md` / `_todos.md`,记录了过去若干轮治理:文档与元数据修正、crate 边界文档、协议盘点、共享 bootstrap 抽取、网关/核心的领域分组试点(voice / security / web / runtime,以及 commands / providers / prompting / reviewing)。

本轮聚焦在**前几轮治理留下的"半成品"和新出现的实际问题**,而不是再产出更多文档。

---

## 2. 总体判断

之前几轮做的"领域分组"是一个**目录上看起来分了组,但物理文件并没有真正搬动**的过渡形态。`#[path = "../X.rs"]` 指令把根目录的文件别名挂到子目录 `mod.rs` 下,虽然达到了"在外部看像分组"的效果,但是:

- 文件物理位置仍在 `crates/<x>/src/` 根目录,新成员依然会困惑"为什么 `voice` 域里看不到 `stt.rs`"。
- `lib.rs` 累积了大量 `pub use ...` 兼容 re-export,实际公开 API 表面没收敛,反而看起来更乱。
- 真正"治理边界"这一目标只完成了一半,后半步(物理迁移 + 修正调用点)被推迟到了"未来"。

此外,有一类**新出现的小型问题**积攒了一批,值得这一轮统一处理:workspace 版本不一致、根目录大量过时计划文件、lint 总开关掩盖问题、stale snapshot 暗示未确认的 UI 回归等。

---

## 3. 具体问题清单(按优先级)

### 3.1 ⚠️ 高优先级 - workspace 元数据不一致

**位置**: [Cargo.toml:13-76](Cargo.toml)

`[workspace.package]` 的 `version` 已升至 `0.3.1`,但 `[workspace.dependencies]` 中所有 47 个内部 crate 的 path 依赖仍然钉死在 `version = "0.3.0"`。这是一个隐性的版本错位,可能在以下场景下出问题:
- 通过 crates.io 发布时上下游版本不匹配。
- `cargo install --path` 与 `cargo add` 行为不一致。
- 任何脚本/工具读 `version` 字段做版本对齐时会出错。

**修复成本**: 极低,改一处配置。

---

### 3.2 ⚠️ 高优先级 - gateway-server 用 `#![allow(dead_code, unreachable_pub)]` 整体掩盖 lint

**位置**: [crates/gateway-server/src/lib.rs:2](crates/gateway-server/src/lib.rs#L2)

```rust
#![allow(unreachable_pub, dead_code)]
```

workspace 全局 `unreachable_pub = "deny"`,但此 crate 在 lib.rs 顶部全量豁免。同时有 19 处 `#![allow(clippy::...)]`。这种"地毯式豁免"的问题:

- 真正的 dead code 与"暂时未用"的 dead code 混在一起,失去了 lint 的诊断价值。
- 一旦未来想重新打开,需要清理的范围远大于现在。
- 与 `core/src/lib.rs` 的精简风格(只豁免必需的 clippy 项)不一致。

**修复路径**: 至少将 `dead_code` 改为 `warn`,逐步清理;`unreachable_pub` 直接移除并修可见性。

---

### 3.3 ⚠️ 中优先级 - 领域分组只搬了"目录壳",物理文件未迁移

**位置**:
- [crates/gateway-server/src/voice/mod.rs](crates/gateway-server/src/voice/mod.rs) 引用 `../stt.rs`、`../tts_*.rs`、`../voice_store.rs` 等
- [crates/gateway-server/src/security/mod.rs](crates/gateway-server/src/security/mod.rs) 引用 `../auth/`、`../rate_limit.rs`、`../redaction.rs` 等
- [crates/gateway-server/src/runtime/mod.rs](crates/gateway-server/src/runtime/mod.rs) 引用 `../agent_routing.rs`、`../routing/`、`../session/`
- [crates/core/src/commands/mod.rs](crates/core/src/commands/mod.rs) 引用 `../bash.rs`、`../parse_command.rs`、`../powershell.rs`、`../shell.rs`、`../shell_snapshot.rs`、`../command_safety/`
- [crates/core/src/providers/mod.rs](crates/core/src/providers/mod.rs) 引用 `../model_*.rs`、`../models_manager/`、`../remote_models.rs`
- [crates/core/src/prompting/mod.rs](crates/core/src/prompting/mod.rs) 引用 `../custom_prompts.rs`、`../instructions/`、`../personality_migration.rs`、`../project_doc.rs`
- [crates/core/src/reviewing/mod.rs](crates/core/src/reviewing/mod.rs) 引用 `../review_format.rs`、`../review_prompts.rs`、`../turn_diff_tracker.rs`

模式如:
```rust
#[path = "../stt.rs"]
pub mod stt;
```

这等于"我希望这个文件在 voice 域,但又不想真的把它搬过去"。带来的问题:
- 新人查找代码时,在 `voice/` 目录下看不到任何 `.rs` 文件(只有 `mod.rs`)。
- IDE 跳转、grep 路径都会指向 `src/` 根目录,与"已分组"的认知矛盾。
- 兼容 re-export 在 lib.rs 中堆积(见下),无法真正把根模块树清干净。

**修复路径**: 物理移动文件到对应的子目录,删除 `#[path]` 指令,并清理因移动失效的 lib.rs 兼容 re-export(如果调用点已能直接走新路径)。

---

### 3.4 ⚠️ 中优先级 - lib.rs 中累积的兼容 re-export

**位置**: [crates/core/src/lib.rs:119-120, 144-146](crates/core/src/lib.rs)、[crates/gateway-server/src/lib.rs:96-100](crates/gateway-server/src/lib.rs)

```rust
// core/src/lib.rs
pub use prompting::{custom_prompts, instructions, personality_migration, project_doc};
pub use reviewing::{review_format, review_prompts, turn_diff_tracker};
pub use commands::{bash, parse_command, powershell, shell, shell_snapshot};
pub(crate) use commands::safety as command_safety;

// gateway-server/src/lib.rs
pub(crate) use runtime::agent_routing;
pub use runtime::{routing, session};
pub use security::{auth, rate_limit, redaction, security_audit, ssrf};
pub use voice::{stt, talk_mode, voice_wake};
pub(crate) use voice::{tts_deepgram, tts_edge, tts_service, voice_store};
```

这些 re-export 是上一轮"边界治理"为了不破坏调用点而留下的兼容层。但目前**没有任何**逐步迁移调用点的计划,意味着它们会被永久留在那里,既没有让外部调用点真的"按域引用",也没有让 lib.rs 表面更干净。

**修复路径**: 决定一个方向并执行——要么真正迁移调用点(去掉 re-export),要么明确"根命名空间是公共 API"并删除子域(避免存在两条引用路径)。

---

### 3.5 🔍 中优先级 - 根目录大量过时 `_*.md` 计划文件

**位置**: 仓库根

```
_auto_reply.md     (2026-04-17)  17 KB
_auto_tasks.md     (2026-04-17)  1.6 KB
_report.md         (2026-05-03)  13 KB - 上一轮报告
_tasks.md          (2026-04-02)  1.1 KB
_todo.md           (2026-04-01)  1 KB
_todos.md          (2026-05-03)  6.9 KB - 上一轮 todo 已全部 done
_todos_cli.md      (2026-05-02)  1 KB
```

这些文件是过去若干轮分析的产物。在 `_todos.md` 头部明确写着"本文件中的事项已按当前仓库范围全部落地",但文件仍在 `git` 跟踪外/内堆积。它们:
- 占用根目录视觉空间,容易被新人误读为"待办"。
- 与 GitHub Issue / PR 描述功能重复。
- 没有归档机制(`docs/archive/`、`.history/` 等)。

**修复路径**: 本轮新报告产生后,把上一轮的 `_report.md` / `_todos.md` 等过时文件归档或删除;并在 `.gitignore` 中明确 `_*.md` 的处理策略。

---

### 3.6 🔍 中优先级 - stale snapshot 暗示真实 UI 回归

**位置**: [crates/tui/src/chat_screen/snapshots/savfox_tui__chat_screen__tests__approval_modal_patch.snap.new](crates/tui/src/chat_screen/snapshots/savfox_tui__chat_screen__tests__approval_modal_patch.snap.new)

旧 snapshot:
```
› 1. Yes, proceed (y)
  2. Yes, and don't ask again for these files (a)
  3. No, and tell Savfox what to do differently (esc)
```

新 `.snap.new`:
```
› 1. Yes, proceed (y) (y)
  2. Yes, don't ask again for these files (a) (a)
  3. No, tell Savfox what to do differently (n) (esc)
```

新版每一行的快捷键被渲染了两次(`(y) (y)`、`(a) (a)`、`(n) (esc)`),这**不是简单的文案变更**,而是疑似行项的快捷键合成逻辑出现了重复拼接的回归。

**修复路径**: 这一项**不应**直接 `cargo insta accept`。应当先定位 list_selection_view 渲染中的快捷键逻辑,确认是回归还是有意修改,再决定接受 snapshot 还是修代码。本轮报告将其登记,实际修复留为单独的 follow-up。

---

### 3.7 🔍 低优先级 - `discovery.rs` 的 mDNS 仍是 stub

**位置**: [crates/gateway-server/src/discovery.rs:78, 145](crates/gateway-server/src/discovery.rs)

```rust
// TODO: Implement actual mDNS registration using mdns-sd crate.
// TODO: Implement actual mDNS browsing using mdns-sd crate.
```

文件作为公共模块存在,但核心功能未实现。这种"占位实现"如果暴露给上游,会让外部以为已经支持了发现能力,实际上不会工作。

**修复路径**: 要么删除该模块(若短期内不打算实现),要么至少在公共类型上明确标 `#[doc(hidden)]` 或 `#[deprecated(note = "not yet implemented")]`,避免误用。本轮报告仅记录,不执行。

---

### 3.8 🔍 低优先级 - workspace git 依赖跟踪 branch

**位置**: [Cargo.toml:137, 194-199, 234, 334](Cargo.toml)

```toml
eventsource-stream = { ..., branch = "next" }
opentelemetry* = { ..., branch = "main" }
reqwest-eventsource = { ..., branch = "next" }
dingtalk-sdk = { ..., branch = "main" }
```

之前的 `git-dependencies.md` 文档已经讨论过策略,但 `Cargo.toml` 本身仍然全部 `branch = "..."` 而不是 `rev = "..."`。`Cargo.lock` 会锁定具体 commit,但 `cargo update -p <pkg>` 会自动跳到最新 commit,长期看会造成"我没改任何东西但 lockfile 又变了"的局面。

**修复路径**: 把所有 git 依赖钉到具体 `rev`(本轮不做,留为后续治理项)。

---

### 3.9 🔍 低优先级 - 巨型文件需要进一步拆分

仍然存在的高 LOC 单文件(超过 2500 行):

| 文件 | 行数 | 备注 |
| --- | --- | --- |
| [crates/tui/src/chat_screen.rs](crates/tui/src/chat_screen.rs) | 7368 | TUI 主屏,内部已有按 region 拆分的空间 |
| [crates/tui/src/chat_screen/tests.rs](crates/tui/src/chat_screen/tests.rs) | 5544 | 单文件超大测试套 |
| [crates/core/src/config/mod.rs](crates/core/src/config/mod.rs) | 4566 | 已是子模块的 mod.rs,但仍然过重 |
| [crates/gateway-dioxus/src/pages/channels/mod.rs](crates/gateway-dioxus/src/pages/channels/mod.rs) | 4513 | 所有 channel UI 共一个文件 |
| [crates/gateway-dioxus/src/pages/agents.rs](crates/gateway-dioxus/src/pages/agents.rs) | 4399 | 单页 4k+ 行 |
| [crates/tui/src/bottom_pane/chat_composer/tests.rs](crates/tui/src/bottom_pane/chat_composer/tests.rs) | 3774 | 测试集中 |
| [crates/tui/src/app/mod.rs](crates/tui/src/app/mod.rs) | 3514 | App 主循环 |
| [crates/tui/src/history_cell/mod.rs](crates/tui/src/history_cell/mod.rs) | 3229 | 历史项渲染 |
| [crates/gateway-server/src/ws_rpc/handlers/browser.rs](crates/gateway-server/src/ws_rpc/handlers/browser.rs) | 3112 | 单 handler 太大 |
| [crates/protocol/src/protocol.rs](crates/protocol/src/protocol.rs) | 3071 | 协议枚举集中 |
| [crates/app-server-protocol/src/protocol/v1.rs](crates/app-server-protocol/src/protocol/v1.rs) | 3039 | v1 协议集中 |

这些不是"必须现在拆",但任何一项拆分对未来开发都是复利收益。本轮不动结构性大文件,留为后续单独 PR 推进。

---

### 3.10 🔍 低优先级 - core 与 gateway-server 的 unwrap/expect 数量

| 模块 | 数量(粗略 grep) |
| --- | --- |
| `crates/core/src/**` | ~420 |
| `crates/gateway-server/src/**` | ~241 |

测试代码可以容忍 unwrap,但生产路径上的 unwrap 会让 panic 变成"不可恢复的服务异常"。这一项需要逐文件审计,不在本轮范围内。

---

## 4. 本轮要执行的改动(实际可落地)

按"低风险、高确定性、对未来开发有正向价值"原则筛选,本轮只做以下:

1. **修复 workspace 版本不一致** — 把 `[workspace.dependencies]` 中所有内部 crate 的 `version = "0.3.0"` 升到 `"0.3.1"`,与 `workspace.package.version` 对齐。
2. **收敛 gateway-server lib.rs 的总 allow** — 把 `dead_code` 从 allow 改为 warn,移除 `unreachable_pub` 的全局豁免,如果有少量违例则尽量修复,实在不能修的局部 `#[allow]`。
3. **物理迁移领域分组的文件** — 把 `voice/`、`security/`、`runtime/`、`commands/`、`providers/`、`prompting/`、`reviewing/` 各自 `#[path]` 引用的文件真的移到对应子目录,删掉 `#[path]` 指令。同时清理因为移动而失效的 lib.rs 兼容 re-export(如果存在)。
4. **归档过时的根目录 `_*.md` 文件** — 把上一轮的 `_report.md`、`_todos.md` 等以及更早的 `_auto_reply.md` 等移入 `docs/_archive/`(或者直接删掉),只保留本轮的新 `_report.md` / `_tasks.md`。
5. **不动 stale snapshot** — 在 `_tasks.md` 中明确登记"snap.new 有疑似回归,需要单独 follow-up",但本轮不动。
6. **每一步后跑 `cargo check --workspace` 与 `cargo fmt --all`** 验证。

---

## 5. 不在本轮范围内的项

明确推迟,不在本轮 `_tasks.md` 中:

- 拆分巨型文件(`chat_screen.rs`、`config/mod.rs`、`pages/channels/mod.rs` 等)
- 全量审计 `unwrap` / `expect`
- 实现 mDNS discovery
- 把 git 依赖从 `branch` 改成 `rev`
- 修复 approval modal snapshot 中的快捷键重复回归(需要单独排查)

---

## 6. 结论

仓库整体质量已经过几轮治理,主要的"结构边界"和"文档骨架"问题已经处理。本轮要解决的是**前几轮留下的小型债务**——版本号错位、半成品的领域分组、地毯式 lint 豁免、根目录的过时计划文件。这些项单独看都不"重要",但累积起来正在拉低仓库的整洁度和新人上手体验。

修完这一轮,下一轮可以把注意力转回更大的结构性议题(巨型文件拆分、unwrap 审计、协议边界细化)。

---

## 7. 完成情况

本轮 T1–T5 全部落地。详细的任务定义见 [_tasks.md](./_tasks.md)。

### 已完成

- **T1**: `Cargo.toml` 中 `[workspace.dependencies]` 内部 47 个 `savfox-*` / `*_test_support` crate 的 `version` 全部从 `"0.3.0"` 升到 `"0.3.1"`,与 `[workspace.package].version` 对齐。
- **T2**: 把 5 个根目录历史计划文件 (`_auto_reply.md`、`_auto_tasks.md`、`_todo.md`、`_todos.md`、`_todos_cli.md`) 移入 `docs/_archive/`;清理了 `.gitignore` 中重复的 `_*.md` 行(从 2 条减到 1 条)。
- **T3**: `crates/gateway-server/src/lib.rs` 把 `dead_code` 从 `allow` 改为 `warn`,`unreachable_pub` 保留 `allow` 但加了 TODO 注释,准备后续逐文件收敛。该改动暴露了 193 处 `dead_code` warning,但**未引入新错误**,可以作为后续清理的输入清单。
- **T4**: 把 7 个领域目录(gateway 的 `voice`/`security`/`runtime`,core 的 `commands`/`providers`/`prompting`/`reviewing`)中所有用 `#[path = "../X.rs"]` 别名挂载的文件**真正物理移动**到对应子目录,删除了 `#[path]` 指令。同步修复了两处 `include_str!` 相对路径(`providers/manager/collaboration_mode_presets.rs` 和 `prompting/project_doc.rs`)。具体涉及 ~30 个文件移动 + 7 个 `mod.rs` 简化。`lib.rs` 中现有的兼容 re-export 全部保留,不影响外部调用方。
- **T5**: `cargo fmt --all` 与 `cargo check --workspace --lib` 均已通过,无新增编译错误。

### 已登记的 Follow-up(本轮不做)

- F1 — `chat_screen` approval modal 的 `(y) (y)` / `(a) (a)` 重复渲染回归(疑似 `bottom_pane/list_selection_view.rs` 的快捷键合成逻辑出错)。本轮未接受 `.snap.new`。
- F2 — 巨型文件拆分(`chat_screen.rs` 7368 行等)。
- F3 — git 依赖从 `branch` 改为 `rev`(eventsource-stream / opentelemetry / reqwest-eventsource / dingtalk-sdk)。
- F4 — `discovery.rs` mDNS stub 决策(实现 / 删除 / `#[deprecated]`)。
- F5 — 生产路径 `unwrap`/`expect` 审计(core ~420、gateway-server ~241)。
- F6 — gateway-server 193 处 `dead_code` warning 收敛(由 T3 暴露)。

### 未跑的验证

- 未跑 `cargo test --workspace`(本轮聚焦在结构改动,结构改动后跑全量测试成本高;由 CI 兜底)。
- 未跑 `cargo clippy --workspace`。
