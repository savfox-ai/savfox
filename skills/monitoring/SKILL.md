---
name: monitoring
description: System monitoring and performance analysis.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📈"
    requires:
      bins: []
      env: []
    install: []
---

# Monitoring Skill

System monitoring and performance analysis.

## System Overview

```bash
top -bn1 | head -20
htop  # interactive (if installed)
```

## CPU

```bash
uptime
mpstat 1 5  # 5 samples, 1 second apart
```

## Memory

```bash
free -h
vmstat 1 5
```

## Disk

Usage:
```bash
df -h
du -sh /path/* | sort -rh | head -10
```

I/O:
```bash
iostat -x 1 5
```

## Network

Connections:
```bash
ss -s  # summary
ss -tlnp  # listening TCP
```

Traffic:
```bash
iftop  # interactive (if installed)
```

## Processes

Top CPU consumers:
```bash
ps aux --sort=-%cpu | head -10
```

Top memory consumers:
```bash
ps aux --sort=-%mem | head -10
```

Find process:
```bash
pgrep -la "process-name"
```

## Docker Monitoring

```bash
docker stats --no-stream
docker system df
```

## Log Monitoring

Watch for errors:
```bash
journalctl -f -p err
tail -f /var/log/syslog | grep -i error
```

## Guidelines

- Use `htop` over `top` for better UI (install if missing)
- Use `ss` over `netstat` (faster, more info)
- Use `iostat` to diagnose disk bottlenecks
- Use `vmstat` to identify memory pressure
- Combine with cron for periodic health checks
