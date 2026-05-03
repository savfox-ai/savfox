# Savfox 项目改进待办

## 状态

本文件中的事项已按当前仓库范围全部落地，以下保留为完成记录。

## 已完成

### 1. 修正文档与元数据漂移

- 状态：`done`
- 已完成内容：
  - 更新根 `Cargo.toml` 的 workspace 描述。
  - 更新 `crates/savfox/Cargo.toml` 描述。
  - 重写 `README.md` 中的定位、文档入口和 gateway 开发命令建议。
  - 重写 `docs/en/concepts/architecture.md`，移除旧命名和过时版本描述。
  - 新增 `docs/zh/concepts/architecture.md`，并修正 `docs/zh/getting-started.md` 的 Rust 版本要求。

### 2. 产出 crate 边界与依赖规则文档

- 状态：`done`
- 已完成内容：
  - 新增 `docs/en/concepts/crate-boundaries.md`
  - 新增 `docs/zh/concepts/crate-boundaries.md`

### 3. 做一次协议类型盘点

- 状态：`done`
- 已完成内容：
  - 新增 `docs/en/concepts/protocol-ownership.md`
  - 新增 `docs/zh/concepts/protocol-ownership.md`

### 4. 抽取共享 bootstrap/runtime 骨架

- 状态：`done`
- 已完成内容：
  - 在 `crates/common/src/service_runtime.rs` 中新增共享 helper：
    - `DEFAULT_CHANNEL_CAPACITY`
    - `env_filter_from_default`
    - `init_stderr_tracing`
    - `spawn_stdin_json_reader`
  - `savfox-mcp-server` 改为复用共享 tracing 与 stdin JSON 读取骨架。
  - `savfox-app-server` 改为复用共享 stdin JSON 读取骨架。
  - `savfox-gateway-server` 改为复用共享 env filter helper。

### 5. 对 `gateway-server` 做一个领域拆分试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/gateway-server/src/voice/mod.rs`
  - 将 `stt` / `talk_mode` / `tts_*` / `voice_*` 归入 voice 域。
  - 在根 `lib.rs` 继续保留兼容性 re-export，避免一次性打断调用点。

### 6. 对 `savfox-core` 做一个模块化试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/core/src/providers/mod.rs`
  - 将 provider/model runtime 相关模块聚合到 `providers` 域。
  - 在 `crates/core/src/lib.rs` 保留兼容性 re-export，控制根模块继续平铺扩张。

### 7. 建立分层测试矩阵

- 状态：`done`
- 已完成内容：
  - `Justfile` 新增：
    - `test-core`
    - `test-protocol`
    - `test-tui`
    - `test-gateway`
    - `test-channels`
    - `test-web`
  - `.github/workflows/ci.yml` 新增按领域路径过滤与 `test-targeted` 任务。
  - 新增中英文测试矩阵文档：
    - `docs/en/concepts/testing-matrix.md`
    - `docs/zh/concepts/testing-matrix.md`

### 8. 建立 git 依赖治理策略

- 状态：`done`
- 已完成内容：
  - 新增 `docs/en/concepts/git-dependencies.md`
  - 新增 `docs/zh/concepts/git-dependencies.md`

### 9. 标准化 channel adapter contract

- 状态：`done`
- 已完成内容：
  - 新增 `docs/en/channels/adapter-contract.md`
  - 新增 `docs/zh/channels/adapter-contract.md`

### 10. 梳理 web 构建与发布职责边界

- 状态：`done`
- 已完成内容：
  - 新增 `docs/en/gateway/web-build-release.md`
  - 新增 `docs/zh/gateway/web-build-release.md`
  - `README.md` 与 `docs/*/SUMMARY.md` 已补充入口。

### 11. 建立中英文公共文档同步机制

- 状态：`done`
- 已完成内容：
  - 新增 `docs/en/concepts/doc-sync.md`
  - 新增 `docs/zh/concepts/doc-sync.md`
  - 新增 `.github/pull_request_template.md`，加入双语同步与测试切片检查项。

## 结果

当前 `_todos.md` 中列出的 11 项改进均已完成落地。
## 第二轮结构收敛

以下项目在第一轮完成后继续追加，目的是把领域分组从单点试验推进到连续模式。

### 12. 对 `gateway-server` 做第二个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/gateway-server/src/web/mod.rs`
  - 将 `server` / `static_assets` / `webchat` / `ws` / `ws_rpc` 收敛到 `web` 域
  - 在根 `lib.rs` 保留兼容性 re-export，避免破坏现有调用路径

### 13. 对 `savfox-core` 做第二个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/core/src/prompting/mod.rs`
  - 将 `custom_prompts` / `instructions` / `personality_migration` / `project_doc` 收敛到 `prompting` 域
  - 在根 `lib.rs` 保留兼容性 re-export，避免破坏现有公开路径
## 第二轮结构收敛

以下项目在第一轮完成后继续追加，目的是把领域分组从单点试验推进到连续模式。

### 12. 对 `gateway-server` 做第二个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/gateway-server/src/web/mod.rs`
  - 将 `server` / `static_assets` / `webchat` / `ws` / `ws_rpc` 收敛到 `web` 域
  - 在根 `lib.rs` 保留兼容性 re-export，避免破坏现有调用路径

### 13. 对 `savfox-core` 做第二个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/core/src/prompting/mod.rs`
  - 将 `custom_prompts` / `instructions` / `personality_migration` / `project_doc` 收敛到 `prompting` 域
  - 在根 `lib.rs` 保留兼容性 re-export，避免破坏现有公开路径
## 第三轮结构收敛

以下项目在第二轮完成后继续追加，目的是把领域分组进一步推进到 gateway 安全域和 core 命令域。

### 14. 对 `gateway-server` 做第三个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/gateway-server/src/security/mod.rs`
  - 将 `auth` / `rate_limit` / `redaction` / `security_audit` / `ssrf` 收敛到 `security` 域
  - 在根 `lib.rs` 保留兼容性 re-export，避免破坏现有调用路径

### 15. 对 `savfox-core` 做第三个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/core/src/commands/mod.rs`
  - 将 `bash` / `command_safety` / `parse_command` / `powershell` / `shell` / `shell_snapshot` 收敛到 `commands` 域
  - 在根 `lib.rs` 保留兼容性 re-export，并保留 `command_safety` 的内部别名以兼容现有 crate 内引用
## 第四轮结构收敛

以下项目在第三轮完成后继续追加，目的是把 gateway 运行域与 core review 域继续收拢到单独命名空间。

### 16. 对 `gateway-server` 做第四个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/gateway-server/src/runtime/mod.rs`
  - 将 `agent_routing` / `routing` / `session` 收敛到 `runtime` 域
  - `channel` / `identity_links` / `message_queue` / `pairing_store` 在 `runtime` 域中复用根模块导出，避免重复定义同一文件
  - 在根 `lib.rs` 保留 `routing` / `session` 兼容性 re-export

### 17. 对 `savfox-core` 做第四个领域分组试点

- 状态：`done`
- 已完成内容：
  - 新增 `crates/core/src/reviewing/mod.rs`
  - 将 `review_format` / `review_prompts` / `turn_diff_tracker` 收敛到 `reviewing` 域
  - `transcript_policy` 在 `reviewing` 域中复用根模块导出，避免与现有根模块声明冲突
  - 在根 `lib.rs` 保留兼容性 re-export
