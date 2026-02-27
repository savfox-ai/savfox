# Generic Webhook Channel Setup

The generic webhook bridge allows integration with any platform that can send
and receive HTTP POST requests.

## Prerequisites

- A running Savfox gateway server.
- The ability to configure webhooks on your target platform.

## Configuration

```toml
[gateway.bridges.webhook]
enabled = true
callback_url = "https://your-service.example.com/savfox-events"
secret = "your-shared-hmac-secret"
```

### Fields

| Field          | Type            | Required | Description                          |
|----------------|-----------------|----------|--------------------------------------|
| `enabled`      | `bool`          | Yes      | Enable the webhook bridge            |
| `callback_url` | `Option<String>`| No       | URL for outbound event delivery      |
| `secret`       | `Option<String>`| No       | HMAC-SHA256 shared secret            |

## Inbound webhooks

Send messages to the gateway by POSTing to:

```
POST https://your-gateway-host:18881/webhooks/webhook
Content-Type: application/json

{
  "action": "start_thread",
  "channel": "webhook:my-integration",
  "prompt": "Explain async in Rust",
  "user_id": "user123"
}
```

### Signature verification

If `secret` is configured, include an HMAC-SHA256 signature in the request
header:

```
X-Webhook-Signature: sha256=<hex-encoded-hmac>
```

The HMAC is computed over the raw request body using the shared secret. The
gateway accepts both `sha256=<hex>` prefixed and raw hex signatures.

### Computing the signature

```python
import hmac
import hashlib

secret = b"your-shared-hmac-secret"
body = b'{"action":"start_thread","prompt":"hello"}'
signature = "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()
```

## Outbound events

When `callback_url` is configured, the gateway sends events to that URL:

```json
{
  "type": "agent_response",
  "channel": "webhook:my-integration",
  "text": "Async in Rust uses the tokio runtime...",
  "thread_id": "abc123",
  "timestamp": 1700000000
}
```

## Channel addressing

Use `webhook:<id>` for the channel format:

```json
{
  "channel": "webhook:my-custom-integration",
  "text": "Hello from Savfox!"
}
```

If the channel string does not contain a colon, it defaults to the webhook
platform.

## Troubleshooting

- **Signature mismatch**: Sign the exact raw body bytes with the correct secret.
- **Callback not received**: Check `callback_url` is set and reachable.
- **400 Bad Request**: Verify the JSON payload matches the expected format.
  Use `webhook:<id>` for the channel address in programmatic calls.
