# Arkret Agent 频道

Savfox 的 Arkret Agent 运行时处理已授权的消息订阅与回复，但不通过 Arkret Signal
发送加密在线状态心跳。当前 v1 wire 已定义独立且经认证的 Agent sender 分支，但
wire 形状本身不构成发送授权。Savfox 尚未闭合当前 accepted Agent key、lifecycle、
controller authority、精确 MLS leaf 与 recipient eligibility 的端到端校验；Agent 密钥
也不得通过合成设备 ID 冒充普通设备。

因此，配对不会申请 `ak.self.signal.command.send.v1`。Savfox 监听器显示已连接，仅表示
本地运行时的连接状态，不表示已向远端发布 Agent 在线状态。只有完整 authority
evidence、发送端与接收端校验均已实现并通过端到端验证后，才能启用 Agent 在线状态。

新配对请求的候选权限由共享 Arkret SDK 操作注册表与能力最低集合生成，包含 Event
和 Seal 两种 frontier 操作。服务权限使用精确的带版本操作 ID，例如
`ak.self.events.read.scan.v1`；`ak.event.read` 等内容动作保持原名。普通在线聊天默认
不申请延迟发布 lease。

编辑已有配对时，原权限数组保持不变；缺失、旧无版本名或 `query` 别名均明确拒绝，
不会自动升级。新候选也只是申请，Station 仍须独立检查不可变 provision、当前 key
与 session 三层上限。运行时要求实际 session grant 与申请的操作集合一致；权限缩小
后无法运行完整监听器，超额授予也拒绝。最后成功 session 的缓存仅供诊断，不代表
当前 key/provision 授权，也不能用于给已有 Agent 增加权限。

按服务返回的原因恢复：不可变 provision 缺项需新建 Agent；key 缺项需在 provision
上限内重新授权；session 缺项需在两层上限内刷新 session。这些权限检查通过不表示
独立的 Agent 身份与运行时迁移已经完成。
无效的旧绑定不会被报告成解绑成功。若旧身份或权限阻止安全撤销，Savfox 保留本地
状态并要求 controller 侧恢复；只有真实解绑确认后才清除旧权限，并允许同一空频道
槽位生成新的配对候选。
