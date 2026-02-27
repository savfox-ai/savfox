# Savfox Gateway Service Installation

## Linux (systemd)

Copy the service file and enable it:

```bash
mkdir -p ~/.config/systemd/user/
cp deploy/savfox-gateway.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable savfox-gateway
systemctl --user start savfox-gateway
```

Check status:

```bash
systemctl --user status savfox-gateway
journalctl --user -u savfox-gateway -f
```

## macOS (launchd)

Edit the plist to replace `USER` with your username, then install:

```bash
sed "s/USER/$(whoami)/g" deploy/ai.savfox.gateway.plist > ~/Library/LaunchAgents/ai.savfox.gateway.plist
launchctl load ~/Library/LaunchAgents/ai.savfox.gateway.plist
```

Check status:

```bash
launchctl list | grep savfox
tail -f /tmp/savfox-gateway.log
```

To stop and unload:

```bash
launchctl unload ~/Library/LaunchAgents/ai.savfox.gateway.plist
```

## Configuration

Set the gateway token via environment variable or config file:

```bash
# Environment variable
export SAVFOX_GATEWAY_TOKEN=your-secret-token

# Or in ~/.savfox/config.toml
[gateway]
token = "your-secret-token"
port = 18881
```
