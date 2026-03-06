# Chat Bridge Lifecycle

Chat bridges connect external messaging platforms to the Savfox gateway. Each
bridge translates between the platform's message format and gateway operations,
enabling users to interact with the agent from Discord, Telegram, Slack, Matrix,
and other services.

## Architecture

```
External Platform          Gateway                 Agent Engine
  (Discord, Telegram...)     (bridge.rs)            (ThreadManager)
       |                       |                         |
       |-- webhook POST ------>|                         |
       |                       |-- parse payload         |
       |                       |-- ChannelAction          |
       |                       |-- invoke agent -------->|
       |                       |                         |-- process turn
       |                       |<-- agent response ------|
       |<-- platform API ------|                         |
```

## The ChatBridge trait

All bridges implement the `ChatBridge` trait (`crates/gateway-server/src/bridges/mod.rs`):

```rust
pub trait ChatBridge: Send + Sync {
    async fn start(&mut self) -> anyhow::Result<()>;
    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()>;
    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()>;
    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction>;
}
```

- **start()** -- initialize the bridge (connect to platform API, register
  commands).
- **send_message()** -- send plain text to a channel.
- **send_rich_message()** -- send structured content (code blocks, embeds,
  titles, color accents).
- **handle_webhook()** -- parse an inbound webhook payload into a
  `ChannelAction`.

## ChannelAction variants

When a webhook arrives, the bridge parses it into one of these actions:

| Action          | Description                                          |
|-----------------|------------------------------------------------------|
| `StartThread`   | Create a new agent thread with a prompt              |
| `SendToThread`  | Route a message to an existing thread                |
| `Approve`       | Respond to an execution approval request             |
| `Ignore`        | No action needed (ping, verification, unrelated msg) |

## Message flow (detailed)

### Inbound (platform to gateway)

1. The external platform sends an HTTP POST to `/webhooks/<platform>`.
2. The bridge handler verifies the request signature (HMAC, Ed25519, or
   platform-specific mechanism).
3. The payload is parsed into a `ChannelAction`.
4. For `StartThread`:
   - The bridge runtime calls `track_inbound_message()` to create or update a
     session entry in the `SessionStore`.
   - The agent is invoked via `bridge.invoke_agent_text_with_metadata()`.
   - The agent's response is sent back to the originating channel.
5. For `Approve`:
   - The approval resolution is forwarded to the exec approval system.

### Outbound (gateway to platform)

1. The gateway calls `bridge.send_platform_message()` with a channel address
   and text.
2. The channel address format is `platform:identifier` (e.g., `discord:12345`).
3. The platform prefix is used to select the correct bridge.
4. The bridge formats the message for the platform's API and sends it.
5. On failure, the runtime retries up to 3 times before logging a warning.

## Supported bridges

| Platform    | Webhook endpoint         | Auth mechanism           |
|-------------|--------------------------|--------------------------|
| Discord     | `/webhooks/discord`      | Ed25519 signature        |
| Telegram    | `/webhooks/telegram`     | Secret token header      |
| Slack       | `/webhooks/slack`        | HMAC-SHA256 signature    |
| Matrix      | `/webhooks/matrix`       | Bearer token             |
| Mattermost  | `/webhooks/mattermost`   | Token verification       |
| Google Chat | `/webhooks/googlechat`   | Bearer token             |
| LINE        | `/webhooks/line`         | Signature verification   |
| Feishu      | `/webhooks/feishu`       | Signature verification   |
| IRC         | `/webhooks/irc`          | None (local)             |
| MS Teams    | `/webhooks/msteams`      | Bearer token             |
| Webhook     | `/webhooks/webhook`      | HMAC-SHA256 (optional)   |
| Zalo        | `/webhooks/zalo`         | Signature verification   |
| WhatsApp    | `/webhooks/whatsapp`     | Signature verification   |
| Signal      | `/webhooks/signal`       | Local bridge             |
| Nostr       | `/webhooks/nostr`        | NIP-based                |
| Twitch      | `/webhooks/twitch`       | Signature verification   |

## Rich messages

Bridges can send structured messages via `RichMessage`:

```rust
struct RichMessage {
    text: String,                  // Main text content
    code_blocks: Vec<CodeBlock>,   // Code blocks with language tags
    title: Option<String>,         // Optional title/header
    color: Option<String>,         // Color accent (platform-specific)
}
```

Platforms render these differently: Discord uses embeds, Slack uses blocks,
Matrix uses HTML formatting, Telegram uses HTML parse mode.

## Credential management

Bridge credentials are configured in two ways:

### config.toml

```toml
[gateway.bridges.discord]
enabled = true
bot_token = "YOUR_BOT_TOKEN"
application_id = "YOUR_APP_ID"
application_public_key = "YOUR_PUBLIC_KEY"

[gateway.bridges.telegram]
enabled = true
bot_token = "123456789:ABCdef..."

[gateway.bridges.slack]
enabled = true
bot_token = "xoxb-..."
signing_secret = "..."

[gateway.bridges.webhook]
enabled = true
callback_url = "https://..."
secret = "hmac-secret"
```

### Environment variables

```bash
DISCORD_BOT_TOKEN=...
TELEGRAM_BOT_TOKEN=...
SLACK_BOT_TOKEN=...
WEBHOOK_SECRET=...
```

### Runtime hot-reload

Credentials can be updated at runtime via `config.patch` without restarting the
gateway. The `RuntimeBridgeSecrets` struct holds the current values and is
protected by an `RwLock`.

## Deduplication

The bridge runtime maintains a deduplication cache (`should_drop_duplicate()`)
with a 10-minute TTL. Duplicate webhook deliveries from platforms that retry on
timeout are silently dropped.

## Error handling and retries

When the agent invocation or message send fails:

1. The error is logged to the in-memory log store.
2. For outbound messages, the runtime retries up to 3 times.
3. If all retries fail, a warning is logged with the channel and error details.

## Channel status

The `channels.status` WS-RPC method reports the current state of each bridge:

```json
{
  "discord": { "configured": true, "running": true, "connected": true },
  "telegram": { "configured": false, "running": false, "connected": false }
}
```

A bridge is "configured" if credentials are present (either in config or
environment), "running" if the bridge has been started, and "connected" if it
has successfully authenticated with the platform API.
