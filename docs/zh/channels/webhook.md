# 通用 Webhook 渠道配置

通用 webhook 渠道允许任何能够发送和接收 HTTP POST 的平台接入 Savfox。

## 前置条件

- 一个正在运行的 Savfox gateway 服务
- 目标平台能够配置 webhook

## 基础配置

```toml
[gateway.channels.webhook]
enabled = true
callback_url = "https://your-service.example.com/savfox-events"
secret = "your-shared-hmac-secret"
```

### 字段说明

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `enabled` | `bool` | 是 | 是否启用 webhook 渠道 |
| `callback_url` | `Option<String>` | 否 | 用于接收出站事件的回调地址 |
| `secret` | `Option<String>` | 否 | 入站请求的 HMAC-SHA256 共享密钥 |

## 入站请求

向下面的地址发送 POST 请求即可把消息送进 gateway：

```http
POST https://your-gateway-host:18881/webhooks/webhook
Content-Type: application/json

{
  "action": "start_thread",
  "channel": "webhook:my-integration",
  "prompt": "Explain async in Rust",
  "user_id": "user123"
}
```

常见字段包括：

- `action`：通常是 `start_thread`
- `channel`：渠道地址，例如 `webhook:my-integration`
- `prompt`：发送给 agent 的消息
- `user_id`：调用方用户标识

## 签名校验

如果配置了 `secret`，请求头中需要带上：

```text
X-Webhook-Signature: sha256=<hex-encoded-hmac>
```

HMAC 使用共享密钥对原始请求体进行计算。gateway 同时接受：

- `sha256=<hex>` 形式
- 纯十六进制字符串

### 生成签名示例

```python
import hmac
import hashlib

secret = b"your-shared-hmac-secret"
body = b'{"action":"start_thread","prompt":"hello"}'
signature = "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()
```

## 出站事件

如果配置了 `callback_url`，gateway 会把 agent 产生的事件回推到该地址，例如：

```json
{
  "type": "agent_response",
  "channel": "webhook:my-integration",
  "text": "Async in Rust uses the tokio runtime...",
  "thread_id": "abc123",
  "timestamp": 1700000000
}
```

## 适用场景

这个渠道适合：

- 自定义业务系统接入
- 没有现成官方渠道适配的平台
- 需要通过中间层自行做身份、消息格式和路由处理的场景
