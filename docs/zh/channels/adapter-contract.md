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

## 稳定性等级

文档和评审中使用以下标签：
- Stable
- Beta
- Experimental
