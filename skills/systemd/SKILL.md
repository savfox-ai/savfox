---
name: systemd
description: Manage Linux services and system state with systemctl and journalctl.
version: "1.0.0"
metadata:
  savfox:
    emoji: "⚙️"
    requires:
      bins:
        - systemctl
      env: []
    install:
      - id: manual
        kind: manual
        instructions: "systemd is built into most Linux distributions."
        bins: [systemctl]
        label: Built-in (Linux)
---

# Systemd Skill

Manage Linux services and system state.

## Service Management

Start/stop/restart:
```bash
sudo systemctl start nginx
sudo systemctl stop nginx
sudo systemctl restart nginx
```

Reload config (without restart):
```bash
sudo systemctl reload nginx
```

Enable on boot:
```bash
sudo systemctl enable nginx
sudo systemctl disable nginx
```

## Service Status

```bash
systemctl status nginx
systemctl is-active nginx
systemctl is-enabled nginx
```

## List Services

Running services:
```bash
systemctl list-units --type=service --state=running
```

All services:
```bash
systemctl list-units --type=service
```

Failed services:
```bash
systemctl --failed
```

## Logs with journalctl

View service logs:
```bash
journalctl -u nginx --since today
journalctl -u nginx -f  # follow
journalctl -u nginx --no-pager -n 100
```

Kernel logs:
```bash
journalctl -k
```

Boot logs:
```bash
journalctl -b
```

## Create a Service

Create `/etc/systemd/system/myapp.service`:
```ini
[Unit]
Description=My Application
After=network.target

[Service]
ExecStart=/usr/local/bin/myapp
Restart=always
User=myapp
WorkingDirectory=/opt/myapp

[Install]
WantedBy=multi-user.target
```

Then:
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now myapp
```

## Timers (Cron Alternative)

List timers:
```bash
systemctl list-timers
```

## Guidelines

- Always run `daemon-reload` after editing unit files
- Use `journalctl` instead of checking log files directly
- Use `enable --now` to enable and start simultaneously
- Check `systemctl --failed` regularly for broken services
