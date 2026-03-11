# Device Pairing

Nodes must complete a pairing flow before they can interact with the gateway.

## Pairing Flow

### 1. Discovery

The node discovers the gateway via:
- **mDNS/Bonjour**: Automatic discovery on local network
- **Manual URL**: User enters the gateway address
- **QR Code**: Scan from the gateway web UI

### 2. Pairing Request

The node sends a pairing request:
```json
{
  "method": "devices.pair.request",
  "params": {
    "name": "My iPhone",
    "platform": "ios",
    "capabilities": ["camera", "microphone", "location"]
  }
}
```

### 3. Operator Approval

The gateway notifies connected operators:
```json
{
  "method": "devices.pair.pending",
  "params": {
    "device_id": "abc123",
    "name": "My iPhone",
    "platform": "ios"
  }
}
```

The operator approves or denies:
```json
{
  "method": "devices.pair.approve",
  "params": {
    "device_id": "abc123"
  }
}
```

### 4. Token Issued

On approval, the gateway issues a device token:
```json
{
  "result": {
    "token": "dev_abc123...",
    "device_id": "abc123",
    "expires_at": "2026-03-13T00:00:00Z"
  }
}
```

### 5. Connection

The node reconnects using its device token for all future sessions.

## Managing Paired Devices

### List Devices

```bash
savfox gateway devices
```

### Revoke a Device

```bash
savfox gateway devices revoke <device-id>
```

### Via WS-RPC

```json
{"method": "devices.revoke", "params": {"device_id": "abc123"}, "id": 1}
```

## Security Considerations

- Device tokens are scoped to the specific capabilities granted during pairing
- Tokens expire and must be refreshed
- Revoking a device immediately disconnects it
- Pairing requests from unknown networks should be treated with caution

## QR Code Pairing

The gateway web UI displays a QR code containing:
```
savfox://pair?host=192.168.1.100&port=18881&code=ABCD1234
```

The node app scans this code to initiate pairing without manual URL entry.
