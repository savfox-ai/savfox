# Signal Channel Setup

Connect Savfox to Signal using the signal-cli channel, allowing users to interact
with the agent via Signal messages.

## Prerequisites

- A phone number registered with Signal.
- [signal-cli](https://github.com/AsamK/signal-cli) installed and configured
  with JSON-RPC mode.
- A running Savfox gateway server.

## Step 1: Install and register signal-cli

```bash
# Install signal-cli (example for Linux)
wget https://github.com/AsamK/signal-cli/releases/latest/download/signal-cli-*.tar.gz
tar xzf signal-cli-*.tar.gz
sudo mv signal-cli-*/bin/signal-cli /usr/local/bin/

# Register your phone number
signal-cli -u +1234567890 register
signal-cli -u +1234567890 verify CODE_FROM_SMS
```

## Step 2: Start signal-cli in JSON-RPC mode

```bash
signal-cli -u +1234567890 jsonRpc --socket unix:///tmp/signal-cli.sock
```

Or with HTTP:

```bash
signal-cli -u +1234567890 jsonRpc --http 127.0.0.1:8080
```

## Step 3: Configure the gateway

```toml
[gateway.channels.signal]
enabled = true
phone_number = "+1234567890"
rpc_url = "http://127.0.0.1:8080/api/v1/rpc"
```

## Step 4: Set up webhook forwarding

Configure signal-cli to forward incoming messages to the gateway webhook:

```
https://your-gateway-host:18881/webhooks/signal
```

The exact mechanism depends on your signal-cli version and configuration.

## Usage

Send a message to the registered Signal number. The channel processes incoming
messages and routes them to the Savfox agent. Replies are sent back via the
signal-cli JSON-RPC API.

## Limitations

- Signal does not support official bot accounts or business APIs. This channel
  uses signal-cli as an unofficial client.
- Group messaging support depends on signal-cli configuration.
- signal-cli must remain running for the channel to function.

## Troubleshooting

- **No responses**: Verify signal-cli is running and the RPC URL is correct.
- **Registration issues**: Signal may require re-verification if the number
  has been inactive.
- **Rate limiting**: Signal imposes rate limits on message sending. Avoid
  sending too many messages in quick succession.
