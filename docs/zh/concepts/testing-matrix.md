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

CI 现在有这些校验层：
- `fmt`：nightly rustfmt，始终运行。
- `test-targeted`：基于路径过滤的 Ubuntu 定向测试。
- `test`：Ubuntu 全 workspace nextest 加 doctest，用于 workspace 级变更、手动触发和定时触发。
- `test-cross-platform`：Windows 和 macOS 全 workspace nextest，仅手动触发或定时触发。

普通 PR 不会自动运行完整的 Windows/macOS 矩阵，除非手动触发 workflow。

## 领域映射

| 领域 | 主要命令 | 范围 |
| ---- | -------- | ---- |
| Core/runtime | `just test-core` | `savfox-core`、config/model/http/api runtime crates |
| Protocol/editor | `just test-protocol` | `savfox-protocol`、`savfox-app-server-protocol`、`savfox-app-server`、`savfox-mcp-server` |
| TUI | `just test-tui` | `savfox-tui` |
| Gateway | `just test-gateway` | `savfox-gateway-server`、`savfox-gateway-shared`、`savfox-channels` |
| Channels only | `just test-channels` | `savfox-channels` |
| Web | `just test-web` | Glory frontend build |

## 选择规则

选择能完整覆盖本次行为变更的最小切片。如果变更跨越共享边界，就运行多个切片。

## RPC 延迟基线

运行 `bash scripts/bench-rpc.sh`，脚本会启动隔离的本地 gateway。预热 10 次后，
默认测量 200 次串行 `status` 往返，不调用模型。可通过
`SAVFOX_RPC_BENCH_SAMPLES` 设置大于零的样本数。JSON 报告包含吞吐量和
p50/p95/p99/最大延迟，默认写入 `$SAVFOX_HOME/rpc-benchmark.json`（未设置时为
`.e2e-savfox-home/rpc-benchmark.json`），也可通过 `SAVFOX_RPC_BENCH_OUTPUT`
指定已有目录中的文件。缺少运行环境、目标测试不存在或样本数为零都会失败。
比较报告时应保持构建模式和机器一致；这是延迟基线，不是负载测试。

## Review 预期

PR 应说明选择了哪些测试切片。即使贡献者有意跳过本地执行并交给 CI 兜底，也应在 PR 中写清楚。
