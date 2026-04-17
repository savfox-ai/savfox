# 消息路由

本文档说明消息如何从外部聊天渠道进入 gateway，被路由到对应 agent，再把回复发回原渠道。

## 概览

```
用户消息              Agent 回复
   |                     ^
   v                     |
 渠道接入层            渠道发送层
 (Discord、Telegram、  (把回复发回平台)
  Slack 等)
   |                     ^
   v                     |
 Webhook / RTM       Response Sender
   |                     ^
   v                     |
 Session Router      Session Manager
   |                     ^
   v                     |
 Agent(Core Engine) -----+
```

## 入站流程

### 1. 平台收到消息

用户在聊天平台发送消息，例如 Discord 频道、Telegram 私聊、Slack 线程等。平台通过 webhook 或实时连接把事件发给 gateway。

### 2. 请求校验

gateway 会先校验请求来源是否合法：

- **Discord**：Ed25519 签名校验（`x-signature-ed25519` + `x-signature-timestamp`）
- **Slack**：HMAC-SHA256 校验（`x-slack-signature`），带 5 分钟重放窗口
- **Telegram**：校验 `x-telegram-bot-api-secret-token`
- **Generic Webhook**：HMAC-SHA256（`x-signature` 或 `x-hub-signature-256`）

签名缺失或校验失败的请求会返回 `401` 并被丢弃。

### 3. 消息归一化

各渠道会把平台原始消息归一化为统一结构，至少包含：

- `text`：消息正文
- `sender`：平台上的发送者标识
- `channel`：平台和频道标识，例如 `discord:123456`
- `group_id`：群聊/频道 ID，私聊时通常为空
- `thread_id`：线程 ID
- `chat_type`：例如 `"dm"`、`"group"`、`"channel"`

### 4. 会话定位

session router 会根据归一化后的消息和 DM scope 生成 session key，例如：

```
agent:{agent_id}:{channel}:group:{group_id}:topic:{thread_id}
```

然后在 session store 中查找：

- 如果已有 session，就复用原上下文
- 如果不存在，就创建新的 UUID v7 session

每次消息进入时都会刷新 session 的 `updated_at`。

### 5. 调用 Agent

消息进入 session 历史后，会被送到 core engine。ThreadManager 会：

1. 加载记忆上下文
2. 组装完整 prompt
3. 调用模型并流式处理输出
4. 如有需要进入工具调用循环

### 6. 上下文压缩

如果会话历史超过模型上下文窗口，ThreadManager 会在发送前把较早消息压缩成摘要。

## 群聊激活配置

群聊里不会默认对每条消息都回复。`group_activation` 用来控制 agent 在群聊中的激活方式：

- `mention`：只有明确提到 agent 时才回复
- `keyword`：命中触发词时才回复
- `always`：群里每条消息都可触发回复
- `command`：只有命令消息才回复

这些配置以 agent 级配置为主。运行中的 session 可以通过 `sessions.patch` 临时覆盖 `group_activation`，但其他 trigger 策略目前仍然是 agent 级。

相关字段还包括：

- `group_keywords`：`group_activation = "keyword"` 时使用的关键词列表
- `agent_aliases`：显式文本定向名称，例如 `reviewer: ...`
- `ingest_policy`：控制未回复消息是否仍然进入 ambient context
- `external_bot_policy`：控制第三方 bot 消息是忽略、只摄入还是允许回复
- `idle_reply`：让 agent 在房间静置一段时间后做一次补位回复

## 触发决策模型

runtime 对每条入站消息不是简单地做“回 / 不回”二选一，而是三态决策：

- `Reply`：调用 agent，并向渠道回消息
- `IngestOnly`：当前不回复，但把消息保留到该 session 的 ambient context
- `Ignore`：既不回复，也不保留

整条链路分两层：

1. 先根据统一的消息元信息做基础触发判定
2. 再套用 agent 级策略（`group_activation`、`ingest_policy`、`external_bot_policy`、`group_keywords`、`agent_aliases`、`idle_reply`）

## 基础触发策略

基础层对私聊更积极，对多人群聊更保守。

### 直接忽略的情况

这些消息在进入 agent 策略前就会被忽略：

- 当前 bot 自己发的消息
- 本系统 agent 的 ghost user 发的消息
- bridge ghost / 系统镜像消息
- 第三方 bot 消息（除非后续被 `external_bot_policy` 改写）

### 直接回复的情况

下面这些条件会直接得到基础层 `Reply`：

- 这条消息是在回复 agent 自己上一条消息
- 这条消息被识别为命令
- 这条消息明确 mention / target 当前 agent
- 当前会话是私聊
- 当前房间只有两个人

这里有一个通用策略：**两人房间按 DM-like 对待**。即使上游平台把它标成 group，只要参与者数量是 2，也按“可直接回复”的对话处理。

### 默认不立即回复的情况

对更大的群聊，runtime 会更保守：

- 如果平台 parser 是通过 plain-text fallback 把群消息送进来的，基础层先给 `IngestOnly`
- 如果消息明确是在发给别的 agent，基础层也给 `IngestOnly`

这样做的目的，是减少共享房间里的误回复和消息爆炸。

## Agent 级策略覆盖

基础判定完成后，runtime 会加载目标 agent 的 trigger 配置并继续修正结果。

### `group_activation`

`group_activation` 只影响群聊类会话（`group`、`broadcast` 或未知房间类型），不会压制私聊和两人房间。

- `mention`：只有 mention 当前 agent 的消息保持可回复，其他群消息降级为 `IngestOnly`
- `keyword`：mention 仍然回复；否则只有正文命中 `group_keywords` 才回复
- `always`：把群里的 fallback 消息从 `IngestOnly` 提升成 `Reply`
- `command`：只有命令回复，其他群消息降级为 `IngestOnly`
- `off`：群消息不再自动回复，统一降级为 `IngestOnly`

`sessions.patch` 目前只能覆盖这个字段。

### `agent_aliases`

`agent_aliases` 允许显式的文本定向，例如：

- `reviewer: inspect this diff`
- `@reviewer summarize the thread`

如果前导 alias 命中当前 agent，会按“明确 mention”处理；如果命中别的 agent，则当前 agent 抑制回复路径，把这条消息当成“发给别人的消息”。

### `external_bot_policy`

这个字段控制第三方 bot 消息怎么处理：

- `ignore`：保持默认行为，直接丢弃
- `ingest_only`：不回复，但保留进 ambient context
- `reply_allowed`：把第三方 bot 当作普通说话者参与 trigger 判定

### `ingest_policy`

这个字段决定“不回复的消息”要不要继续保留：

- `preserve_base`：保持基础层的 `IngestOnly`
- `none` / `reply_only`：把 `IngestOnly` 再压成 `Ignore`
- `targeted_only`：只保留那些明确发给别的 agent 的 `IngestOnly`
- `all_human_messages`：把原本会被忽略的人类消息提升成 `IngestOnly`
- `all_non_bot_messages`：保留所有非 bot 消息
- `all_messages`：除 self/ghost 系统消息外，其余都保留

### `idle_reply`

`idle_reply` 增加了一条“延迟触发”的补充路径。它不是在消息到达当下立即回复，而是先观察房间是否继续有活动；如果房间静置了一段时间，再补位回复一次。

当前 MVP 的规则是：

- 只作用于群聊类会话
- 只考虑最终落到 `IngestOnly` 的人类消息
- 不会对显式 mention、命令、reply-to-self、或明确发给别的 agent 的消息做延迟补位
- 同一个 session 里只要后续出现新的入站活动，就取消之前待触发的 idle fallback
- 如果超时后仍然没有新活动，就用当前 session 的 ambient context 加上一段 idle prompt，触发一次 agent 回复

`idle_reply` 当前支持这些字段：

- `enabled`：是否开启延迟补位回复
- `delay_secs`：房间需要静置多久才触发补位
- `max_per_hour`：每个 session 每小时最多允许触发多少次 idle fallback
- `prompt`：触发延迟补位时附加的自定义提示词

## Ambient Context

当消息最终判定为 `IngestOnly` 时，它会被放进该 session 的内存态 ambient buffer。下一次这个 session 真正触发回复时，这些 buffered 消息会先以 ambient context 的形式拼到 prompt 前面，然后被消费掉。

这意味着系统可以“看见”房间里最近发生了什么，但不需要对每条消息都立即回应。

## 平台差异说明

不同渠道的 parser 允许用不同方式把消息送入 runtime，但 runtime 最终依赖的是归一化后的元信息，而不是某个平台自己的 reply 规则。

当前可以按下面理解：

- direct mention 和 reply-to-self 仍然是最强的跨平台回复信号
- 两人房间跨平台统一按私聊式处理
- 群聊 plain-text fallback 一般先 `IngestOnly`
- 只有当 agent 自己的 trigger policy 明确允许时，群聊 fallback 才会升级成回复

## 出站流程

### 7. 发送回复

agent 最终文本会再被路由回原渠道：

1. session manager 根据 session 的渠道和投递上下文决定要发到哪里
2. 渠道层把回复格式化成目标平台可接受的形式
3. 通过平台 API 把消息发出去

如果回复太长，渠道层可能拆成多条消息，以适配平台长度限制。

## WebSocket 客户端

WebSocket 客户端走的是类似但更简化的路径：

1. 客户端连接 `/ws` 并完成认证
2. 发送 `chat.send`
3. gateway 把消息路由到对应 session 和 agent
4. 流式响应通过 `Event` 回推
5. 最终结果通过 `Response` 返回

## 限流

gateway 会按认证 token 做 token-bucket 限流。超过限制的请求会返回 `429 Too Many Requests`。REST 和 WebSocket 都受这套机制约束。

## 审批路由

当 agent 想执行需要审批的命令时：

1. agent 发出审批请求
2. gateway 把审批请求转回原始渠道
3. 用户批准或拒绝
4. 决议再被路由回 agent，决定继续还是中止

用户也可以通过 `savfox gateway approvals` 在 CLI 中处理审批。

## 多渠道会话

一个 session 可以跨多个渠道延续。如果用户先在 Discord 发起会话，之后又在 Telegram 继续，同一 agent 和相同 scope 下可以保持上下文连续。session 中会记录 `channel` 和 `last_channel` 等字段来追踪这种迁移。
