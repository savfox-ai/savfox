---
name: ssh
description: Manage SSH connections, keys, and remote operations.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🔐"
    requires:
      bins:
        - ssh
      env: []
    install: []
---

# SSH Skill

Manage SSH connections and remote operations.

## Connect

```bash
ssh user@host
ssh -p 2222 user@host
ssh -i ~/.ssh/id_ed25519 user@host
```

## Run Remote Command

```bash
ssh user@host "ls -la /var/log"
ssh user@host "cat /etc/hostname && uptime"
```

## Copy Files

Local to remote:
```bash
scp file.txt user@host:/remote/path/
scp -r local-dir/ user@host:/remote/path/
```

Remote to local:
```bash
scp user@host:/remote/file.txt ./
```

## SSH Keys

Generate key:
```bash
ssh-keygen -t ed25519 -C "your@email.com"
```

Copy key to server:
```bash
ssh-copy-id user@host
```

## Port Forwarding

Local forwarding (access remote service locally):
```bash
ssh -L 8080:localhost:80 user@host
```

Remote forwarding (expose local service remotely):
```bash
ssh -R 8080:localhost:3000 user@host
```

SOCKS proxy:
```bash
ssh -D 1080 user@host
```

## SSH Config

Edit `~/.ssh/config`:
```
Host myserver
    HostName 192.168.1.100
    User admin
    Port 22
    IdentityFile ~/.ssh/id_ed25519
```

Then connect with: `ssh myserver`

## Guidelines

- Use Ed25519 keys (stronger and shorter than RSA)
- Never share private keys
- Use SSH config for frequently accessed hosts
- Use `-o StrictHostKeyChecking=no` only for testing
- Use `ssh-agent` to avoid typing passphrases repeatedly
