# Git 依赖策略

Savfox 当前确实保留了一小部分 git 依赖与 git patch，这件事必须被治理，而不是默认扩散。

## 为什么存在 git 依赖

当前常见原因包括：
- crates.io 还没有所需修复
- 某些协议或传输相关 fork 需要与 Savfox 行为保持一致
- 需要 pin 到明确 revision 以保证兼容性

OpenTelemetry 已使用 crates.io 正式版本，原先的 Git patches 已移除。

## 每个 git 依赖都应记录

1. 为什么 crates.io 版本不够用
2. 它是短期过渡还是长期 fork
3. 上游什么版本、commit 或 issue 可以让它被移除
4. 谁负责升级和故障响应

## 优先级顺序

优先选择：
1. crates.io release
2. 精确 git revision
3. branch 跟踪
4. 浮动上游 `main`，仅在确实无法避免时使用

## 依赖升级约束（2026-10-30）

锁文件采用 crates.io 的 rmcp 3.8.0、OpenTelemetry 系列 0.33.1 和
tracing-opentelemetry 0.34.0，原先的六项 OpenTelemetry Git overrides 已移除。
升级保留以下依赖约束：

| 保留版本 | 约束 |
| --- | --- |
| starlark 0.13.0、allocative 0.3.4、allocative_derive 0.3.3 | 保留兼容的 Starlark/allocative 组合。Starlark 0.14.2 精确要求 blake3 1.8.2，与 Arkret 至少 1.8.7 的要求冲突。Allocative 0.3.4 精确锁定 derive crate。 |
| time 0.3.47、time-core 0.1.8、time-macros 0.2.27 | rama-crypto 0.4.0 精确锁定 time，time 再精确锁定配套 crate。 |
| generic-array 0.14.7 | crypto-common 0.1.7 精确锁定该版本，单独升级无法解析。 |
| pest/pest_derive/pest_generator/pest_meta 2.9.2 | 2.9.5 系列新增 pest_stack_guard 1.0.0，后者精确要求 psm 0.1.24；锁文件保留 psm 0.1.32，单独升级 pest 无法解析。 |

这些版本均未被 yank，也不是被 workspace 的 Rust 版本阻塞。通过降级
crypto-common 或 psm 升级 generic-array 或 pest，会把一项旧依赖换成另一项。
本次升级不声称每项传递依赖都已使用最新发布版本或最新主版本。

## Matrix Appservice 安全维护 fork

`matrix-bot-sdk` 0.2.4 仍依赖 Salvo 0.89，传递引入存在漏洞的 zstd 0.13。
Savfox 采用 Salvo 1.0.1，并将维护 fork
`https://github.com/arkret-org/matrix-bot-sdk` 固定到
`d079403dd100af26d2fe7c5ea83dffa7c547cc5a`（`arkret/salvo1-security`）。
维护仓仅导入 crates.io 已公开的 0.2.4 源码包，保留 Apache-2.0 许可证，
不包含上游私有 Git 历史。Appservice 和客户端功能均保留。

Savfox 集成维护者负责依赖升级和兼容性回归。上游发布支持当前维护版 Salvo、
且不再引入受影响压缩依赖的版本后，运行 Matrix Appservice 路由、客户端和
Gateway 集成检查，再切回 crates.io。
