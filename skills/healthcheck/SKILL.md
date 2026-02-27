---
name: healthcheck
description: "Check the health and availability of HTTP endpoints and services"
version: "1.0.0"
metadata:
  savfox:
    emoji: "\U0001F3E5"
    requires:
      bins: ["curl"]
    install:
      - id: brew-curl
        kind: brew
        formula: curl
        bins: [curl]
        label: "Install curl via Homebrew"
      - id: apt-curl
        kind: apt
        package: curl
        bins: [curl]
        label: "Install curl via apt"
---
# Healthcheck Skill

You can check the health and availability of HTTP endpoints using `curl`. This is useful for monitoring services, debugging connectivity issues, and verifying deployments.

## Basic Health Check

Check if an endpoint is responding:

```bash
curl -s -o /dev/null -w "%{http_code}" "https://example.com/health"
```

This returns just the HTTP status code (e.g. `200`, `503`).

## Detailed Health Check

Get full response details including timing:

```bash
curl -s -o /dev/null -w "status: %{http_code}\ntime_total: %{time_total}s\ntime_connect: %{time_connect}s\ntime_starttransfer: %{time_starttransfer}s\nsize: %{size_download} bytes\n" "https://example.com/health"
```

## Check with Response Body

When the health endpoint returns useful JSON:

```bash
curl -s -w "\n--- HTTP %{http_code} in %{time_total}s ---" "https://example.com/health"
```

## Multiple Endpoints

To check several endpoints in sequence, run individual curl commands for each and report results together.

## SSL Certificate Check

Verify the TLS certificate:

```bash
curl -vI "https://example.com" 2>&1 | grep -E "expire|subject|issuer|SSL"
```

## Response Headers

Inspect response headers for security and caching configuration:

```bash
curl -sI "https://example.com"
```

Look for:
- `Strict-Transport-Security` (HSTS)
- `Content-Security-Policy`
- `X-Content-Type-Options`
- `X-Frame-Options`
- `Cache-Control`

## Port Connectivity

Check if a specific port is reachable (TCP-level):

```bash
curl -s --connect-timeout 5 "http://host:port/" -o /dev/null -w "%{http_code}"
```

Use `--connect-timeout` to avoid long waits on unresponsive hosts.

## Interpreting Results

| Status Code | Meaning               | Action                        |
|-------------|-----------------------|-------------------------------|
| 200         | Healthy               | Service is running normally   |
| 301/302     | Redirect              | Follow redirect or update URL |
| 401/403     | Auth required/denied  | Check credentials/tokens      |
| 404         | Not found             | Verify the endpoint path      |
| 500         | Internal server error | Check service logs             |
| 502         | Bad gateway           | Upstream service may be down  |
| 503         | Service unavailable   | Service is overloaded/starting|
| 000         | Connection failed     | DNS, network, or firewall issue|

## Guidelines

1. Always use `--connect-timeout 10` and `--max-time 30` to avoid hanging on unresponsive endpoints.
2. When checking multiple endpoints, report all results together in a table format.
3. For production monitoring, suggest the user set up proper uptime monitoring rather than relying on ad-hoc checks.
4. If an endpoint requires authentication, prompt the user for the token or API key -- never guess credentials.
5. When timing is important, use the `-w` format string to report `time_connect`, `time_starttransfer`, and `time_total`.
6. Compare response times to typical baselines: <200ms is fast, 200-1000ms is normal, >1s may indicate issues.
7. If a health check fails, suggest checking DNS resolution (`nslookup`), network path (`traceroute`), and service logs as next steps.
