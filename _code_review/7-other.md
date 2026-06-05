# 代码审查报告 — 其他质量问题（正确性 / 并发 / 错误处理 / 资源 / 性能 / 可观测性）

审查范围：近期改动（`git diff HEAD`）重点 crate：`crates/channels/src/contrix/*`、`crates/gateway-server/*`（terminal_pty / terminal_agent / ws / agent_terminal_delegate / ws_rpc/handlers/agent）、`crates/keyring-store`、`crates/gateway-server/src/security/auth`。

下列每条均已 Read 实际代码确认。严重程度分级：**高**（可触发实际故障/数据丢失/挂起）、**中**（边界场景出错或体验劣化）、**低**（健壮性/可观测性改进）。

---

## 高严重度

### H1. `wait_for_text` 与 `Notify::notify_waiters()` 存在丢失唤醒（missed-wakeup）竞态

**文件**：`crates/gateway-server/src/terminal_pty.rs:611-636`（消费侧），`:406-429`（生产侧 `append_output`）

**问题**：`wait_for_text` 的等待循环是：
```rust
loop {
    let entries = session.transcript().await;          // 1. 检查
    if entries.iter().any(|e| e.text.contains(needle)) { return Ok(Some(entries)); }
    ...
    if tokio::time::timeout(wait, session.notify.notified()).await.is_err() { // 2. 注册并等待
        return Ok(None);
    }
}
```
生产侧 `append_output` 先更新 transcript（`:409-412`），再 `self.notify.notify_waiters()`（`:428`）。

`tokio::sync::Notify::notify_waiters()` **不会为未来的等待者保存 permit**（与 `notify_one()` 不同）。如果一次 `notify_waiters()` 恰好发生在消费者「步骤 1 检查完毕」之后、「步骤 2 注册 `notified()`」之前，该通知会被永久丢失。消费者随后只能等到**下一条**输出到来才被唤醒，否则一直阻塞到超时。

**影响**：管理式 PTY 的 `agent.terminal.pty.read?wait_for_text=...`（`ws_rpc/handlers/agent.rs:547`）可能在目标文本其实已经出现的情况下仍阻塞至超时返回，进而误判 REPL "ready"/命令完成等关键时序。在交互式代理流水线里会表现为偶发的"卡住/超时"。这是真实的并发正确性 bug，不是理论问题。

**修复建议**：改用「先获取 `Notified` future 再检查」的标准模式以闭合窗口：
```rust
loop {
    let notified = session.notify.notified();   // 先注册 future（pin）
    tokio::pin!(notified);
    let entries = session.transcript().await;   // 再检查
    if entries.iter().any(|e| e.text.contains(needle)) { return Ok(Some(entries)); }
    // ...超时包裹 notified.await
}
```
`Notify` 文档明确给出此「check-after-enable」顺序以避免丢失通知。注意 `notify_waiters()` 仍不保存 permit，但只要在检查**之前**就持有了 `Notified` future（已 enabled），窗口即被关闭。

---

## 中严重度

### M5. `wait_for_text` 路径 `timeout_ms=0` 被强制改成 1ms，缺省不传超时时等待形同失效（复核降级：高→中）

**文件**：`crates/gateway-server/src/ws_rpc/handlers/agent.rs:541-558`

**问题**：`timeout_ms` 缺省为 0，随后调用 `wait_for_text(.., Duration::from_millis(timeout_ms.max(1)))`。当客户端传 `wait_for_text` 但不传 `timeout_ms`（或显式传 0）时，超时被钳为 **1 毫秒**。`wait_for_text` 内部 deadline = `now + 1ms`，几乎必然在首次 `transcript()` 检查后立即超时返回 `Ok(None)` → `unwrap_or_default()` → 返回空 entries。

**影响**：调用方期望「不带超时即长期等待」或「等待合理默认时长」，实际得到近乎即时的空返回，`wait_for_text` 形同失效。属于错误的边界条件处理。

**复核降级说明（高→中）**：原报告将此定为「高」。复核认为夸大：该缺陷既不会导致挂起，也不会造成数据丢失或损坏——仅是「不显式传 `timeout_ms` 时等待无效」，且客户端只要显式传一个合理 `timeout_ms` 即可完全规避。属功能性边界缺陷而非可触发实际故障/数据丢失/挂起的高危项，故降级为中。

**修复建议**：为 `wait_for_text` 路径设定合理的默认超时（例如缺省 5s，并保留 30s 上限），不要把 0 当作 1ms：
```rust
let timeout_ms = params.get("timeout_ms").and_then(Value::as_u64)
    .unwrap_or(5_000).clamp(1, 30_000);
```

---

### M1. PTY transcript 字节记账用 `String::len()`（UTF-8 字节）而修剪只保留 `entries.len() > 1`，多字节文本下内存上限可被显著突破

**文件**：`crates/gateway-server/src/terminal_pty.rs:274-288`

**问题**：`append` 用 `text.len()`（字节）累加 `total_bytes`，修剪循环为：
```rust
while self.total_bytes > self.max_bytes && self.entries.len() > 1 {
    let removed = self.entries.remove(0);
    self.total_bytes = self.total_bytes.saturating_sub(removed.text.len());
}
```
记账本身一致（都用字节）。但 `entries.len() > 1` 的守卫意味着**永远至少保留 1 条**，且不会拆分单条。若单条 entry 极大（reader 每次最多读 4096 字节，理论上单条 ≤ 4096B，问题较小），上限基本可控。真正的小问题是：`max_bytes` 默认 2MiB，每条 ≤4096B，最坏情况保留到刚好越界后才停止，实际峰值可达 `max_bytes + 4096`。可接受，但建议在文档/注释里说明这是「软上限」。

**影响**：低-中。内存超额有界（约一个读缓冲），不会无界增长。

**修复建议**：可不改；若要精确，可在 append 前先按需修剪。注意 `Vec::remove(0)` 是 O(n)，高频输出下整体 O(n²)，见 P1。

### M2. `applet_transactions` 中 Idempotency 先 `record` 再分发，但分发是 `tokio::spawn` 异步，分发失败时无法回滚去重，重试会被当成"重复"丢弃

**文件**：`crates/gateway-server/src/channels/contrix_applet.rs:395-518`

**问题**：在 `IdempotencyDecision::Fresh` 分支立即 `record(...)`（`:414`），然后对每个命令 `tokio::spawn` 异步分发（`:498`）。若 spawn 出去的流水线**失败**（panic、下游不可用），调用方（Contrix 服务器）按 idempotency 协议重试时，会命中 `Duplicate` 分支（`:420`）直接返回 `ok:true` 缓存结果——但实际上消息从未成功处理，**静默丢失**。

**影响**：中。这是 at-most-once 而非 at-least-once 语义。在下游短暂故障窗口内到达的事务会永久丢失，且对端收到 200 误以为成功。

**修复建议**：要么把 record 推迟到分发确认入队/成功之后，要么接受 at-most-once 但在文档明确标注；更稳妥的是分发改为同步入队到一个有界持久队列后再 record。至少应在 spawn 的任务里对失败做告警日志（当前任务体内无任何错误处理，见 L1）。

### M3. 多处 `timestamp_millis().max(0) as u64` 作为 `actor_seq` / HLC，时钟回拨或并发会产生非单调序列

**文件**：
- `crates/gateway-server/src/channels/contrix.rs:445`（`actor_seq`）
- `crates/gateway-server/src/channels/contrix_applet.rs:793`（`actor_seq`）、`:926`（HLC）
- `crates/channels/src/contrix/outbound.rs:104`、`applet/outbound.rs:153`（HLC）

**问题**：`actor_seq` 注释要求「Monotonic per-actor sequence number. Caller maintains this.」（`applet/outbound.rs:48-49`），但调用方实际用毫秒时间戳充当。两条消息在同一毫秒内发出 → 相同 `actor_seq`；NTP 回拨 → 序列倒退。Contrix 服务器若校验 `actor_seq` 严格递增，会拒绝事件或判重。

**影响**：中。高频发送（同毫秒多条）或时钟回拨时，事件可能被服务器拒绝/排序错误。`outbound.rs:103` 自己也注释了 HLC 的 logical 位恒为 0，依赖「单发射器按时间单调」——但同毫秒并发恰恰打破该假设。

**修复建议**：维护一个 per-actor 的原子计数器（`AtomicU64`），以 `max(now_ms, last+1)` 方式生成单调序列；HLC 的 logical 位同理在同毫秒内自增，而非恒 0。

### M4. `ws.rs` Drop guard 在同步 `Drop` 里 `tokio::spawn` 清理，运行时关闭时清理任务可能不被执行

**文件**：`crates/gateway-server/src/ws.rs:43-55`（`SessionCleanupGuard`）、`:174-188`（`ConnectionSlotGuard`）

**问题**：两个 RAII guard 都在 `Drop` 中 `tokio::spawn(async move { ... })` 来做异步清理（移除 session / 释放连接槽）。如果 Drop 发生在 Tokio runtime 正在 shutdown 时，`tokio::spawn` 会 panic（"spawn after runtime shutdown"）或任务被立即丢弃不执行，导致连接槽 / session 泄漏。正常路径已 `disarm` 并显式清理（`:405-406`），所以仅影响 panic / 早返回 + 运行时关闭的叠加场景。

**影响**：中-低。正常运行无碍；进程退出阶段的早退连接可能泄漏 per-IP 连接计数（仅影响该进程生命周期，随退出释放）。

**修复建议**：清理逻辑尽量走显式 `await` 路径（如本文件正常路径所做）；Drop 兜底里对 `tokio::runtime::Handle::try_current()` 做判断，拿不到 handle 时退化为同步尽力而为或跳过，避免 spawn-after-shutdown panic。

---

## 低严重度

### L1. applet 分发 spawn 的任务体无任何错误/完成日志，失败完全静默

**文件**：`crates/gateway-server/src/channels/contrix_applet.rs:498-517`、`crates/gateway-server/src/channels/contrix.rs:334-354`

**问题**：`tokio::spawn` 内直接 `.await` 流水线函数，其返回值（若有 Result）或 panic 均未被记录。结合 M2，inbound 事务处理失败时既不回滚去重也不告警，排障时无任何线索。

**影响**：低（可观测性）。生产环境难以定位"消息收到但代理没回应"类问题。

**修复建议**：在 spawn 任务内对流水线结果做 `if let Err(err) = ... { warn!(...) }`，并考虑 `tokio::task::JoinHandle` 的 panic 捕获日志。

### L2. `looks_like_jsonrpc` 仅扫描前 64 字节，紧凑 JSON 中 `"jsonrpc"` 若被前置大字段挤出窗口会被误判为 GatewayMessage

**文件**：`crates/gateway-server/src/ws.rs:159-164`

**问题**：注释声称「first ~64 bytes always contains the discriminator」。这对自家客户端成立，但对手工/第三方构造的 JSON-RPC 帧（例如把 `"id"` 或自定义字段放在 `"jsonrpc"` 之前且很长）会判错，落入 `GatewayMessage` 解析分支并因解析失败被静默 `continue`（`:382-391`），客户端收不到 JSON-RPC 错误响应只见"无响应"。

**影响**：低。仅影响非常规字段顺序的外部客户端；自家代码不受影响。

**修复建议**：64 字节窗口可放宽到首个 `}` 或一个更大的常量（如 256）；或在两个分支都解析失败时回送一个统一的 parse error，避免"静默 continue"。

### L3. `construct_applet_client` / `construct_account_client` 每次发送都重新 DID-proof 登录，未复用 session

**文件**：`crates/gateway-server/src/channels/contrix_applet.rs:949-988`（每次 `send_via_applet`/`emit_bridge_error` 各调用一次），`crates/gateway-server/src/channels/contrix.rs:367-409`

**问题**：`send_via_applet` 中先 `construct_applet_client`（含一次 `ContrixHttpClient::login` 网络往返）发送主事件；失败后 `emit_bridge_error` 内又 `construct_applet_client` **再登录一次**（`:942`）。每条出站消息至少 1 次、失败时 2 次完整 DID-proof 登录握手。`client.rs` 已提供 `ContrixSession`（带 `expires_at`）与 `refresh_bearer`，但调用方完全没缓存/复用。

**影响**：低-中（性能 / 热路径）。出站消息延迟叠加一次登录 RTT；高频发送下对 Contrix 认证端点压力大，且 `_session` 被直接丢弃（`:969` 的 `let (client, _session)`），`is_near_expiry` 机制形同虚设。

**修复建议**：按 (config_id / account_id) 缓存已登录的 `ContrixHttpClient` + `ContrixSession`，发送前用 `is_near_expiry` 判断是否需要 `refresh_bearer` 或重登，避免每条消息重新登录。

### L4. `terminal_agent::record_terminal_runtime_metrics` 把 `spawn_count` 当作"调用计数"自增，命名与语义不符

**文件**：`crates/gateway-server/src/terminal_agent.rs:301`

**问题**：`record_terminal_runtime_metrics` 在每次记录退出时都 `metrics.spawn_count += 1`，但该函数在进程已结束后调用，`spawn_count` 实际等于"完成计数"。字段名 `spawn_count` 会让读指标的人误解（真正 spawn 失败 `SpawnError` 也计入）。

**影响**：低（可观测性 / 语义清晰度）。

**修复建议**：重命名为 `invocation_count` 或 `recorded_count`，或在真正 spawn 时单独自增。

### L5. `spawn_reader` 错误分支把读错误写进 transcript 并 `break`，但未标记 completion，依赖 reader 自然退出

**文件**：`crates/gateway-server/src/terminal_pty.rs:715-743`

**问题**：stdout/stderr reader 读到错误时把错误文本 append 到 transcript 后 `break` 退出任务，但不调用 `mark_completion`。若两个 reader 都因错误退出而子进程仍"存活"（罕见），session 的 `completion` 仍是 `Running`，metadata 显示可重连，但实际已无输出来源。正常 EOF（`Ok(0)`）路径同样不标记完成——完成依赖 sentinel 文本或 idle 超时或显式 close。

**影响**：低。idle 超时兜底会最终回收；但中间窗口内 metadata 状态不准确。

**修复建议**：reader 在两个流都结束 / 出错后，可触发一次 `mark_completion`（需协调两个 reader，或由一个 watcher 监控 child 退出）。当前 idle 兜底可接受，建议补注释说明。

---

## 已确认无问题（排查后排除）

- `terminal_pty.rs:144` `Box::pin(stdout)`：`stdout` 此处是 `ChildStdout`，单次 pin，非双重 pin。正确。
- `applet_transactions` 的 `std::sync::Mutex` guard（`:397-448`）在 `.await` 之前的代码块内全部 drop，**未跨 await 持锁**。正确。
- `terminal_agent.rs:298,326` 指标锁用 `unwrap_or_else(PoisonError::into_inner)`，毒化不级联 panic。正确且稳健。
- `keyring-store` 的 Mock 与默认实现锁处理（`unwrap_or_else(PoisonError::into_inner)`）稳健，无跨 await 持锁（均为同步 trait）。正确。
- `auth.rs` 直接 token 校验 `tokens.get(token)`（`:113`）虽非常量时间（时序侧信道），但属安全类问题，归口安全审查 agent，本报告不计入。
- `signer.rs` seed 处理用 `zeroize` 且长度校验在 copy 前，错误路径也 zeroize。正确。

---

## 汇总

| 编号 | 文件:行 | 类别 | 严重度 |
|---|---|---|---|
| H1 | terminal_pty.rs:611-636 | 并发（丢失唤醒） | 高 |
| M5 | ws_rpc/handlers/agent.rs:541-558 | 边界条件 | 中（原高，复核降级） |
| M1 | terminal_pty.rs:274-288 | 资源（软上限） | 中 |
| M2 | contrix_applet.rs:395-518 | 并发/语义 | 中 |
| M3 | contrix.rs:445 等 | 正确性（单调序列） | 中 |
| M4 | ws.rs:43-55,174-188 | 资源（spawn-after-shutdown） | 中 |
| L1 | contrix_applet.rs:498 / contrix.rs:334 | 可观测性 | 低 |
| L2 | ws.rs:159-164 | 健壮性 | 低 |
| L3 | contrix_applet.rs:949 / contrix.rs:367 | 性能（每发登录） | 低 |
| L4 | terminal_agent.rs:301 | 可观测性 | 低 |
| L5 | terminal_pty.rs:715-743 | 状态准确性 | 低 |

最值得优先修复的是 **H1**（真实的 missed-wakeup 竞态，会让 PTY `wait_for_text` 偶发挂起至超时）与 **M5**（原 H2，`timeout_ms=0` 被钳成 1ms 使等待形同失效；复核已降级为中，因客户端可显式传 timeout 规避且无数据损坏）。

---

## 复验记录

复核基准：实际 Read 引用文件:行号，核对 tokio/Rust 语义。逐条结论如下。

- **H1（terminal_pty.rs:611-636 丢失唤醒）— 保留**。等待循环确为「先 `transcript()` 检查，再 `notify.notified()` 等待」，生产侧 `append_output`（:406-428）先改 transcript 再 `notify_waiters()`。`notify_waiters()` 不为未来等待者保存 permit，检查与注册之间的窗口内通知会丢失。存在超时兜底（:629-634），故最坏是阻塞至 deadline 返回 `Ok(None)` 而非无限挂起——报告描述「阻塞到超时」准确，未夸大。评高合理。
- **H2→M5（agent.rs:541-558 timeout_ms=0→1ms）— 降级（高→中）**。代码事实成立：缺省 `unwrap_or(0)` 后 `.max(1)` 把 0 钳成 1ms，不显式传超时则 wait 形同失效。但该缺陷不导致挂起/数据丢失/损坏，且客户端显式传合理 `timeout_ms` 即可规避，不符合本报告「高=可触发实际故障/数据丢失/挂起」的定义。已降级为中并移入中严重度区块（编号改 M5）。
- **M1（terminal_pty.rs:274-288 软上限）— 保留**。记账与修剪均用字节，`entries.len() > 1` 守卫使峰值约 `max_bytes + 一个读缓冲(4096B)`，有界。报告自评低-中且建议可不改，结论恰当。
- **M2（contrix_applet.rs:395-518 record 先于 spawn 分发）— 保留**。`Fresh` 分支立即 `record`（:414），随后 `tokio::spawn` 异步分发（:498），spawn 任务体内无错误处理。分发失败后重试命中 `Duplicate` 返回缓存 ok，确为 at-most-once、静默丢失。描述准确，评中合理。
- **M3（contrix.rs:445 等 actor_seq/HLC 非单调）— 保留**。`actor_seq` 注释要求单调（applet/outbound.rs:48-49），实际用 `timestamp_millis().max(0) as u64`；HLC logical 位恒 0（outbound.rs:104-105）。同毫秒并发/时钟回拨破坏单调。报告用「若服务器严格校验...会拒绝」限定，且代码注释自承「Contrix v1 tolerates monotonic-by-time」，属潜在风险，评中（偏保守）可接受，保留。
- **M4（ws.rs:43-55,174-188 Drop 中 spawn 清理）— 保留**。两 guard 的 Drop 确为 `tokio::spawn`。正常路径 `SessionCleanupGuard` 已 disarm（:405），但 `ConnectionSlotGuard` 无 disarm、每次结束都靠 Drop spawn。仅 runtime shutdown 时有 spawn-after-shutdown 风险（runtime 仍存活时 spawn 正常）。报告评中-低且影响限于进程退出阶段，结论准确。
- **L1（spawn 任务无错误日志）— 保留**。已确认 contrix_applet.rs:498 与 contrix.rs:334 的 spawn 任务体内直接 `.await` 流水线，无 Result/panic 记录。可观测性问题，评低准确。
- **L2（ws.rs:159-164 looks_like_jsonrpc 仅扫前64字节）— 保留**。代码确为 `head[..min(64)].contains("\"jsonrpc\"")`。仅影响异常字段顺序的外部客户端，自家协议不受影响，评低准确。
- **L3（每发重新 DID-proof 登录）— 保留**。`construct_account_client`（contrix.rs:367-409）每次 login 且 `_session` 丢弃（:392）；applet 侧同理。无 session 缓存复用，评低-中（性能）准确。
- **L4（terminal_agent.rs:301 spawn_count 语义错位）— 保留**。`record_terminal_runtime_metrics` 在进程退出后调用却自增 `spawn_count`，实为完成/记录计数，命名误导。评低（可观测性）准确。
- **L5（terminal_pty.rs:715-743 reader 退出不 mark_completion）— 保留**。stdout/stderr reader 在 EOF/错误时 `break` 但不标记完成，依赖 idle 超时兜底。中间窗口 metadata 状态不准，评低准确。
- **「已确认无问题」区块 — 复核认可**。抽查其中：stdout 单次 pin、applet 同步 Mutex guard 不跨 await、指标锁 `PoisonError::into_inner` 不级联 panic、signer zeroize——均与代码一致，排除合理。

**统计**：原有 11 条（H1、H2、M1-M4、L1-L5）。删除 0 条；降级 1 条（H2→M5，高→中）；保留 10 条（其余原级别不变）。
