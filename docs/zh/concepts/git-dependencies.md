# Git 依赖策略

Savfox 当前确实保留了一小部分 git 依赖与 git patch，这件事必须被治理，而不是默认扩散。

## 为什么存在 git 依赖

当前常见原因包括：
- crates.io 还没有所需修复
- 某些协议或传输相关 fork 需要与 Savfox 行为保持一致
- 需要 pin 到明确 revision 以保证兼容性

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
