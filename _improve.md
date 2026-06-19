# 代码审查与改进记录

日期：2026-06-20

## 本轮已处理

- [x] `crates/core/src/tools/handlers/web_fetch.rs`：`web_fetch` 的 SSRF 校验存在 DNS TOCTOU 风险。代码先用 `tokio::net::lookup_host` 校验目标地址，实际请求时 reqwest 会再次解析；DNS rebinding 可能让校验和连接命中不同地址。已改为每一跳重定向都复检，并用 reqwest `resolve_to_addrs` 固定到已校验地址，同时禁用环境代理参与该 fetch。
- [x] `crates/core/src/tools/handlers/web_fetch.rs`：缓存 key 之前会把完整 URL 转小写，导致大小写敏感的 path/query 发生缓存串用，例如 `/Readme` 和 `/readme`。已改为只 trim，不改写大小写，并补充单测。
- [x] `crates/core/src/tools/handlers/web_fetch.rs`：重定向后返回内容仍用初始 URL 包装，来源标识不准确。已改为用最终 URL 包装返回内容。
- [x] `crates/core/src/tools/handlers/telegram_actions.rs`：Telegram bot token 位于 URL 路径中，reqwest 错误文本可能带出完整 URL。已在请求/读取错误回传中使用 `without_url()`，并对 `getChat` 的 `chat_id` query 参数做 URL 编码。
- [x] `crates/core/src/tools/handlers/telegram_actions.rs`：参数结构里保留了 spec 没暴露、实现也不读取的字段，并用 `allow(dead_code)` 掩盖。已移除死字段和 allow。
- [x] `crates/core/src/tools/handlers/nodes.rs`、`crates/core/src/tools/spec.rs`：`nodes` 工具只是占位，却向模型暴露 `run_command`、`camera_capture`、`get_location`、`send_notification` 等未接入能力，并保留未使用字段。已收窄 tool spec，只声明当前 discovery/status 占位能力，并移除死字段。

## 待继续处理

- [x] `crates/core/src/tools/handlers/discord_actions.rs`、`crates/core/src/tools/handlers/slack_actions.rs`：多个 path/query 参数直接拼进 URL。已对 Discord path segment 和 Slack history query 参数做 percent-encoding，并对列表/history `limit` 做上限 clamp。
- [x] `crates/core/src/tools/handlers/discord_actions.rs`、`crates/core/src/tools/handlers/slack_actions.rs`、`crates/core/src/tools/handlers/whatsapp_actions.rs`：第三方 API 错误响应体会原样回传给模型。已增加共享错误清洗 helper，去掉 reqwest 错误 URL，截断第三方错误体，并对已知 token/API URL 做替换脱敏。
- [x] `crates/core/src/tools/handlers/web_fetch.rs`：仍可进一步限制可接受 content-type。已拒绝明显二进制 media type，仅允许空类型、`text/*`、JSON/XML/JS/SVG/RSS/Atom 等文本型响应。
- [x] `crates/gateway-server/src/ws_rpc/handlers/*.rs`：多个 handler 文件使用 `#![allow(unused_imports)]`。已移除这些 allow，仅保留必要的 `clippy::module_inception` allow，并清理 `model.rs` 中实际未使用的导入。
- [x] 全仓库格式基线：`cargo fmt --all -- --check` 当前会在 `crates/gateway-dioxus`、`crates/gateway-server` 的既有文件上报格式差异。已运行 `cargo fmt --all` 吸收基线格式差异。

## 验证

- [x] `cargo test -p savfox-core web_fetch --lib`
- [x] `cargo check -p savfox-core`
- [x] `cargo fmt -p savfox-core -- --check`
- [x] `cargo check -p savfox-gateway-server`
- [x] `cargo fmt --all`
- [x] `cargo fmt --all -- --check`
