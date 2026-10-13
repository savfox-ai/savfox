# 渠道适配器契约

这个文档定义 `crates/channels` 及其 gateway 集成的最低契约。

## 适配器职责

渠道适配器负责平台翻译，不负责 core agent 业务逻辑。

每个适配器至少应明确：
- 入站事件归一化
- 出站消息投递
- identity mapping 与稳定外部 ID
- 鉴权/凭据要求
- 重试与失败语义
- 平台可能重复投递时的 dedupe 或幂等策略
- 足够用于排障的 tracing/logging

## Gateway 边界

Gateway runtime 负责：
- session 查找与创建
- agent routing policy
- 长生命周期服务编排
- approval 与执行策略

适配器不应绕过这个边界。

## 确认与执行责任

每个适配器必须分别定义平台接收确认、本地耐久 inbox 完成位点、coordinator 接纳和回复提交。
不能用“已 dispatch”描述这四种不同结果，也不能把内存队列成功当成耐久执行接管。决定 source
完成位点之前必须验证可信路由；若转移重试责任到另一队列，目标必须先持久化完整工作与幂等键。
暂时失败保留 source 重试，终态失败保留具名诊断；不要靠清 dedupe 自动重放已执行指令。
仅有内存 coordinator 的路径必须明确进程崩溃恢复限制，不得承诺模型副作用“恰好一次”。

Arkret 路由必须保留 SDK 的独立 `stream_ref`、完整账号、Realm、Strand 和原请求 Event ID。
普通 Realm/Topic 与 native Sidecar 即使同名或引用同一来源 Strand，也不能共用推断的路由。
回归应覆盖缺坐标、不可读持久文件、接纳失败后 source 可重试，以及真实回复到原对话。

## 稳定性等级

文档和评审中使用以下标签：
- Stable
- Beta
- Experimental
