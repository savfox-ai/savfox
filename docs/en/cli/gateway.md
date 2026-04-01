# Gateway Subcommands (`savfox gateway`)

The `gateway` subcommand starts and manages the Savfox gateway server. The gateway provides HTTP/WebSocket access for web clients, chat platform bridges, and multi-device usage.

## Starting the Server

```bash
savfox gateway
savfox gateway --port 8080
savfox gateway --port 8080 --token my-secret-token
savfox gateway --host 0.0.0.0 --port 443 --tls-cert cert.pem --tls-key key.pem
```

| Flag | Default | Description |
|------|---------|-------------|
| `--host <ADDR>` | `127.0.0.1` | Bind address |
| `--port <PORT>` | `18881` | Listen port |
| `--token <TOKEN>` | Auto-generated | Bearer token for authentication |
| `--tls-cert <PATH>` | -- | TLS certificate file for HTTPS/WSS |
| `--tls-key <PATH>` | -- | TLS private key file |

If `--token` is omitted, a random token is generated and printed at startup. Copy it for client use.

## Management Subcommands

All management subcommands connect to a running gateway instance.

### `status`

Check whether the gateway is running and healthy:

```bash
savfox gateway status
```

Returns the server version, uptime, active session count, and connected client count.

### `logs`

View or stream server logs:

```bash
savfox gateway logs              # Show recent log entries
savfox gateway logs --follow     # Stream logs in real time
savfox gateway logs --lines 200  # Show last 200 lines
```

| Flag | Description |
|------|-------------|
| `--follow` | Tail the log stream (like `tail -f`) |
| `--lines <N>` | Number of recent lines to display |

### `models`

List all LLM models available through the configured providers:

```bash
savfox gateway models
```

This queries each enabled provider and returns a combined model list with provider name, model ID, and capabilities.

### `approvals`

Manage execution approval requests from remote clients. When the agent needs permission to run a command, the request appears here:

```bash
savfox gateway approvals list
savfox gateway approvals approve <REQUEST_ID>
savfox gateway approvals deny <REQUEST_ID> --reason "Not safe to run"
```

| Subcommand | Description |
|------------|-------------|
| `list` | Show all pending approval requests |
| `approve <ID>` | Approve a pending request |
| `deny <ID>` | Deny a request (optional `--reason`) |

### `devices`

Manage paired devices. Device pairing allows phones, tablets, or other machines to connect to the gateway:

```bash
savfox gateway devices list
savfox gateway devices pair --name "My Phone"
savfox gateway devices revoke <DEVICE_ID>
```

| Subcommand | Description |
|------------|-------------|
| `list` | Show all paired devices |
| `pair` | Generate a pairing token for a new device |
| `revoke <ID>` | Revoke access for a device |

### `channels`

List and manage chat bridge channel integrations:

```bash
savfox gateway channels
```

Displays all configured bridges (Discord, Telegram, Slack, etc.) and their connection status.

### `nodes`

Manage connected nodes in a multi-node deployment:

```bash
savfox gateway nodes
```

Shows each connected node's ID, role, and last heartbeat time.

## Configuration

Gateway settings live in `~/.savfox/config.toml`:

```toml
[gateway]
port = 18881
host = "127.0.0.1"
token = "my-secret-token"
```

Override any value from the CLI:

```bash
savfox -c gateway.port=9090 gateway
```

## Examples

```bash
# Start on a custom port with auto-generated token
savfox gateway --port 9090

# Start with TLS for production use
savfox gateway --host 0.0.0.0 --port 443 --tls-cert /etc/ssl/cert.pem --tls-key /etc/ssl/key.pem

# Check health from another terminal
savfox gateway status

# Watch logs while debugging a bridge
savfox gateway logs --follow

# Approve a pending execution
savfox gateway approvals list
savfox gateway approvals approve abc123
```
