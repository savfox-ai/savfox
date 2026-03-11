# Channel Architecture

Channels connect external chat platforms to the Savfox gateway, allowing the agent to communicate across multiple channels.

## How Channels Work

```
[Discord] ──┐
[Telegram] ──┤
[Slack] ─────┤──→ [Gateway Channel Router] ──→ [Agent Engine]
[Matrix] ────┤
[Webhook] ───┘
```

1. A message arrives on an external platform
2. The Channel converts it to a normalized `IncomingMessage`
3. The gateway routes it to the appropriate session
4. The agent processes the message and generates a response
5. The Channel converts the response back to the platform's format

## Channel Trait

All Channels implement the `ChatChannel` trait:

```rust
#[async_trait]
pub trait ChatChannel: Send + Sync {
    async fn start(&mut self) -> Result<()>;
    async fn send_message(&self, channel: &str, text: &str) -> Result<()>;
    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> Result<()>;
    async fn handle_webhook(&self, req: &mut Request, res: &mut Response) -> Result<()>;
}
```

## Available Channels

| Channel | Protocol | Auth Method |
|--------|----------|-------------|
| Discord | WebSocket (Gateway API) | Bot Token |
| Telegram | HTTPS (Bot API) | Bot Token |
| Slack | WebSocket (Socket Mode) | Bot Token + App Token |
| Matrix | HTTPS (Client-Server API) | Access Token |
| Mattermost | WebSocket | Personal Access Token |
| Google Chat | HTTPS (Webhook) | Service Account |
| Line | HTTPS (Messaging API) | Channel Access Token |
| Feishu/Lark | HTTPS (Bot API) + Webhook/Stream events | App ID + Secret |
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
Platform → Channel.handle_webhook() → IncomingMessage → Session → Agent
```

### Outgoing

```
Agent → Response → Session → Channel.send_message() → Platform
```

## Configuration

Channels are configured in `config.toml`:

```toml
[[channels]]
type = "discord"
enabled = true

[channels.discord]
bot_token = "${DISCORD_BOT_TOKEN}"
```

## Adding a New Channel

1. Create `crates/gateway-server/src/Channels/<name>.rs`
2. Implement `ChatChannel` trait
3. Add config struct to `crates/gateway-server/src/config.rs`
4. Register in `crates/gateway-server/src/Channels/mod.rs`
5. Add to Channel runtime initialization in `runtime.rs`

## See Also

- [Channel Configuration Guides](../channels/discord.md)
- [Webhook Channel](../channels/webhook.md)
