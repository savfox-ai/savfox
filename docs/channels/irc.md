# IRC Channel Setup

Connect Savfox to IRC networks through an HTTP channel service.

## Prerequisites

- An IRC client or channel service with HTTP API support.
- A running Savfox gateway server.

## Architecture

IRC does not have a native HTTP API. The Savfox gateway communicates with IRC
through a local HTTP channel service that translates between HTTP and the IRC
protocol. The channel service handles:

- Connecting to the IRC server and joining channels.
- Receiving messages and forwarding them to the gateway webhook.
- Accepting outbound messages via HTTP POST and sending them to IRC channels.

## Step 1: Set up an IRC channel

You need a service that:

1. Connects to your IRC network.
2. Forwards incoming messages to `https://your-gateway-host:18881/webhooks/irc`
   as HTTP POST requests.
3. Exposes a `/send` endpoint for outbound messages.

Several open-source IRC-to-HTTP channels exist. Configure one to connect to your
IRC server and channel.

## Step 2: Configure the gateway

Set the channel URL via environment variable:

```bash
export IRC_BRIDGE_URL="http://127.0.0.1:6667"
```

The default channel URL is `http://127.0.0.1:6667` if not set.

## Step 3: Webhook format

The gateway expects inbound webhooks at `/webhooks/irc` with a JSON body
containing the message details (channel, sender, text).

Outbound messages are sent to the channel's `/send` endpoint:

```json
{
  "channel": "#mychannel",
  "message": "Hello from Savfox!"
}
```

## Channel addressing

Use `irc:<channel_name>` for programmatic messaging:

```json
{
  "channel": "irc:#mychannel",
  "text": "Hello from Savfox!"
}
```

## Troubleshooting

- **Channel not responding**: Verify the channel service is running and the
  `IRC_BRIDGE_URL` is correct.
- **Messages not forwarded**: Check that the channel is configured to POST
  messages to the gateway webhook URL.
- **Connection drops**: IRC connections can be unstable. Ensure the channel
  service has reconnection logic.
