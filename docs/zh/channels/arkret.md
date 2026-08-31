# Arkret Agent 频道

Savfox 的 Arkret Agent 运行时处理已授权的消息订阅与回复，但不通过 Arkret Signal
发送加密在线状态心跳。当前 v1 Signal 信封要求普通账户设备作为发送者，尚未注册
Agent 端点载体；Agent 密钥或合成设备 ID 不能代替普通设备授权。

因此，配对不会申请 `ak.self.signal.command.send`。Savfox 监听器显示已连接，仅表示
本地运行时的连接状态，不表示已向远端发布 Agent 在线状态。只有协议定义合法载体，
并实现完整的发送端与接收端验证后，才能启用 Agent 在线状态。
