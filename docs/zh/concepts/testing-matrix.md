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
- `tooling-tests`：安装器隔离测试、Web bundle 完整性和定向包映射检查。
- `clippy`：Ubuntu workspace lint 检查，启用 `--all-targets --all-features`，覆盖可选集成。
- `audit`：`cargo audit --deny warnings` 对新增公告警告和漏洞均失败；经评审的维护告警精确例外保留在 `.cargo/audit.toml`。
- `test-targeted`：基于路径过滤的 Ubuntu 定向测试。Gateway/shared/channel 分别运行默认 features 和 `--all-features` 测试，覆盖 Arkret；可选 feature 测试与默认测试使用相同的 Gateway/channel/Web/workspace 变更条件。
- `test`：Ubuntu 全 workspace nextest 加 doctest，用于 workspace 级变更、CI workflow 变更、未纳入定向领域的 crate、手动触发和定时触发。新增 crate 在明确归入定向领域前自动回退到这一层。
- `test-cross-platform`：Windows 和 macOS 全 workspace nextest，仅手动触发或定时触发。

普通 PR 不会自动运行完整的 Windows/macOS 矩阵，除非手动触发 workflow。

## 领域映射

| 领域 | 主要命令 | 范围 |
| ---- | -------- | ---- |
| Core/runtime | `just test-core` | `savfox-core`、config/model/http/api runtime crates；CI 另运行 `savfox-common` |
| Protocol/editor | `just test-protocol` | `savfox-protocol`、`savfox-app-server-protocol`、`savfox-app-server`、`savfox-mcp-server` |
| TUI | `just test-tui` | `savfox-tui`；CI 另运行 `savfox-ansi-escape` |
| Gateway | `just test-gateway` | `savfox-gateway-server`、`savfox-gateway-shared`、`savfox-channels` |
| Channels only | `just test-channels` | `savfox-channels` |
| Web | `just test-web` | Glory frontend build |

## 选择规则

`node scripts/check-ci-test-matrix.cjs` 比较 workspace 包元数据、CI 全仓排除名单
与定向测试命令。每个被排除的 crate 都必须拥有明确的定向测试包；作为依赖参与
编译不会运行该包自身的单测。Web 原生单测与 WASM 构建共同运行。
检查器也要求审计拒绝 warnings、全 feature workspace Clippy，以及每个提供 `arkret` feature 的
Gateway/shared/channel 包明确运行全 feature 测试。这些可选检查在 Ubuntu 运行；
Windows 专属 keystore 依赖仍受 Cargo 的目标平台条件约束。它们补充默认 feature
测试，不替代默认测试或跨平台测试。

Gateway 回归 fixture 会调用生产路由、dispatcher、WebSocket 握手、配置校验器
和持久化会话存储，并在行为不符合预期时失败。仅构造 JSON 的示例不计为集成覆盖。
显式运行被忽略的真实 Gateway E2E 时，必须设置 `E2E_GATEWAY_URL` 和
`E2E_GATEWAY_TOKEN`，缺少运行环境会使测试失败。

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
