# Nodes

Nodes are devices that connect to the gateway and extend the agent's capabilities with hardware access.

## What is a Node?

A node is a client device (phone, tablet, IoT device) that:
- Connects to the gateway via WebSocket
- Provides hardware capabilities (camera, microphone, GPS, screen)
- Receives commands from the agent
- Sends sensor data back to the gateway

## Node Capabilities

| Capability | Description | Platforms |
|------------|-------------|-----------|
| Camera | Capture photos/video | iOS, Android |
| Microphone | Record audio | iOS, Android |
| Location | GPS coordinates | iOS, Android |
| Screen | Screen capture/recording | iOS, Android, Desktop |
| Calendar | Read/write events | iOS, Android |
| Contacts | Read contacts | iOS, Android |
| SMS | Send/read messages | Android |
| Canvas | Render agent-driven UI | iOS, Android |
| TTS | Text-to-speech playback | iOS, Android |
| Voice | Voice wake + talk mode | iOS, Android |

## Connection Flow

1. Node discovers gateway via mDNS/Bonjour or manual URL
2. Node connects via WebSocket to `/ws`
3. Node authenticates with a device token
4. Node announces its capabilities
5. Gateway routes capability requests to the node

## Configuration

Nodes register via the pairing flow:

```json
{
  "method": "devices.pair",
  "params": {
    "name": "My iPhone",
    "capabilities": ["camera", "microphone", "location", "screen"]
  }
}
```

## Managing Nodes

### List Connected Nodes

```bash
savfox gateway devices
```

### Via WS-RPC

```json
{"method": "devices.list", "params": {}, "id": 1}
```

## Security

- Nodes must complete the pairing flow before accessing agent functions
- Each node gets a scoped device token
- Capability access requires explicit grant
- Nodes can be revoked at any time

## See Also

- [Device Pairing](pairing.md)
- [Gateway Protocol](../gateway/protocol.md)
