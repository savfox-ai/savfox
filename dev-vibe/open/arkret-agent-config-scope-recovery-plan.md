# Arkret Agent 配置、Scope 承诺与旧实例恢复方案

## 1. 背景与本次现场结论

2026-07-25 对 Inkson → Soland → Savfox 的真实 Agent 配对和私聊链路做了联合调试。新实例 `arkret-bb` 最终完成了以下闭环：

- Agent runtime key 已授权，Savfox 本地 key digest 与 Soland 授权记录一致。
- Savfox 获得 DPoP-bound Agent session，listener 进入 `subscribing`。
- Agent 发布 MLS KeyPackage，Inkson 成功创建加密 Direct Conversation。
- Savfox 收到 controller 消息，调用本地 Agent，并以 `bb` actor 回信。

同时确认现存旧实例 `arkret-arkret` 仍处于不可自动修复的历史配置状态：

- 配置仍为 enabled。
- 后台持续重试 Agent session grant。
- Soland 返回
  `failed_precondition: reason_code=agent_requested_scope_commitment_invalid`。
- 它不应阻止新实例 `arkret-bb` 工作，也不能被自动改绑、覆盖或删除。

本文记录 Savfox 侧配置模型、旧 scope、实例路由和诊断暴露问题，作为后续完善清单。

## 2. 现场配置状态

| 实例 | 当前状态 | 结论 |
|---|---|---|
| `arkret-bb` | Paired、runtime ready、subscribing、可收发 | 当前可工作的参考实例 |
| `arkret-arkret` | Enabled、retrying、scope commitment invalid | 历史实例，需要显式迁移、重新配对或停用 |

处理原则：

1. 不自动删除或覆盖 `arkret-arkret`。
2. 不允许它的失败污染 `arkret-bb` 的状态、健康度或 outbound account 选择。
3. 不通过修改本地 scope 文本伪造已存在 Agent 的权威 provisioning ceiling。
4. 旧实例只有在 operator 明确选择后，才能执行 disable、unbind、re-provision 或 delete。

## 3. Scope 模型必须区分的三个层次

### 3.1 Provisioning ceiling

Agent 创建时写入 Soland `agent_principals.requested_scope` 的 action 集是权威、不可变的 provisioning ceiling。`ak.agent.key.authorize` 与 session proof 都会绑定这份权限边界。

它不是 Savfox 可以自由改写的普通客户端配置。

### 3.2 Session requested scope

Savfox 每次申请 Agent session 时提交的 `requestedScope` 必须满足：

- action 名来自当前 Arkret action registry；
- 是 provisioning ceiling 的合法子集；
- 包含当前运行模式必需的 service actions；
- proof 中的 scope commitment 与规范化后的实际请求一致。

### 3.3 Effective permission

最终权限仍是 provisioning ceiling、Realm membership、participation、content grant 和 session requested scope 的交集。Savfox 配置显示“已请求”不代表运行时一定拥有该权限。

## 4. 本次发现的陈旧 action 名

现场最初为 `arkret-bb` 保存了旧版、缩写或推测出来的 action 名。它们不能直接用于当前 session proof。

| 陈旧/错误配置 | 当前 canonical action | 处理 |
|---|---|---|
| `ak.self.events.scan` | `ak.self.events.query.scan` | 迁移名称 |
| `ak.self.keypackages.publish` | `ak.self.keys.keypackages.upload.create` | 迁移名称 |
| `ak.self.keypackages.consume` | `ak.self.keys.keypackages.command.consume` | 迁移名称 |
| `ak.self.keypackages.revoke` | `ak.self.keys.keypackages.command.revoke` | 迁移名称 |
| `ak.self.device_messages.receive` | `ak.self.device_messages.query.list` + `ak.self.device_messages.command.ack` | 拆成两个 action |
| `ak.self.resources.resolve` | 当前 `bb` ceiling 中不存在同名 action | 删除；不得猜测替代项 |

`arkret-bb` 当前权威 action ceiling 为：

```text
ak.event.read
ak.message.create
ak.reaction.add
ak.self.events.stream.subscribe
ak.self.events.query.scan
ak.self.events.command.submit
ak.self.keys.keypackages.upload.create
ak.self.keys.keypackages.command.consume
ak.self.keys.keypackages.command.revoke
ak.self.device_messages.query.list
ak.self.device_messages.command.ack
```

Savfox 的静态 `DEFAULT_AGENT_RUNTIME_SCOPE` 只能作为“创建新配置时的建议值”，不能覆盖已配对 Agent 的权威 ceiling。默认值与服务 action registry 发生漂移时，必须在保存前显式提示。

## 5. 已确认的 Savfox 产品问题

### 5.1 配置界面允许保存未知或陈旧 action

当前 `requestedScope` 是字符串数组，保存流程缺少 canonical action registry 校验。错误直到 listener 申请 session 才以 401/412 暴露，用户只能看到泛化的认证失败。

需要：

- 保存前拒绝未知 action；
- 对已知历史 alias 给出明确迁移预览；
- 展示“本地请求”和“权威 ceiling”的差异；
- 对已绑定 Agent 禁止静默扩大 scope。

### 5.2 旧 Agent 的 commitment 错误无法靠重试恢复

`agent_requested_scope_commitment_invalid` 表示持久信任事实与请求不一致。继续按 36 秒重试不会改变结果，只会制造日志噪声和错误健康度。

需要把错误分类为 `migration_required` 或 `reprovision_required`：

- 停止高频认证重试；
- 保持实例配置和诊断可见；
- 给出 disable、unbind/re-pair、re-provision 的 operator action；
- 不自动轮换 Agent identity。

### 5.3 Platform 级状态会被第一个旧实例污染

`channels.status` 的 `instances[id]` 能显示实例级状态，但 platform 聚合状态仍可能从同类型第一个 saved config 取值。旧 `arkret-arkret` 的 412 因而可能让 Arkret 总卡片显示 error，即使 `arkret-bb` 已 connected。

需要：

- 所有状态、测试、登录和诊断 RPC 支持明确的实例 ID；
- platform 聚合状态报告实例计数，例如 `1 ready / 1 migration required`；
- 禁止用“第一个配置”的错误覆盖其他实例；
- UI 默认进入具体实例诊断，而不是类型级模糊状态。

### 5.4 入站实例 identity 曾在回信路径丢失

listener 已把 `saved_channel_config_id=arkret-bb` 写入 `StartThreadMeta`，但回信经过 `send_with_retry` 后丢失该字段，outbound resolver 转而选择第一个 enabled Arkret config，即旧 `arkret-arkret`。

结果是：

- `bb` 能收到消息并完成 Agent 推理；
- 回信却使用旧 Agent 的 key、authorization ref 和 scope；
- session grant 报 `agent_requested_scope_commitment_invalid`。

本次已修复普通回信、命令回信、审批提示和错误回信的实例 identity 传递，并增加“两个 enabled Arkret 实例、旧实例排序在前”的回归测试。

后续仍需审计：

- idle reply；
- cron/wake 主动发送；
- WebSocket `send`；
- dead-letter replay；
- Sidecar/Applet fallback；
- 所有只接收 `platform + realm_id`、没有实例 ID 的 outbound API。

### 5.5 JSON 工具可能破坏 canonical pairing timestamp

调试中使用 PowerShell `ConvertFrom-Json` / `ConvertTo-Json` round-trip 后，
`2026-07-25T09:05:35.700Z` 被改写为 `2026-07-25T09:05:35.7Z`。Arkret canonical timestamp parser 按规范拒绝该值，实例随后变成 `configured=false` / `needs_config`。

需要：

- `channels.config.save` 提供 typed Arkret patch DTO；
- pairing bootstrap 字段由 Rust typed serializer 处理；
- 局部修改 scope 时不要重新序列化不相关的 bootstrap；
- 保存后立即重新 parse + validate 持久化结果；
- 错误中明确指出 `pairing_expires_at` 非 canonical，而不是只返回 needs config。

## 6. 建议的修复阶段

### 阶段 A：P0 安全与诊断

- [ ] 为 Arkret `requestedScope` 接入 canonical action registry 校验。
- [ ] 把 `agent_requested_scope_commitment_invalid` 分类为
  `migration_required`，停止无意义高频重试。
- [ ] `channels.status`、`channels.test` 和健康页按准确实例 ID 查询。
- [ ] platform 聚合状态显示 ready/retrying/migration-required 实例数量。
- [ ] 在实例详情中显示本地 scope、未知 action、必需 action 缺失和最后 session reason code。
- [ ] 审计所有 Arkret outbound 路径是否保留 `saved_channel_config_id`。

### 阶段 B：P1 权威 scope 获取与迁移

- [ ] 增加只读的 Agent runtime authorization/ceiling 解析路径，来源必须是 Soland 权威记录或已接受 Event，不信任本地猜测。
- [ ] 保存已配对配置前计算 requested scope 是否为权威 ceiling 的合法子集。
- [ ] 建立版本化 alias 表，只对未绑定配置提供自动迁移；已绑定配置只给预览和 operator action。
- [ ] 提供 `channels.arkret.inspect`，输出非敏感诊断：实例 ID、Agent ID、key digest、authorization ref、scope diff、runtime phase。
- [ ] 提供 dry-run migration，不输出 keyring seed、gateway token 或 bearer token。

### 阶段 C：P2 生命周期与兼容性

- [ ] 为旧配置增加显式 `legacy_scope_profile` / `migration_required` 状态。
- [ ] 提供重新配对/重新 provisioning 向导，明确区分“轮换 runtime key”和“创建新 Agent ceiling”。
- [ ] unbind 时验证旧 MLS KeyPackage pool 的 revoke/cleanup 结果。
- [ ] 保存和启动时记录 config schema/version，避免只能从 action 字符串猜年代。
- [ ] 对不可恢复错误使用低频退避和一次性通知，避免永久刷日志。

## 7. 自动化回归矩阵

### Scope

- [ ] 未知 action 保存失败并返回具体字段。
- [ ] 历史 alias 返回迁移建议，不被当成 canonical action 签入 proof。
- [ ] session scope 是 ceiling 子集时成功。
- [ ] session scope 扩大 ceiling 时 fail closed。
- [ ] 缺少 listen/send 必需 service action 时保存失败。

### 多实例

- [ ] 两个 enabled Arkret Agent config 分别绑定不同 principal。
- [ ] 第一个实例 commitment invalid，第二个实例仍能启动、收信和回信。
- [ ] 第二个实例的 inbound reply 必须由第二个 Agent actor 签名。
- [ ] disable/delete/re-pair 一个实例不影响另一个实例。
- [ ] platform 聚合状态不会把单一旧实例错误描述为全部 Arkret 不可用。

### 配置序列化

- [ ] scope patch 后 canonical millisecond timestamp 逐字节保持。
- [ ] keyRef 只保存引用，不在 RPC、日志或诊断中泄露 seed。
- [ ] 保存后重新读取的配置可通过 typed parser 和 runtime validation。
- [ ] 非 canonical timestamp 返回字段级错误。

### 恢复

- [ ] Gateway 重启后 `arkret-bb` 自动恢复到 subscribing。
- [ ] `migration_required` 实例不会阻止其他实例恢复。
- [ ] 旧实例不会被选为新实例 inbound 会话的 outbound account。
- [ ] dead-letter replay 与 idle reply 仍使用原始 saved config identity。

## 8. 当前旧实例的建议操作

在专门的迁移功能完成前，对 `arkret-arkret` 保持保守处理：

1. 保留配置文件，避免丢失旧 Agent identity 和可审计上下文。
2. UI 标记为 `Needs migration`，不要显示 Connected。
3. operator 若不再需要它，可先 disable；不要直接删除。
4. 若仍需使用，先从 Inkson/Soland 确认该 Agent 的权威 ceiling 和 authorization chain。
5. ceiling/commitment 本身属于旧 provisioning 事实时，创建新的 pairing/provisioning 流程；不要只改本地 `requestedScope` 后继续碰运气。
6. 完成新实例验证和旧 KeyPackage pool 清理后，再由 operator 明确删除旧配置。

## 9. 验收定义

本事项只有同时满足以下条件才可关闭：

1. Savfox 不再接受未知 Arkret action。
2. 已绑定 Agent 的 scope 请求可与权威 ceiling 比较。
3. commitment 错误被归类为需要迁移，而不是无限认证重试。
4. 两个同类型实例的监听、状态、Agent key、scope、MLS 状态和回信路由完全隔离。
5. 配置局部更新不会改变 canonical bootstrap 字段。
6. 真实双实例 E2E 中，旧实例失败时新实例仍能收到消息并以正确 Agent actor 回信。
