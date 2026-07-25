# Arkret 账户状态空闲写入与历史消息误分发修复

## 背景与已确认现象

Savfox 与 Arkret 桥接后，账户订阅状态保存在：

```text
{SAVFOX_HOME}/gateway/arkret-account-state/<channel+account>.json
```

该文件是 Garth `FileStore` 的完整持久化快照，包含订阅游标、已处理事件窗口、durable inbox、outbound queue 与 stamp floor。写入采用“同目录临时文件 + `sync_all` + 原子替换”，因此文件监听器会看到类似删除后重建的变化；原子替换本身用于避免崩溃留下半个 JSON，不应移除。

现场只读采样确认：

- 文件修改时间持续变化，但连续样本的 SHA-256 完全相同；
- 当前 durable inbox 为空，outbound 只有终态 `sent` 项；
- Gateway 仍以约 50ms 周期尝试构造认证客户端；
- 认证不可用时，同一条 “cannot drain durable inbox” 告警会持续刷屏；
- 当前没有证据表明旧消息正在被再次分发；现有事件时间与首次接收时间接近；
- 但首次绑定、游标失效、服务端要求 resync 时，订阅会以 `after = None, catchup = true` 获取历史快照，Savfox 当前会把其中可解析消息走与实时增量相同的 agent 分发路径，存在真实的历史误触发风险。

## 根因

### 1. 空闲状态仍写盘

Garth `FileStore::mutate` 对所有 mutation 无条件序列化并原子替换整个文件。以下逻辑即使没有状态变化也会进入 mutation：

- account runner 的 retry checkpoint：相同游标 + 空事件；
- outbound worker 的 `Idle` / `RetryAt` preflight；
- 重复保存相同 cursor、重复 remember、对不存在 delivery 的 ack 等。

### 2. Gateway 无工作时仍轮询认证与 outbound

Savfox Gateway 每 50ms 调用一次 durable work：

1. 无条件调用 session provider；
2. 扫 durable inbox；
3. 构建 outbound engine 并执行一次 preflight。

这会放大无变化写盘，并在认证异常时制造日志风暴。

### 3. catch-up 来源信息丢失

Garth 知道发起请求前是否没有 cursor，但把 `SyncUpdates` 转成 durable `ClientEvent` 时没有保留该来源。Savfox 因而无法区分：

- 有 cursor 的实时增量/catch-up；
- 无 cursor 的首次同步或恢复性全量 catch-up。

## 修复范围

### A. Garth：不持久化无变化 mutation

- mutation 在持有跨进程锁并重新加载磁盘状态后，对比 mutation 前后的规范 JSON；
- 只有状态实际变化，或 `ensure_created` 面对不存在的文件时，才递增持久化 generation 并执行原子替换；
- 保留原有临时文件、刷盘和原子替换语义；
- 提供只读的 active outbound 检查，供 Gateway 做工作预检。

### B. Garth：把 initial catch-up 标记写入 durable batch

- 在 account sync 请求发出前记录 `sync_loop.token().is_none()`；
- 将该标记写入 batch 首个 `AccountUpdateContext.initial_catchup`；
- 字段使用 serde 默认值，旧状态文件缺少该字段时按 `false` 读取；
- orchestrator 与 subscription driver 的 durable inbox 路径保持一致。

### C. Savfox：空闲预检、降低轮询与告警节流

- durable work 周期从 50ms 调整为 250ms；
- 在构造认证客户端之前只读检查：
  - 是否有已到重试时间的 inbox delivery；
  - 是否有 active outbound；
- 两者均无工作时立即返回；
- pending 工作遇到认证失败时，warning 最多每 30 秒一次，其余降为 debug；
- 不改变订阅长轮询及正常新消息的 cursor 增量语义。

### D. Savfox：initial catch-up 只建基线、不触发 agent

- 从 durable batch 的首个 `AccountUpdates` 读取 `initial_catchup`；
- initial catch-up 中：
  - 明文事件写入 seen window，但不调用 agent；
  - 加密事件仍推进必要的 MLS 解密/密钥包消费，然后写入 seen window，但不执行 Sidecar binding，也不调用 agent；
  - limited timeline 的 scan-backfill 沿用相同的“只建基线”策略；
- 有 cursor 的正常增量继续按现有逻辑分发；
- 增加结构化日志，明确一批事件是 baseline 还是 live。

## 不在本任务中的改动

- 不改变 Arkret 服务端 cursor/catch-up 协议；
- 不移除原子文件替换；
- 不改变 AI agent 聊天历史的 session/provenance 存储结构；
- 不修改 Garth 当前工作树中与本问题无关的 `agent_mls.rs` 改动；
- 不清理用户现有账户状态文件，也不重置 cursor。

## 验收标准

1. 对同一 cursor 的空 checkpoint、idle outbound preflight 等无变化操作，不改变状态文件内容、修改时间或持久化 generation。
2. 状态真实变化时仍原子持久化，重开 `FileStore` 后数据完整。
3. durable inbox 与 outbound 都为空时，Savfox 不调用 session provider。
4. 认证不可用且存在 pending work 时，warning 被节流，不再按轮询周期刷屏。
5. `initial_catchup = true` 的明文与可解密加密历史事件不会进入 agent 分发，但会被 durable dedupe 记住。
6. `initial_catchup = false` 的新事件仍能正常进入 agent 分发。
7. cursor 失效或 resync 后的下一次无 cursor 请求也被标记为 initial catch-up。
8. Garth 与 `savfox-gateway-server` 的定向测试通过，`cargo fmt --all -- --check` 通过。

## 复检清单

- 检查 Savfox 与 Garth 的 git diff，确认没有覆盖既有用户改动；
- 搜索所有 `account_updates_to_events` durable inbox 调用点，确认 origin 标记没有遗漏；
- 搜索所有 Savfox account parsed-event 调用点，确认 baseline 标志传递到 limited scan 和 encrypted path；
- 运行新增回归测试；
- 对状态文件做短时只读采样：空闲期哈希与修改时间均稳定；
- 检查 Gateway 日志：无工作时不再重复出现 durable inbox 认证告警。

## 实施结果（2026-07-25）

状态：代码修复与离线回归完成；运行中 Gateway 需使用新构建重启后，才能执行最后两项现场观测。

已完成：

- Garth `FileStore` 会在持有跨进程锁、加载最新磁盘状态后比较 mutation 前后 JSON；无变化时不递增 generation、不写临时文件、不原子替换目标文件。
- Garth 增加只读 active-outbound 预检。
- durable account runner 和 subscription driver 会将“请求前无 cursor”记录为 `AccountUpdateContext.initial_catchup`；旧 JSON 缺少字段时兼容为 `false`。
- Savfox durable worker 改为 250ms 周期，并在认证前检查到期 inbox 和 active outbound；完全空闲时不调用 session provider。
- pending work 的认证 warning 限制为最多每 30 秒一次。
- initial/recovery catch-up 的普通 timeline、limited scan 与 encrypted 路径均只建立 seen baseline，不调用 agent；encrypted 路径仍保留必要的 MLS 推进，且不会消费旧 Sidecar binding。
- listener diagnostic 增加 `baselined_events`。
- 保留了任务开始前已有的“加密自回声在 MLS decrypt 前忽略”改动。

验证结果：

- `cargo test --features native --test file_store_noop`（Garth）：通过，1/1；验证相同 cursor、空 checkpoint、重复 seen 与空 outbound mutation 后，文件字节和修改时间均不变。
- `cargo check --features native --lib`（Garth）：通过。
- `cargo clippy --features native --lib --no-deps -- -D warnings`（Garth）：通过。
- `cargo test -p savfox-gateway-server --features arkret initial_account_catchup_selects_history_baseline_mode`：通过，1/1。
- `cargo check -p savfox-gateway-server --features arkret`：通过。
- 两个仓库的 `cargo fmt --all -- --check`：通过。
- Savfox Arkret 定向全集：21 项中 19 项通过；两个既有测试失败，分别是 MLS welcome 计数和 realm policy group-id 表示断言，失败路径不经过本任务新增逻辑。
- Savfox clippy 被既有 lint 阻断：`arkret.rs` 中旧的 `needless_continue` / `single_match_else`，以及 `server.rs` 的 `manual_filter`；本任务新增代码没有产生 clippy 诊断。
- Garth 全部 unit tests 被既有 `runtime_store.rs` 测试编译错误阻断：重复导入 `Hash`、重复指定 `thumbnail_blob_ref`。为避免修改无关工作，使用独立 integration test 验证本次持久化行为。

部署后现场复检：

1. 使用包含 Savfox 与 Garth 本次改动的新构建重启 Gateway。
2. 在 durable inbox 为空、outbound 仅有终态项目时连续采样目标 JSON 10 秒，确认 SHA-256、修改时间均不变。
3. 检查同一窗口日志，确认没有周期性 durable-work 认证 warning。
4. 发送一条新消息，确认 cursor 增量、`received_events` 与 `dispatched_events` 各增加一次。
5. 在测试账户清空 cursor 后重启，确认出现 history-baseline 日志、`baselined_events` 增加且 agent 不被历史消息启动。
