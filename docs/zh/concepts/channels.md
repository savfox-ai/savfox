# 聊天渠道生命周期

聊天渠道负责把外部消息平台接进 Savfox gateway，并在平台消息格式与 gateway 操作之间做转换。

## 架构概览

```text
外部平台                Gateway                 Agent Engine
(Discord/Telegram)       (channel.rs)            (ThreadManager)
     |                       |                         |
     |-- webhook / stream -->|                         |
     |                       |-- 解析 payload          |
     |                       |-- ChannelAction         |
     |                       |-- 调用 agent ---------->|
     |                       |                         |-- 执行 turn
     |                       |<-- agent response ------|
     |<-- 平台 API 返回 ------|                         |
```

## Chatchannel trait

渠道实现统一遵循 `Chatchannel` trait，主要能力包括：

- `start()`：启动渠道接入
- `send_message()`：发送纯文本
- `send_rich_message()`：发送富文本/结构化消息
- `handle_webhook()`：把入站事件解析为 `ChannelAction`

## ChannelAction

典型动作包括：

| 动作 | 说明 |
|------|------|
| `StartThread` | 新建会话并开始处理 |
| `SendToThread` | 把消息送到已有线程 |
| `Approve` | 响应执行审批 |
| `Ignore` | 忽略无关事件 |

## 入站流程

1. 平台把事件发送到 `/webhooks/<platform>` 或实时流入口
2. 渠道校验签名或平台身份
3. 解析为 `ChannelAction`
4. 对于用户消息：
   - 进入 session 跟踪
   - 进入 runtime 做路由和 trigger 判定
   - 需要时调用 agent
5. 对于审批消息：
   - 转给审批系统继续处理

## 出站流程

1. gateway 根据渠道地址找到对应实现
2. 把统一消息格式转换成平台 API 所需格式
3. 调用平台 API 发送
4. 若失败，runtime 侧可按策略重试

## 地址格式

渠道地址一般使用：

```text
platform:identifier
```

例如：

- `discord:12345`
- `telegram:98765`
- `slack:C012345`
- `matrix:!roomid:matrix.org`

## 渠道职责边界

渠道层主要负责：

- 平台协议和鉴权
- 消息收发格式转换
- 基础元信息提取（sender、group、thread、mention 等）

真正的 agent 选择、session 复用、trigger 判定和 ambient context 处理，主要在 gateway runtime 中完成。

## 支持的渠道

仓库中已经支持或正在维护的渠道包括：

- Discord
- Telegram
- Slack
- Matrix
- WhatsApp
- Signal
- IRC
- Webhook

不同渠道在 parser 层允许的 fallback 行为可能不同，但最终都会汇总到统一 runtime。
