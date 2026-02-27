---
name: network
description: Network diagnostics, DNS lookups, and port scanning.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🌐"
    requires:
      bins: []
      env: []
    install: []
---

# Network Skill

Network diagnostics and troubleshooting.

## DNS Lookup

```bash
dig example.com
nslookup example.com
host example.com
```

Specific record type:
```bash
dig example.com MX
dig example.com TXT
dig example.com AAAA
```

## Connectivity

Ping:
```bash
ping -c 4 example.com
```

Traceroute:
```bash
traceroute example.com
```

## Port Checking

Check if port is open:
```bash
nc -zv host 443
```

List listening ports:
```bash
ss -tlnp
netstat -tlnp
```

## HTTP Testing

Check headers:
```bash
curl -I https://example.com
```

Check SSL certificate:
```bash
openssl s_client -connect example.com:443 -servername example.com </dev/null 2>/dev/null | openssl x509 -noout -dates
```

## IP Info

Local IP:
```bash
ip addr show
hostname -I
```

Public IP:
```bash
curl -s ifconfig.me
curl -s ipinfo.io/json
```

## Bandwidth

```bash
curl -o /dev/null -w "Speed: %{speed_download} bytes/sec\n" https://speed.cloudflare.com/__down?bytes=10000000
```

## Guidelines

- Use `dig` over `nslookup` for more detailed DNS info
- Use `ss` over `netstat` (newer, faster)
- Use `nc -zv` for quick port checks without full connections
- Use `mtr` for combined ping+traceroute
