# 当前代码审阅与改进报告

审阅范围：当前工作区的 Contrix channel / gateway 接入改动，以及其与通用 channel 发送链路的连接点。

排除项：用户已确认 `contrix` 通过仓库外本地 `path` 依赖接入是允许的，因此本报告不把该点列为问题。

## 已完成改进

- [x] **C-01 Contrix Applet 入站路由存在鉴权绕过**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`
  - 问题：`/appservices/contrix/{config_id}/...` 只要 `config_id` 匹配就放行；直接 `/api/v1/applet/...` 在存在单个 applet 时会无 token 回退到该 applet。这样外部请求可绕过 bearer 校验触发 ping/describe/transaction 等接口。
  - 改进：移除单 applet fallback；config-scoped 和 direct 路由都要求 `Authorization: Bearer <token>` 匹配已配置 applet token。

- [x] **C-02 Bearer token 解析泄漏内存且比较方式不合适**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`
  - 问题：原实现通过 `String::leak` 返回请求 token 引用，每个请求都会泄漏；token 用普通字符串比较。
  - 改进：改为返回 owned `String`，增加 `parse_bearer_header`，并使用 `subtle::ConstantTimeEq` 做 token 比较。

- [x] **C-03 Applet transactions 缺失 `Idempotency-Key` 时被静默伪造**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`
  - 问题：缺 header 时用当前时间生成 `anon-*` key，破坏调用方重试幂等语义，也让服务端无法发现协议错误。
  - 改进：缺少 `Idempotency-Key` 直接返回 `400 missing_idempotency_key`。

- [x] **C-04 Applet config 未强制配置入站鉴权 token**
  - 位置：`crates/channels/src/contrix/applet/config.rs`
  - 问题：Applet 配置可以通过 validate 但没有可用于入站 HTTP 鉴权的 token，导致只能依赖不安全 fallback。
  - 改进：`ContrixAppletConfig::validate()` 要求 `accessToken` / `contrixBearerToken` 非空，并新增配置单测。

- [x] **C-05 Contrix account-mode 回复未接入 gateway 发送链路**
  - 位置：`crates/gateway-server/src/channel/credential_manager.rs`、`crates/gateway-server/src/channels/contrix.rs`
  - 问题：agent 回复的 channel 会变成 `contrix:*`，但 `send_platform_message_with_context` 没有 `contrix` 分支，最终只记录 unknown platform warning，调用方还会当作发送成功。
  - 改进：新增 `contrix` 发送分支，调用 `send_to_contrix_account`。

- [x] **C-06 account-mode 入站事件把 flow id 当作发送 channel**
  - 位置：`crates/gateway-server/src/channels/contrix.rs`
  - 问题：`dispatch_to_agent` 优先把 `flow_id` 放进 `channel_id`，但出站发送函数需要的是 `realm_id`，会导致回复按 flow id 解析 realm。
  - 改进：channel id 改为 `realm_id`，`flow_id` 通过 `reply_target` 传入发送链路。

- [x] **C-07 配置了 capability grant 后加载失败会被静默降级**
  - 位置：`crates/gateway-server/src/channels/contrix.rs`
  - 问题：`grant_event_path` 配置存在时，加载或校验失败会被忽略，随后继续发送没有 `authorization_ref` 的事件。
  - 改进：account-mode 出站在 grant 加载失败或 grant 不覆盖 `cx.message.create` 时返回错误，不再静默降级。

- [x] **C-08 Contrix SDK 字段漂移导致 gateway 编译失败**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`
  - 问题：`AppletProtocolResBody` 字段已从 `icon_blob` 变为 `icon_blob_ref`。
  - 改进：更新字段名，`cargo check -p savfox-gateway-server --features contrix` 已通过。

## 第二轮处理结果与剩余项

- [x] **C-09 Applet `third_party/users` 和 `third_party/locations` 仍是 stub**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`
  - 影响：协议表面已经暴露，但返回 `404 not_implemented`；如果 Contrix server 依赖第三方查询，将无法完成 discovery / lookup。
  - 改进：实现基于 bearer 鉴权、协议列表和 applet namespace 的 third-party user/location lookup；user lookup 会按外部用户 id mint ghost DID，location lookup 会从 query 参数推导 realm/space id，并返回 `external_ref`。

- [x] **C-10 Applet-mode agent 回复仍未完整走 `send_via_applet`**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`、`crates/gateway-server/src/channel/credential_manager.rs`
  - 影响：本轮只接通 account-mode `contrix` 回复。Applet-mode 需要保存 config/appservice 上下文、ghost actor DID、external_ref 和 actor_seq 后再调用 `send_via_applet`。
  - 改进：`contrix` 出站发送现在先按已注册 applet 的 realm namespace 匹配 applet-mode，并通过 `send_via_applet` 发送；未匹配 applet 时才回落到 account-mode。发送时传入 flow id、bot actor DID、`external_ref` 和当前时间生成的 `actor_seq`。

- [ ] **C-11 Capability grant 仍未做完整签名信任验证**
  - 位置：`crates/channels/src/contrix/grant.rs`
  - 影响：当前只校验 event kind、subject、realm、expiry 和 proof digest binding；代码注释明确 issuer DID document / JWS 信任验证仍未做。
  - 本轮部分加固：无 proof grant 现在会被拒绝；proof 必须是 production-shaped，digest binding 必须通过，event actor 必须匹配 grant issuer，且至少一个 proof verification method 必须属于 issuer DID。完整 DID document 解析和 JWS 公钥验签仍未接入，因此此项保持未勾选。

- [x] **C-12 Applet transaction body 未按 describe 暴露的 `max_body_bytes` 强制限流**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`
  - 影响：`describe` 声明 `max_body_bytes: 65536`，但 handler 直接 `parse_json`，没有显式请求体大小限制。
  - 改进：transaction handler 改用 `payload_with_max_size(65_536)` 读取请求体，超限返回 `413 payload_too_large`，再对限定后的 bytes 做 JSON 反序列化。

- [x] **C-13 account listener 的事件去重集合可能长期增长**
  - 位置：`crates/gateway-server/src/channels/contrix.rs`
  - 影响：`dedupe: HashSet<String>` 在长连接期间只在 reset 时清空，没有 TTL / 容量上限。
  - 改进：将无界 `HashSet` 改成带 FIFO 淘汰的 `EventDedupe`，默认最多保留 4096 个 event id，reset 时仍会清空。

- [x] **C-14 请求路径中的 poisoned mutex 使用 `expect` 会 panic**
  - 位置：`crates/gateway-server/src/channels/contrix_applet.rs`、`crates/gateway-server/src/channels/contrix.rs`
  - 影响：registry/runtime mutex poisoned 时会直接 panic；HTTP handler 应更倾向返回 500。
  - 改进：applet registry lookup/register 改为返回 `Result`，HTTP 解析失败时返回 `500 state_unavailable`；transaction runtime mutex poisoned 也返回 500；account listener runtime state poisoned 时记录 warning 并 abort 新 listener task。

## 验证

- [x] `cargo check -p savfox-channels --features contrix`
- [x] `cargo check -p savfox-gateway-server --features contrix`
- [x] `git diff --check`
- [x] `cargo test -p savfox-channels --features contrix grant -- --nocapture`
- [x] `cargo test -p savfox-gateway-server --features contrix contrix_applet -- --nocapture`
