# Savfox 私有执行会话与 Arkret 交付投影方案

## 1. 目标与产品语义

Savfox 本地 session 与 Arkret 远端会话承担不同职责，不应实现为逐消息双向镜像：

- **Savfox session 是私有执行工作区**：保存任务拆解、反复推演、工具调用、代码修改、失败尝试、审批过程和更细的操作上下文。它允许“碎碎念”，粒度高，默认只对本地 operator 可见。
- **Arkret conversation 是共享协作与交付面**：保存指令来源、已接受状态、阶段性成果、阻塞事项、最终结论和可交付物。它面向远端参与者，粒度低，内容应稳定、可审计、可理解。
- **二者是源任务与执行投影的关系，不是完整副本关系**：Arkret 提供任务意图和外部事实；Savfox 执行任务；Savfox 只在明确的交付检查点向 Arkret 发布压缩后的公开结果。

本方案的默认原则是：

1. Arkret 指令可以进入一个绑定的 Savfox 执行 session。
2. Savfox 本地普通 user/assistant turn 不自动发送到 Arkret。
3. 任务接受、阶段性里程碑、需要远端决策的阻塞和任务结束，可以生成 Arkret 交付更新。
4. Arkret 上看到的发送者、来源、可见性和任务状态必须真实，不得把 operator 输入冒充成远端人类消息。
5. 本地私有内容可以帮助 Agent 执行，但不得在没有明确发布边界的情况下被原样泄露到远端。

## 2. 非目标

本任务不做以下事情：

- 不把 Savfox rollout、tool log、reasoning、审批记录逐条复制到 Arkret。
- 不要求 Arkret timeline 与本地 session history 数量相等或逐字一致。
- 不允许 Agent 使用自己的 Arkret DID 冒充本地 operator 的人类 DID。
- 不把初次 Arkret history catch-up 中的每条旧消息都重新触发 Agent。
- 不以 LLM 自由判断作为唯一的“何时发布”机制；关键状态必须来自显式状态转换或 operator 操作。
- 不在本阶段设计跨多个远端平台的统一发布协议；先完成 Arkret 端到端闭环，但领域模型保持 transport-neutral。

## 3. 两类上下文的边界

### 3.1 Arkret 公开上下文

允许进入 Arkret 交付面的内容：

- 原始任务指令及其 sender DID、Event id、Realm、Strand；
- Agent 对任务的接受或拒绝；
- 已完成且对协作者有意义的阶段性成果；
- 需要远端参与者回答、授权或选择的阻塞；
- 最终结果、验证结果、交付物引用和后续建议；
- 公开状态的修订、撤回和失败说明。

默认禁止发布：

- chain-of-thought、内部推理草稿和“碎碎念”；
- 完整工具 stdout/stderr、重复调试日志和无结论的尝试；
- 本地绝对路径、环境变量、token、密钥、session grant、MLS 私有状态；
- 未经确认的猜测、临时答案和随后会被覆盖的中间文本；
- operator 明确标记为 private 的输入；
- 与当前 Arkret task binding 无关的其他本地 session 内容。

### 3.2 Savfox 私有执行上下文

本地 session 可以包含：

- Arkret 指令的结构化快照；
- operator 在本地追加的说明和纠偏；
- Agent 与工具的完整执行记录；
- 尚未形成稳定结论的中间结果；
- 由远端公开信息和本地私有信息共同形成的执行上下文。

本地上下文可以比 Arkret 详细，但发布器必须重新生成最小充分的交付摘要，不能简单截取最后一条 assistant reply。

## 4. 领域模型

### 4.1 RemoteConversationKey

Arkret conversation 必须使用稳定、不可串线的复合键：

```text
(channel_config_id, account_id, realm_id, strand_id)
```

规则：

- `reply_to` 和 Event id 表示因果关系，不作为整个 logical session 的唯一键；
- 同一 Realm 下不同 Strand 必须隔离；
- 不同 Arkret 配置或 account 即使指向同一 Realm，也不得共享本地绑定状态；
- DM 不能继续依赖默认 `DmScope::Main` 合并所有 peer；Arkret 绑定至少按上述完整键隔离。

### 4.2 ExecutionBinding

增加持久化的 Arkret 执行绑定，建议字段如下：

```rust
struct ArkretExecutionBinding {
    binding_id: Uuid,
    local_session_id: String,
    channel_config_id: String,
    account_id: String,
    realm_id: String,
    strand_id: String,
    source_event_id: String,
    source_sender_did: String,
    mode: ArkretDeliveryMode,
    state: DeliveryState,
    last_ingested_event_id: Option<String>,
    last_published_checkpoint_id: Option<String>,
    public_summary_revision: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
```

约束：

- 一个 local execution session 默认只能绑定一个公开交付目标；跨目标发布必须显式 handoff 或新建 session。
- 一个 Arkret conversation 可以关联后续子 session，但必须由 binding 明确记录，不能仅靠相同 Agent id 猜测。
- `SessionEntry.thread_id` 不再同时承载平台 thread id 和 Savfox core thread UUID。至少拆分为 `core_thread_id`、`remote_thread_id` 和 `reply_to_event_id`。

### 4.3 DeliveryCheckpoint

Savfox 向 Arkret 发布的是交付检查点，而不是本地消息：

```rust
enum DeliveryCheckpointKind {
    Accepted,
    Milestone,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

struct DeliveryCheckpoint {
    checkpoint_id: Uuid,
    binding_id: Uuid,
    sequence: u64,
    kind: DeliveryCheckpointKind,
    title: String,
    summary: String,
    artifacts: Vec<DeliveryArtifact>,
    verification: Vec<String>,
    blockers: Vec<String>,
    next_actions: Vec<String>,
    source_revision: u64,
    visibility: DeliveryVisibility,
    created_at: DateTime<Utc>,
}
```

`checkpoint_id + sequence` 用作 durable outbox 与 Arkret 幂等键。成功提交后保存远端 Event id，收到自己的 Event echo 时只确认投影，不再次写入本地 session。

### 4.4 消息来源与可见性

本地 turn 至少区分：

```text
origin = arkret_remote | local_operator | agent | tool | system
visibility = remote_public | local_private | publish_candidate
```

仅 `publish_candidate` 经过交付过滤、脱敏和 operator/policy gate 后才能变成 `remote_public`。

## 5. 入站：Arkret 指令如何进入 Savfox

### 5.1 三种处理模式

替换当前 baseline/live 二分法，明确三种语义：

| 模式 | 写入 cursor/dedupe/MLS | 写入公开上下文投影 | 触发 Agent |
|---|---:|---:|---:|
| `Baseline` | 是 | 否 | 否 |
| `Hydrate` | 是 | 是 | 否 |
| `Trigger` | 是 | 是 | 是 |

- `Baseline` 用于首次建立账户和纯 crypto/state 恢复。
- `Hydrate` 用于为新 execution binding 补齐有界的 Arkret 公开历史。
- `Trigger` 只用于新指令、明确 mention、Sidecar request 或当前策略允许触发的实时事件。

live timeline gap 的 scan-backfill 默认进入 `Hydrate`；补拉结束后至多选择一条仍需处理的最新事件进入 `Trigger`，禁止逐条回复旧消息。

### 5.2 有界远端上下文恢复

创建或恢复 binding 时：

1. 使用 `events_query` 按 Realm/Strand 获取最近公开事件；如果服务端查询暂时只能按 Realm，则客户端必须在解密后按 Strand 过滤。
2. 验证 Event 签名、权限和 Realm membership，再解密 E2EE content。
3. 只保留与当前 Strand 有关的指令、状态和交付摘要。
4. 按 Event 顺序构造 `RemoteContextSnapshot`，不调用 Agent。
5. 将快照作为带 provenance 的外部上下文注入 execution session。
6. 达到 token 上限后优先保留原始任务、最近修订、未解决阻塞和最新交付状态；更老内容生成带 Event 范围的摘要。

这不是语义向量搜索。第一阶段采用确定性的 Strand timeline hydration；后续如增加检索工具，也必须返回 Event id、sender DID 和时间，且 E2EE 内容只在本地解密后索引。

### 5.3 sender 注入

当前 sender DID 不能只写入 `SessionEntry.sender/provenance`；模型输入要携带可信结构化 envelope：

```text
source_platform = arkret
channel_config_id = ...
realm_id = ...
strand_id = ...
event_id = ...
sender_did = ...
sender_kind = human | agent | service
received_at = ...
```

这些字段通过结构化 `UserInput` metadata 或受信 system context 注入，不能拼成用户可伪造的普通正文前缀。

## 6. 出站：何时向 Arkret 发布

### 6.1 Delivery mode

为 Arkret binding 增加模式：

```rust
enum ArkretDeliveryMode {
    TaskDelivery,
    InteractiveChat,
}
```

- `TaskDelivery`：采用本方案的检查点发布；普通本地 assistant reply 不立即发送。
- `InteractiveChat`：保留当前即时对话行为，但仍使用正确的 conversation key、sender metadata 和 E2EE。

新建个人 Agent task binding 默认使用 `TaskDelivery`。已有配置迁移时保持旧行为，只有显式启用后切换，避免静默改变现有机器人回复语义。

### 6.2 发布触发器

`TaskDelivery` 只在以下事件生成 checkpoint：

1. `Accepted`：远端指令通过策略校验并成功创建 execution binding。可以即时发送简短回执，不包含推理过程。
2. `Milestone`：显式完成一个可验证阶段，且距上次里程碑有实质变化。
3. `Blocked`：缺少远端输入、授权、外部状态或关键选择，Savfox 无法继续有效推进。
4. `Completed`：任务目标完成，必要验证已经运行，交付物已确定。
5. `Failed/Cancelled`：任务终止，且需要远端知道原因和可恢复路径。
6. operator 显式执行 `publish/share`。

普通 assistant turn、工具调用结束、内部 plan 更新和短暂 retry 不触发发布。

### 6.3 里程碑判定

优先使用确定性信号：

- goal/task 状态机的 `complete`、`blocked`、`cancelled`；
- 产物生成并通过验证；
- operator 明确标记 checkpoint；
- 需要远端回答的 approval/question；
- 实现阶段显式完成并形成稳定 diff/测试结果。

LLM 可以建议 milestone 文本，但不能单独决定状态转换。没有显式信号时保留在本地 session。

### 6.4 发布内容模板

建议 Arkret 文本保持稳定结构：

```text
[状态] Milestone / Completed / Blocked

结果：一句话说明发生了什么。
交付：产物、变更或结论。
验证：已经完成的检查。
下一步：下一阶段或需要远端参与者做的事情。
```

只有字段有内容时才渲染。默认不包含本地绝对路径；本地产物只有在已上传、可共享或有稳定内容摘要时才进入 Arkret。

## 7. 身份、签名与加密

### 7.1 发送者

- Arkret 原始指令保留真实远端 `actor_id/sender_did`。
- Delivery checkpoint 由配置的 Agent principal DID 签名并发送，因此远端 sender 是 Agent。
- operator 在本地写下的文字默认是 `local_operator + local_private`，不拥有远端 sender 身份。
- operator 选择“通过 Agent 发布”时，Arkret actor 仍是 Agent；可在签名覆盖的 metadata 中记录 `initiated_by=operator_via_agent`，但不得宣称 operator 是 Event actor。
- 只有 Savfox 获得并使用 operator 自己的 Arkret 凭据完成签名时，才能以 operator DID 发布；该能力不属于本任务默认范围。

### 7.2 远端加密

- checkpoint 沿用 Realm policy：要求 E2EE 时使用 MLS 加密 content，再签名完整 Event。
- 缺少合法 MLS group state 时 fail closed，不降级明文。
- checkpoint correlation、Sidecar binding 等敏感 metadata 在 E2EE Realm 中进入 `encrypted_metadata`。
- Event envelope 必需的 actor、Realm、Event id 等路由元数据不承诺隐藏。

### 7.3 本地状态

- Savfox rollout 和本地 binding store 不属于 Arkret E2EE 边界，应明确标记为本地敏感数据。
- MLS state 不应长期以未包装的 `mls_store_json` 明文保存；使用系统 credential vault 中的 wrapping key 做 at-rest encryption。
- 所有同一 account/group 的 MLS Welcome、Commit、decrypt、encrypt 和持久化 mutation 必须串行化，并增加 generation/CAS，避免入站和 outbox 并发覆盖状态。

## 8. 代码改造阶段

### 阶段 A：P0 会话隔离与数据模型

- [ ] 增加 `RemoteConversationKey` 和 durable `ArkretExecutionBindingStore`。
- [ ] Arkret account dispatch 写入准确的 `channel_config_id`、`account_id`、`realm_id`、`strand_id`。
- [ ] routing key 使用完整 conversation key，禁止同 Realm 不同 Strand 串 session。
- [ ] 拆分 `SessionEntry.thread_id` 的远端 thread 与 core thread 语义，并为旧 metadata 提供一次性迁移。
- [ ] 为 turn/provenance 增加 `origin`、`visibility`、Arkret Event id 和 sender DID。
- [ ] 群聊和 DM 的 sender envelope 以可信结构化方式进入 Agent 上下文。
- [ ] 增加会话隔离回归测试：两个 Strand、两个 account、两个 config 和两个 sender 不会意外共享执行 history。

主要涉及：

- `crates/gateway-server/src/channels/arkret.rs`
- `crates/gateway-server/src/channels/arkret_applet.rs`
- `crates/gateway-server/src/channels/runtime.rs`
- `crates/gateway-server/src/runtime/session/tracking.rs`
- `crates/gateway-server/src/runtime/session/store.rs`
- `crates/gateway-server/src/channel/session_bridge.rs`

### 阶段 B：P1 Arkret 公开上下文 hydration

- [ ] 将 `AccountInboundMode` 扩展为 `Baseline/Hydrate/Trigger`。
- [ ] limited scan-backfill 默认 Hydrate，不逐条触发 Agent。
- [ ] 增加按 RemoteConversationKey 构建 `RemoteContextSnapshot` 的服务。
- [ ] hydration 保留远端人类指令和既有 Agent 交付消息，并按 actor 映射为公开 user/assistant turn。
- [ ] 自己的历史 Event 不再简单全部 loopback-drop；在 hydration 中作为公开交付记录导入，在 live echo 中只做 outbox ack。
- [ ] snapshot 导入 rollout 时不产生模型执行和远端回复。
- [ ] Applet 模式没有查询权限时明确返回 `history_unavailable`，不得假装上下文完整；后续通过 Arkret query capability 或 transaction context 扩展。

### 阶段 C：P1 Delivery projector 与 durable outbox

- [ ] 增加 `ArkretDeliveryMode`，旧 binding 默认 `InteractiveChat`，新 task binding 可配置默认 `TaskDelivery`。
- [ ] 增加 `DeliveryCheckpoint`、脱敏过滤器和稳定文本 renderer。
- [ ] 增加 durable outbox：checkpoint 先持久化，再构造/签名/加密/发送，成功后绑定远端 Event id。
- [ ] `send_to_arkret_account` 接受 checkpoint correlation、source Event id 和 delivery metadata。
- [ ] 普通 reply path 在 `TaskDelivery` 下只保存本地，不调用 Arkret send。
- [ ] 支持 `Accepted/Milestone/Blocked/Completed/Failed/Cancelled`。
- [ ] 增加 operator 显式 publish API；默认预览待发布内容并显示远端 Agent sender DID。
- [ ] outbox echo、重试和进程重启保持幂等，不重复发布同一 checkpoint。

### 阶段 D：P2 状态信号与 UI

- [ ] 将 goal/task 明确状态转换接入 checkpoint publisher。
- [ ] 在 session UI 显示 `Local private`、`Publish candidate`、`Published to Arkret`。
- [ ] 显示绑定目标：Arkret config/account/Realm/Strand、source sender、last published checkpoint。
- [ ] 提供 `Publish milestone`、`Ask remote`、`Complete and deliver` 操作。
- [ ] 本地 composer 默认 private；切换为 publish 时必须有明显提示，不复用普通发送按钮的隐式行为。
- [ ] 支持查看 Arkret 上次公开摘要和本地当前执行摘要的差异，但不展示 chain-of-thought。

### 阶段 E：P2 加密状态与恢复

- [ ] MLS state 按 scope 加锁并使用 generation/CAS。
- [ ] 引入 credential-vault wrapping key，对 crypto state 做 at-rest encryption。
- [ ] Gateway 崩溃恢复后先恢复 binding/outbox/MLS，再恢复 listener。
- [ ] 恢复时已提交但未确认的 checkpoint 使用相同幂等键查询或重试。
- [ ] 增加密钥缺失、epoch 落后、cursor 丢失和 outbox 半提交测试。

## 9. 状态机

```text
Unbound
  -> Accepted
  -> Running
  -> Milestone* -> Running
  -> Blocked -> Running
  -> Completed | Failed | Cancelled
```

约束：

- `Milestone` 不结束任务，可以多次出现；每次必须有递增 sequence 和内容变化。
- `Blocked` 必须说明需要谁提供什么；解除后回到 `Running`。
- `Completed` 必须携带结果和验证摘要；没有验证时明确写“未验证”及原因。
- 终态后新的远端指令默认创建新 binding 或显式 reopen，不能静默复用旧 rollout。

## 10. 发布策略默认值

建议初始默认配置：

```json
{
  "mode": "task_delivery",
  "sendAccepted": true,
  "publishOnBlocked": true,
  "publishOnCompleted": true,
  "publishOnFailed": true,
  "autoMilestones": false,
  "minimumMilestoneIntervalSeconds": 300,
  "requirePreviewForManualPublish": true
}
```

第一版关闭自动 milestone，只允许显式 task state、operator publish 和终态触发。待内容过滤与误发测试成熟后，再允许 agent 提议、状态机确认的自动 milestone。

## 11. 可观测性

每个 binding 暴露非敏感诊断：

- local session id、remote conversation key 的脱敏/结构化表示；
- delivery mode 和当前状态；
- last ingested Event id、last published checkpoint id/Event id；
- pending outbox 数量、最近错误和下一次重试时间；
- hydration 起止 Event、导入条数、过滤条数和是否 truncated；
- private turn 数、publish candidate 数和 published checkpoint 数；
- 当前 MLS readiness，但不输出 key、明文 state 或 session grant。

日志必须使用 binding/checkpoint/Event id 关联，禁止记录完整私有 prompt 或 checkpoint 加密前正文。

## 12. 自动化回归矩阵

### 会话与上下文

- [ ] 同 Realm 两个 Strand 创建不同 local execution session。
- [ ] 同 Strand 不同 config/account 不共享 session、outbox 或 MLS scope。
- [ ] 初次 baseline 不导入历史、不触发 Agent。
- [ ] Hydrate 导入公开历史但不触发 Agent。
- [ ] live gap 补拉多条旧消息只 hydrate，不产生回复风暴。
- [ ] 当前 Trigger 事件进入正确 session，并携带不可伪造的 sender DID metadata。
- [ ] Gateway 重启后从 rollout 与 binding 恢复同一 execution session。

### 私有与公开边界

- [ ] 本地普通 user/assistant turn 不调用 Arkret outbound。
- [ ] 本地 private turn 可以影响后续执行，但不会被 checkpoint renderer 原样引用。
- [ ] 另一 session 的内容不能进入当前 binding 的 checkpoint。
- [ ] tool log、reasoning、密钥形态字符串和本地绝对路径被发布过滤器拒绝或脱敏。
- [ ] operator publish 预览显示实际 Agent sender DID。

### Delivery checkpoint

- [ ] Accepted、Milestone、Blocked、Completed、Failed、Cancelled 均生成规范结构。
- [ ] 同一 checkpoint 重试只产生一个 Arkret Event。
- [ ] Agent 自己的 live echo 只确认 outbox，不再次触发 Agent。
- [ ] checkpoint 携带 source Event correlation，并保持原 Realm/Strand。
- [ ] TaskDelivery 普通 reply 不发送；InteractiveChat 仍即时回复。
- [ ] Completed 包含交付与验证；Blocked 包含所需远端动作。

### 身份与加密

- [ ] 远端人类指令保留原 sender DID。
- [ ] checkpoint Event actor 是绑定的 Agent principal，不是本地 operator。
- [ ] E2EE Realm 的 checkpoint content 和敏感 metadata 均加密。
- [ ] 缺少 MLS group state 时 fail closed，outbox 保留可重试状态。
- [ ] 并发入站 decrypt 与出站 encrypt 不丢失 MLS epoch/state。
- [ ] 本地 crypto state 文件不出现未包装的 MLS 私有状态。

## 13. 验收定义

本事项只有同时满足以下条件才可关闭：

1. Savfox 本地普通对话不会自动逐条同步到 Arkret。
2. Arkret task conversation 与本地 execution session 通过稳定 binding 关联，不再按 Realm 或默认 main session 粗粒度串线。
3. Agent 能获得当前 Strand 的有界公开上下文、真实 sender 和 source Event，但 history hydration 不触发旧消息回复。
4. TaskDelivery 只发布明确的接受、里程碑、阻塞和终态 checkpoint。
5. Arkret 上的 checkpoint sender 始终是实际签名的 Agent principal；operator 身份不被伪造。
6. 私有执行细节、工具日志、reasoning、凭据和无关 session 内容不会进入远端交付。
7. checkpoint 使用 durable outbox、幂等键和 source Event correlation，崩溃恢复后不重复、不丢失。
8. E2EE Realm 的交付更新保持现有 MLS fail-closed 语义，同一 crypto scope 的并发 mutation 被安全串行化。
9. UI 能明确区分 Local private、Publish candidate 和 Published to Arkret，operator 不会误以为普通本地发送已经公开。

## 14. 推荐实施顺序

严格按以下顺序推进：

1. 先修 RemoteConversationKey、binding 和 thread id 命名空间，消除跨 Strand/DM 上下文泄漏。
2. 再实现 sender metadata 与 Baseline/Hydrate/Trigger，保证输入上下文正确且不误回复历史。
3. 然后实现 TaskDelivery、checkpoint renderer 和 durable outbox，切断普通 reply 的自动远端发送。
4. 接入 goal/task 状态与 UI 显式发布操作。
5. 最后完成 MLS state at-rest encryption、并发 CAS 和真实 Arkret E2E 验收。

在第 1、2 阶段完成前，不应开启自动 milestone；在 durable outbox 和内容过滤完成前，不应将 TaskDelivery 设为已有 Arkret 实例的默认行为。
