# 测试矩阵

Savfox 工作区已经足够大，不能只靠“先跑整个 workspace”这一种策略。

## 分层

### 本地快速切片

开发时优先使用定向命令：
- `just test-core`
- `just test-protocol`
- `just test-tui`
- `just test-gateway`
- `just test-channels`
- `just test-web`

### 全量校验

在以下场景使用 `just test`：
- 修改了共享行为且影响面较大
- 准备发布
- 多个定向切片都通过后，做一次跨 crate 验证

### CI 分层

CI 现在有两层测试：
- `test-targeted`：基于路径过滤的 Ubuntu 定向测试
- `test`：Linux / macOS / Windows 上的 `cargo test --workspace`

## 领域映射

| 领域 | 主要命令 |
| ---- | -------- |
| Core/runtime | `just test-core` |
| Protocol/editor | `just test-protocol` |
| TUI | `just test-tui` |
| Gateway | `just test-gateway` |
| Channels only | `just test-channels` |
| Web | `just test-web` |
