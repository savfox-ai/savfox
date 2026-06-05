# Savfox 安全审计报告

- **审核时间**：2026-06-03
- **审核人**：资深 Rust 安全审计（Claude）
- **审核范围**：近期活跃开发的 crate，重点为
  - `crates/channels`（尤其 `crates/channels/src/contrix/*`：`signer.rs`、`session.rs`、`grant.rs`、`http.rs`、`client.rs`、`parse.rs`、`applet/*`）
  - `crates/gateway-server`（`security/ssrf.rs`、`security/auth/auth.rs`、`terminal_pty.rs`、`terminal_agent.rs`、`ws.rs`、`ws_rpc/*`、`channels/contrix*.rs`、`config/validator.rs`）
  - `crates/keyring-store`、`crates/login-oauth`
- **方法**：先 `git diff HEAD` 了解近期改动，再逐文件 Read 确认。所有发现均有实际代码引用佐证，宁缺毋滥。

整体评价：本代码库的安全基础工作做得相当扎实——HMAC 恒定时间比较（`subtle::ct_eq`）、SSRF 的 DNS pinning、密钥 `zeroize`、bcrypt 口令、RPC scope 严格 deny-by-default、密钥日志只打印长度等，都已正确实现。下列发现主要集中在 **Contrix Applet 入站事件缺乏密码学验证** 与 **SSRF 私网判定的若干云元数据/CGNAT 盲区**。

---

## 发现汇总

| # | 严重程度 | 主题 | 文件 |
|---|---|---|---|
| 1 | High | Contrix Applet 入站事件不校验签名/证明，仅凭 bearer 即信任 `sender_did` | `gateway-server/src/channels/contrix_applet.rs`、`channels/src/contrix/applet/transaction.rs` |
| 2 | Medium | Capability grant 仅校验 proof binding，不做真正的签名验证 | `channels/src/contrix/grant.rs` |
| 3 | Medium | SSRF 私网判定遗漏云元数据 IP（仅拦 169.254.169.254）与 CGNAT/0.0.0.0/8 | `gateway-server/src/security/ssrf.rs` |
| 4 | Informational | HMAC 挑战-响应在多 token 下逐个比较，遍历提前返回引入弱时序差异（复验降级） | `gateway-server/src/security/auth/auth.rs` |
| 5 | Low | Account 模式静态 `access_token` 与 SHA-256 旧口令迁移路径仍可长期存在 | `channels/src/contrix/parse.rs`、`gateway-server/src/security/auth/auth.rs` |

---

## 1. [High] Contrix Applet 入站 `transactions` 端点不验证事件签名，`sender_did` 可被任意伪造

**文件**：
- `crates/gateway-server/src/channels/contrix_applet.rs:306-526`（`applet_transactions`）
- `crates/channels/src/contrix/applet/transaction.rs:67-121`（`classify_inbound_event`）

**问题描述**：

入站 `POST /api/v1/applet/transactions` 的处理流程为：
1. `resolve_applet_for_request` 用 bearer token 鉴权**调用方**（恒定时间比较，OK）；
2. 解析 `AppletTransactionReqBody`，做幂等窗口检查；
3. 对每个事件调用 `classify_inbound_event` 决定是否派发；
4. 派发到 agent pipeline，并把事件中的 `actor_id` 原样当作 `sender_did` / `peer_id` 身份传递下去。

关键在于：`classify_inbound_event` **完全没有验证事件的 `proofs` / 签名**，只做了 loopback 过滤、realm namespace 匹配、内容类型与非空判断：

```rust
// crates/channels/src/contrix/applet/transaction.rs:67
pub fn classify_inbound_event(cfg: &ContrixAppletConfig, event: &Event) -> AppletEventOutcome {
    let actor = event.actor_id.as_str();
    if actor == cfg.bot_actor_id || cfg.namespaces.actor_matches(actor) {
        return AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet);
    }
    if event.kind != "cx.message.create" { ... }
    // ... 仅做 realm/content/body 检查，无任何 event.proofs / event.validate_* 调用
    AppletEventOutcome::Dispatch(AppletInboundCommand {
        sender_did: actor.to_owned(),   // ← 未经验证的身份，直接下传
        ...
    })
}
```

而 `applet_transactions` 在派发时把这个未验证的 `sender_did` 当作 `peer_id` 写入 pipeline：

```rust
// crates/gateway-server/src/channels/contrix_applet.rs:506
Some(runtime::StartThreadMeta {
    peer_id: Some(cmd.sender_did),
    group_id: Some(cmd.realm_id),
    ...
})
```

**攻击场景/影响**：
- 任何掌握了该 applet 的 bearer token（或在 Contrix 服务端与 savfox 之间能注入请求）的一方，可以提交任意 `actor_id` 的 `cx.message.create` 事件。下游 agent / 身份链接 / DM 策略（`identity.link`、`dm.allowlist` 等）会把这个 `sender_did` 当成可信发送者身份，从而**冒充任意用户 DID** 触发 agent，可能绕过基于发送者身份的授权或投毒身份映射。
- bearer token 只能证明「请求来自 Contrix 服务器」，不能证明「事件确实由声称的 actor 签发」。在 Contrix 的 DID-proof 体系里，事件的真实性应来自事件本身的 ed25519 proof，而非传输层 bearer。

代码注释本身已承认这是 Phase 6 的遗留限制（`crates/channels/src/contrix/applet/mod.rs:19-25`：`Events are unsigned (proofs: [])`），但出站方向已在 Phase 8 引入签名（`sign_outbound_event`），入站方向仍缺校验，形成不对称信任边界。

**修复建议**：
- 在 `classify_inbound_event`（或在 `applet_transactions` 派发前）对每个事件执行：`event.proofs` 非空检查 + `event.validate_proof_bindings()` + 针对 `actor_id` 的 verification_method 归属校验，复用 `grant.rs` 中 `load_and_verify_grant` 已有的 proof 校验模式；理想情况下还应解析 `actor_id` 的 DID document 验证 JWS 真实签名。
- 在真正的签名验证落地前，至少应在文档/配置中明确「applet 模式信任 Contrix 服务端对事件真实性的背书」，并把 `sender_did` 标记为「传输层声明、未经端到端验证」，避免下游把它当强身份用于授权决策。

---

## 2. [Medium] Capability grant 仅做 proof binding 校验，未做真正的签名验证

**文件**：`crates/channels/src/contrix/grant.rs:73-128`

**问题描述**：

`load_and_verify_grant` 用于加载 `cx.capability.grant` 并把其 `event_id` 作为出站写操作的 `authorization_ref`。它的校验包括：proofs 非空、`validate_production()`、`validate_proof_bindings()`（摘要与 body 绑定）、issuer 与 verification_method 归属、subject/realm/expiry 匹配。但代码注释明确指出**没有做真正的密码学签名验证**：

```rust
// crates/channels/src/contrix/grant.rs:92-97
// Proof binding (digest-content tie). Real cryptographic signature
// verification (issuer DID document lookup) is still out of scope here,
// but unsigned or dev-proof grants must not be accepted.
if event.proofs.is_empty() {
    anyhow::bail!("capability grant {}: missing proofs", path.display());
}
```

**攻击场景/影响**：
- `validate_proof_bindings()` 只验证「proof 里的 `payload_digest` 等于事件 body 的摘要」，但不验证「这个 proof 的 JWS 签名确实由 issuer 的私钥产生」。能写入 `$SAVFOX_HOME/contrix/grants/<id>.json` 的攻击者（本地文件写权限、被攻陷的配置同步、供应链）可以伪造一份「数字摘要自洽、issuer 字段任填」的 grant，让 savfox 以为自己持有合法授权并把其 `event_id` 当作 `authorization_ref` 提交。最终是否生效取决于 Contrix 服务端是否再次独立校验该 grant；若服务端信任 applet 上送的 `authorization_ref`，则构成授权提升。
- 影响相对受限（需要本地文件写入 + 服务端不二次校验），故定为 Medium。

**修复建议**：
- 落地 issuer DID document 解析 + JWS 验签（与发现 #1 共用同一套验签基础设施）。
- 在验签未落地前，确保 grant 文件目录有严格文件权限，并在文档中标注「grant 真实性最终由 Contrix 服务端裁决，本地校验仅为防呆」。

---

## 3. [Medium] SSRF 私网判定遗漏云元数据 IP 与 CGNAT/0.0.0.0/8

**文件**：`crates/gateway-server/src/security/ssrf.rs:113-202`

**问题描述**：

近期 diff 正确补上了 IPv4-mapped/compatible IPv6（`::ffff:169.254.169.254` 等）的判定（`ssrf.rs:123-134`），这是个好修复。但 `is_private_ip` 与 `validate_ip` 仍存在以下盲区：

```rust
// crates/gateway-server/src/security/ssrf.rs:144-148
pub fn is_metadata_hostname(host: &str) -> bool {
    let host = host.trim().trim_matches('.').to_ascii_lowercase();
    host == "metadata.google.internal" || host == "169.254.169.254"
}
// ssrf.rs:192-201
fn validate_ip(ip: IpAddr) -> Result<(), SsrfError> {
    if is_private_ip(ip) { ... }
    if ip.to_string() == "169.254.169.254" { ... }   // ← 只硬编码这一个元数据 IP
    Ok(())
}
```

`is_private_ip`（`ssrf.rs:114-142`）依赖标准库 `Ipv4Addr::is_private()`，它**不覆盖**：

1. **CGNAT / Shared Address Space `100.64.0.0/10`**（RFC 6598）——许多云厂商把内部服务、元数据代理放在此段。
2. **`0.0.0.0/8`**——在 Linux 上 `0.0.0.0` 常被路由到本机；`is_unspecified()` 只命中 `0.0.0.0/32`，不命中 `0.x.y.z`。
3. **其它云元数据端点**：阿里云/Oracle 的 `100.100.100.200`、部分平台用 `fd00:ec2::254`（虽是 ULA 会被 `is_unique_local` 命中）以及 AWS IMDSv2 仍是 169.254.169.254（已覆盖），但 GCP 以外的 `metadata.*` 主机名（如 `metadata`、`metadata.azure.internal` 视部署而定）未在 `is_metadata_hostname` 列举。

由于 `validate_ip` 是在 DNS 解析后对每个解析到的 IP 调用（`resolve_pinned_hostname:271-285`），上述任一 IP 若被攻击者用作 webhook/skills/媒体抓取目标且不在 blocklist 中，即可绕过 SSRF 防护访问内网/元数据。

**攻击场景/影响**：在允许用户配置外呼 URL 的功能（webhooks、`skills.install_url`、媒体理解抓取等）中，攻击者构造解析到 `100.64.x.x` 或 `0.x.x.x` 的域名，绕过私网拦截访问云内部服务。属经典 SSRF 提权链的一环。

**修复建议**：在 `is_private_ip` 的 IPv4 分支显式增加：
```rust
// CGNAT shared address space (RFC 6598)
let o = v4.octets();
let cgnat = o[0] == 100 && (64..=127).contains(&o[1]);
let zero_net = o[0] == 0;            // 0.0.0.0/8
v4.is_private() || v4.is_loopback() || v4.is_link_local()
    || v4.is_broadcast() || v4.is_unspecified() || cgnat || zero_net
```
并把 `is_metadata_hostname` / `validate_ip` 中的元数据 IP 扩展为一个集合（`169.254.169.254`、`100.100.100.200`、`fd00:ec2::254` 等），主机名集合补上常见 `metadata*` 变体。

---

## 4. [Informational] HMAC 挑战-响应在多 token 场景下因提前 `return` 引入弱时序差异

**文件**：`crates/gateway-server/src/security/auth/auth.rs:131-156`

**问题描述**：

`validate_challenge_response` 对每个已知 token 计算 `HMAC(nonce, token)` 并用 `ct_eq` 恒定时间比较——单次比较本身是安全的。但循环在第一个匹配处提前返回：

```rust
// auth.rs:137-154
for (token, info) in tokens.iter() {
    ...
    if expected.as_bytes().ct_eq(signature_hex.as_bytes()).into() {
        return Some(info.clone());   // ← 提前返回
    }
}
None
```

当配置了多个 token 时，「匹配第 1 个 token」与「匹配第 N 个 token」的总耗时不同，理论上可让攻击者推断匹配位置/token 数量。攻击窗口很窄（HMAC 由服务端随机 nonce 驱动，攻击者无法离线预计算），故 Low。

> **复验降级说明（Informational）**：经核对 `auth.rs:137` 实际遍历的是 `HashMap` 的 `tokens.iter()`，其迭代顺序非确定且每进程随机化，因此「匹配位置」与「token 序号」之间不存在稳定映射——攻击者即便测得耗时差也无法反推出任何有意义的 token 身份/数量信息。叠加随机 nonce 不可离线预计算，本条实际可利用性趋近于零，从 Low 降为 Informational（仅作加固备注）。

**修复建议**：累积一个 `matched: Option<TokenInfo>`，循环遍历**全部** token 后再返回（始终全量比较），消除提前返回的时序差。或保持现状但在注释中记录此权衡。

---

## 5. [Low] Account 模式静态 `access_token` 与 SHA-256 旧口令迁移路径长期存在

**文件**：
- `crates/channels/src/contrix/config.rs`（`access_token` 字段，见 `parse.rs:139-148` 测试中的 `access_token: "t"`）
- `crates/gateway-server/src/security/auth/auth.rs:173-202`（`validate_password` 旧 SHA-256 路径）

**问题描述**：

1. Contrix account 模式在未配置 `key_ref` 时退化为静态 bearer（`channels/src/channels/contrix.rs:406-408` 的 `ContrixHttpClient::new(&channel.base_url, &account.access_token)`）。静态长期 token 一旦泄露即可冒充账号，且无轮换/过期机制。代码已正确地将其作为「Phase 1-7 兼容回退」，但未对静态 token 的存在发出告警或弃用提示。

2. `validate_password` 保留 SHA-256 旧哈希校验路径并在登录成功时尝试升级 bcrypt：
```rust
// auth.rs:182-198 —— 旧路径用 ct_eq 比较无盐 SHA-256
let sha_hash = hex::encode(sha2::Sha256::digest(password.as_bytes()));
if sha_hash.as_bytes().ct_eq(entry.password_hash.as_bytes()).into() { ... auto-upgrade ... }
```
无盐单轮 SHA-256 对弱口令几乎无防护。自动升级机制良好，但**从未登录过的旧账号**会一直停留在弱哈希；若口令存储泄露，这些账号可被彩虹表瞬间破解。

**修复建议**：
- 为静态 Contrix `access_token` 增加启动告警 + 文档弃用说明，推荐迁移到 `key_ref` DID-proof。
- 对 SHA-256 旧哈希设置迁移截止期：超期未登录的账号强制要求重置口令，或后台批量提示运维。

---

## 已确认为「实现正确」的安全要点（非问题，记录备查）

- `signer.rs`：seed 材料用 `zeroize` 在多个路径擦除；`InlineSeedBase64` 在 release build 编译期拒绝（`signer.rs:100-110`）。
- `http.rs` / `auth.rs`：webhook HMAC 与 token 挑战均使用 `subtle::ConstantTimeEq`，并对长度不等正确返回 false。
- `auth.rs::required_scope`：未知方法 deny-by-default 落到 `Scope::Admin`（`auth.rs:484-490`）；`hooks.*` 写、`account/login/*`、`agent.terminal.*`(含 pty) 均提升为 Admin，防止 OperatorWrite 自我提权为 RCE。
- `ws.rs`：入站 WS 帧大小上限 1 MiB（M15）、per-IP 并发连接原子计数（S12）、连接槽 RAII 释放；query token 风险在注释中明确告知。
- `terminal_pty.rs` / `terminal_agent.rs`：spawn 使用 `Command::new(program).args(...)` 显式 argv，**未经 shell 解释**，无字符串拼接式命令注入；cwd 校验为目录；worktree 路径强制位于 git root 内（`terminal_agent.rs:613-623`）。
- `keyring-store/src/lib.rs`：`trace!` 只记录 `value_len`，不打印 secret 明文。
- `ssrf.rs`：DNS pinning（`build_pinned_client`）防 DNS rebinding；重定向每跳重新校验；IPv4-mapped IPv6 已纳入私网判定。
- `device_code_auth.rs`：服务器返回的轮询 interval 被 `clamp(1,60)`，防止恶意服务端拖死或打满 CPU。
- 工作区禁止 `unsafe`（审计范围内未见 `unsafe` 块）。

---

## 优先处理建议

1. **发现 #1（High）** 优先：为 Contrix Applet 入站事件补上签名/proof 验证，或明确降级 `sender_did` 的信任级别，避免身份伪造进入 agent 授权决策。
2. **发现 #3（Medium）** 次之：补全 SSRF 私网判定的 CGNAT/0.0.0.0/8 与多云元数据 IP，改动小、收益直接。
3. 发现 #2 与 #1 共用验签基础设施，可一并规划。
4. 发现 #4/#5 为加固项，可纳入技术债跟踪。

---

## 复验记录

复验人：安全审计复核员（持怀疑态度、以代码证据为准）。逐条对照引用文件:行号与上下文，结论如下：

- **发现 #1（High）→ 保留**。代码证据成立：`transaction.rs:67-121` 的 `classify_inbound_event` 确实只做 loopback/realm/content 过滤，无任何 `event.proofs`/验签调用，`sender_did` 取自未验证的 `actor_id`（`transaction.rs:117`）；`contrix_applet.rs:506-507` 将其原样作为 `peer_id`/`group_id` 注入 pipeline。入口仅经 bearer 校验（`resolve_applet_for_request` → `applet_token_matches`，`contrix_applet.rs:202`，`parse.rs:157-161` 为 `ct_eq` 恒定时间比较），bearer 只能证明「来自 Contrix 服务端」而非事件由声称 actor 签发；出站已签名（`sign_outbound_event`）形成不对称信任边界，mod.rs:21 注释自承 `Events are unsigned`。维持 High（注：实际利用需持有/注入 Contrix 服务端 bearer，威胁面受此限制，但身份伪造进入授权决策的风险评级合理）。

- **发现 #2（Medium）→ 保留**。`grant.rs:92-128` 注释与代码一致：仅 `validate_proof_bindings()`（摘要-内容绑定）+ issuer/vm 归属 + subject/realm/expiry 校验，明确不做 JWS 真实验签（grant.rs:92-94）。利用前提为本地写入 grants 目录且服务端不二次校验，Medium 评级与受限影响相符。

- **发现 #3（Medium）→ 保留**。`ssrf.rs:114-142` 的 `is_private_ip` IPv4 分支确无 `100.64.0.0/10`（CGNAT）与 `0.0.0.0/8` 覆盖（`is_unspecified()` 仅命中 `0.0.0.0/32`）；`validate_ip`（ssrf.rs:197）只硬编码 `169.254.169.254` 一个元数据 IP，`is_metadata_hostname`（ssrf.rs:145-148）仅列 google/169.254.169.254。盲区真实存在，且 `validate_ip` 在 DNS 解析后逐 IP 调用（ssrf.rs:282），SSRF 绕过路径可达。Medium 合理。

- **发现 #4（Low → Informational）→ 降级**。代码（auth.rs:137-154）的提前 `return` 属实，但遍历对象是 `HashMap::iter()`，迭代顺序非确定且进程级随机，「匹配位置↔token 序号」无稳定映射；叠加随机 nonce 不可离线预计算，实际可利用性趋近于零。从 Low 降为 Informational。

- **发现 #5（Low）→ 保留**。`contrix.rs:407` 确有 `ContrixHttpClient::new(&channel.base_url, &account.access_token)` 静态 bearer 回退（`key_ref` 未配置时）；`auth.rs:182-198` 确有无盐单轮 SHA-256 旧哈希校验 + 自动升级路径，未登录旧账号将长期停留弱哈希。两点均为真实的弃用/加固观察项，Low 合理。

**复验统计**：原有 5 条；删除 0 条；降级 1 条（#4：Low → Informational）；保留 4 条（#1/#2/#3/#5，含 1 条 High、2 条 Medium、1 条 Low，评级均维持）。报告「已确认实现正确」一节抽查（`applet_token_matches` 恒定时间、SSRF DNS pinning、`required_scope` deny-by-default）与代码相符，未发现需修正之处。
