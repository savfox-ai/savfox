# Savfox 改进任务清单(本轮)

> 来源: [_report.md](./_report.md)。本轮只做"低风险、高确定性"的项,巨型文件拆分、unwrap 审计、mDNS 实现等结构性大项不在本轮范围。

## 任务总览

| # | 任务 | 状态 | 风险 |
| - | --- | --- | --- |
| T1 | 修复 workspace 内部 crate 版本号不一致 | ✅ done | 低 |
| T2 | 归档/清理过时的根目录 `_*.md` 计划文件 | ✅ done | 低 |
| T3 | 收敛 gateway-server lib.rs 的总 allow | ✅ done | 中 |
| T4 | 物理迁移领域分组的文件,删除 `#[path]` 别名 | ✅ done | 中 |
| T5 | 验证(cargo fmt + cargo check) | ✅ done | - |

## Follow-up(本轮不做,登记给后续)

- F1 — `chat_screen` approval modal 快捷键重复渲染回归(`*.snap.new` 显示 `(y) (y)`、`(a) (a)`、`(n) (esc)`)。本轮**不**接受 snapshot,需要单独排查 `bottom_pane/list_selection_view.rs` 的快捷键合成逻辑。
- F2 — 巨型文件拆分(`chat_screen.rs` 7368 行、`config/mod.rs` 4566 行、`pages/channels/mod.rs` 4513 行等)。
- F3 — 把 workspace git 依赖从 `branch = "..."` 全部改为 `rev = "..."`(eventsource-stream / opentelemetry / reqwest-eventsource / dingtalk-sdk)。
- F4 — `crates/gateway-server/src/discovery.rs` 的 mDNS stub 决策(实现 / 删除 / 标 deprecated)。
- F5 — 生产路径 `unwrap`/`expect` 审计(core ~420、gateway-server ~241)。

---

## T1 — 修复 workspace 版本号不一致

**问题**: [Cargo.toml:13](Cargo.toml) 的 `[workspace.package].version = "0.3.1"`,但 [Cargo.toml:29-76](Cargo.toml) 的 `[workspace.dependencies]` 中所有内部 crate 仍是 `version = "0.3.0"`。

**改动**: 把 `[workspace.dependencies]` 中 47 个内部 `savfox-*` / `app_test_support` / `core_test_support` / `exec_server_test_support` / `mcp_test_support` 的 `version = "0.3.0"` 改为 `"0.3.1"`。

**验证**: `cargo check --workspace` 不报版本错误。

---

## T2 — 归档过时的根目录计划文件

**问题**: 根目录积累 `_auto_reply.md`、`_auto_tasks.md`、`_todo.md`、`_todos.md`、`_todos_cli.md` 等历史 todo 文档,内容已 done,无归档机制。

**改动**:
1. 新建 `docs/_archive/` 目录(若不存在)。
2. 把以下文件移入 `docs/_archive/`:
   - `_auto_reply.md`
   - `_auto_tasks.md`
   - `_todo.md`
   - `_todos.md`
   - `_todos_cli.md`
3. 当前轮的 `_report.md` / `_tasks.md` 留在根目录(本轮工作进行中)。
4. 在 `.gitignore` 中**不要**屏蔽 `_*.md`,保持归档文件可见。

**验证**: `git status` 显示 5 个文件移动,根目录只剩本轮新写的 `_report.md` / `_tasks.md`。

---

## T3 — 收敛 gateway-server lib.rs 的总 allow

**问题**: [crates/gateway-server/src/lib.rs:2](crates/gateway-server/src/lib.rs#L2) 是 `#![allow(unreachable_pub, dead_code)]`,违反 workspace `unreachable_pub = "deny"`。

**改动**:
1. `dead_code` 从 allow 降为 warn(`#![warn(dead_code)]`),保留 19 处 clippy allows 不动。
2. 尝试移除 `unreachable_pub` 全局豁免;如果发现需要修的可见性数量过大(>20 处),则保留全局 allow 但加 `// TODO: 收敛 unreachable_pub` 注释。
3. 跑 `cargo check -p savfox-gateway-server` 确保不引入新错误。

**验证**: `cargo check -p savfox-gateway-server --lib` 通过。

> **降级策略**: 若 `unreachable_pub` 修复涉及大面积 visibility 变更,只把 `dead_code` 改成 warn,保留 `unreachable_pub` 暂时 allow 但加 TODO 注释。

---

## T4 — 物理迁移领域分组的文件,删除 `#[path]` 别名

**问题**: 上一轮领域分组用 `#[path = "../X.rs"]` 把根目录文件挂到子目录 `mod.rs`,文件物理位置没动。

**改动**: 对每个域执行"git mv 文件 → 改写 mod.rs 删除 `#[path]` → 验证"。

### 4.1 `crates/gateway-server/src/voice/`
- `stt.rs` → `voice/stt.rs`
- `tts_deepgram.rs` → `voice/tts_deepgram.rs`
- `tts_edge.rs` → `voice/tts_edge.rs`
- `tts_service.rs` → `voice/tts_service.rs`
- `voice_store.rs` → `voice/voice_store.rs`
- `talk_mode/` → `voice/talk_mode/`
- `voice_wake/` → `voice/voice_wake/`

### 4.2 `crates/gateway-server/src/security/`
- `auth/` → `security/auth/`
- `rate_limit.rs` → `security/rate_limit.rs`
- `redaction.rs` → `security/redaction.rs`
- `security_audit.rs` → `security/security_audit.rs`
- `ssrf.rs` → `security/ssrf.rs`

### 4.3 `crates/gateway-server/src/runtime/`
- `agent_routing.rs` → `runtime/agent_routing.rs`
- `routing/` → `runtime/routing/`
- `session/` → `runtime/session/`

`channel`、`identity_links`、`message_queue`、`pairing_store` 仍在根 lib.rs 声明(它们在 runtime/mod.rs 是 `pub use crate::*` 而非真正搬入,不动)。

### 4.4 `crates/core/src/commands/`
- `bash.rs` → `commands/bash.rs`
- `parse_command.rs` → `commands/parse_command.rs`
- `powershell.rs` → `commands/powershell.rs`
- `shell.rs` → `commands/shell.rs`
- `shell_snapshot.rs` → `commands/shell_snapshot.rs`
- `command_safety/` → `commands/safety/`(目录移动 + 模块改名)

### 4.5 `crates/core/src/providers/`
- `model_fallback.rs` → `providers/fallback.rs`
- `model_identifiers.rs` → `providers/identifiers.rs`
- `model_provider_info.rs` → `providers/info.rs`
- `models_manager/` → `providers/manager/`
- `remote_models.rs` → `providers/remote.rs`

### 4.6 `crates/core/src/prompting/`
- `custom_prompts.rs` → `prompting/custom_prompts.rs`
- `instructions/` → `prompting/instructions/`
- `personality_migration.rs` → `prompting/personality_migration.rs`
- `project_doc.rs` → `prompting/project_doc.rs`

### 4.7 `crates/core/src/reviewing/`
- `review_format.rs` → `reviewing/review_format.rs`
- `review_prompts.rs` → `reviewing/review_prompts.rs`
- `turn_diff_tracker.rs` → `reviewing/turn_diff_tracker.rs`

### 4.8 lib.rs 兼容 re-export 处理

**保留**所有现有 `pub use` re-export 不动——外部 crate 仍依赖 `savfox_core::bash` 这种旧路径。本轮只做物理迁移,不改 API 表面。

**验证**:
1. `cargo check --workspace` 全部通过
2. `cargo fmt --all`
3. 抽样 grep 确认根目录不再有迁移走的文件名

---

## T5 — 验证

```bash
cargo fmt --all
cargo check --workspace --all-targets
```

如果有 warning 数量增加(由 T3 的 allow 收敛产生),记录在 `_report.md` 的"完成情况"小节。
