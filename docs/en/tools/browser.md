# Browser Automation

Savfox agents can interact with web pages using browser automation tools.

## Overview

The browser tool allows agents to:
- Navigate to URLs
- Read page content
- Click elements
- Fill forms
- Take screenshots
- Execute JavaScript

## Usage

The agent can use browser automation when instructed:

```
Browse to https://example.com and extract the main heading
```

## Configuration

```toml
[tools.browser]
enabled = true
headless = true
timeout_secs = 30
allowed_domains = []  # empty = all domains
blocked_domains = ["*.internal.company.com"]
```

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `enabled` | `true` | Enable browser tool |
| `headless` | `true` | Run browser without GUI |
| `timeout_secs` | `30` | Page load timeout |
| `allowed_domains` | `[]` | Whitelist domains (empty = all) |
| `blocked_domains` | `[]` | Blacklist domains |
| `max_pages` | `5` | Max concurrent pages |
| `user_agent` | Auto | Custom user agent string |

## Actions

| Action | Description |
|--------|-------------|
| `navigate` | Go to a URL |
| `click` | Click an element by selector |
| `type` | Type text into an input field |
| `screenshot` | Capture page screenshot |
| `content` | Get page text content |
| `html` | Get page HTML |
| `evaluate` | Run JavaScript on the page |
| `wait` | Wait for an element to appear |
| `scroll` | Scroll the page |
| `back` | Navigate back |
| `forward` | Navigate forward |

## Security

- Browser runs in a sandboxed environment
- Domain restrictions prevent access to internal services
- JavaScript execution is isolated per page
- Cookies and session data are cleared between tasks
- No access to local filesystem from browser context

## Limitations

- Heavy pages may hit memory limits in the sandbox
- Some sites block automated browsers
- CAPTCHAs cannot be solved automatically
- WebSocket-heavy SPAs may not render fully in headless mode
