# Bridge Architecture

Bridges connect external chat platforms to the Savfox gateway, allowing the agent to communicate across multiple channels.

## How Bridges Work

```
[Discord] ──┐
[Telegram] ──┤
[Slack] ─────┤──→ [Gateway Bridge Router] ──→ [Agent Engine]
[Matrix] ────┤
[Webhook] ───┘
```

1. A message arrives on an external platform
2. The bridge converts it to a normalized `IncomingMessage`
3. The gateway routes it to the appropriate session
4. The agent processes the message and generates a response
5. The bridge converts the response back to the platform's format

## Bridge Trait

All bridges implement the `ChatBridge` trait:

```rust
#[async_trait]
pub trait ChatBridge: Send + Sync {
    async fn start(&mut self) -> Result<()>;
    async fn send_message(&self, channel: &str, text: &str) -> Result<()>;
    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> Result<()>;
    async fn handle_webhook(&self, req: &mut Request, res: &mut Response) -> Result<()>;
}
```

## Available Bridges

| Bridge | Protocol | Auth Method |
|--------|----------|-------------|
| Discord | WebSocket (Gateway API) | Bot Token |
| Telegram | HTTPS (Bot API) | Bot Token |
| Slack | WebSocket (Socket Mode) | Bot Token + App Token |
| Matrix | HTTPS (Client-Server API) | Access Token |
| Mattermost | WebSocket | Personal Access Token |
| Google Chat | HTTPS (Webhook) | Service Account |
| Line | HTTPS (Messaging API) | Channel Access Token |
| Feishu/Lark | HTTPS (Bot API) | App ID + Secret |
| IRC | TCP | SASL / NickServ |
| WhatsApp | HTTPS (Business API) | Access Token |
| Signal | HTTPS (signal-cli-rest) | Phone Number |
| MS Teams | HTTPS (Bot Framework) | App ID + Secret |
| Nostr | WebSocket (Relay) | Private Key |
| Twitch | IRC (TMI) | OAuth Token |
| Webhook | HTTPS | HMAC Secret |

## Message Flow

### Incoming

```
Platform → Bridge.handle_webhook() → IncomingMessage → Session → Agent
```

### Outgoing

```
Agent → Response → Session → Bridge.send_message() → Platform
```

## Configuration

Bridges are configured in `config.toml`:

```toml
[[channels]]
type = "discord"
enabled = true

[channels.discord]
bot_token = "${DISCORD_BOT_TOKEN}"
```

## Adding a New Bridge

1. Create `crates/gateway-server/src/bridges/<name>.rs`
2. Implement `ChatBridge` trait
3. Add config struct to `crates/gateway-server/src/config.rs`
4. Register in `crates/gateway-server/src/bridges/mod.rs`
5. Add to bridge runtime initialization in `runtime.rs`

## See Also

- [Channel Configuration Guides](../channels/discord.md)
- [Webhook Bridge](../channels/webhook.md)
