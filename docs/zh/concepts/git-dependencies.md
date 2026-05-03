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
