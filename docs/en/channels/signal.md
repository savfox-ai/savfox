# Signal Channel

Connect Savfox to Signal via the signal-cli REST API.

## Prerequisites

- [signal-cli-rest-api](https://github.com/bbernhard/signal-cli-rest-api) running
- A registered Signal phone number

## Configuration

```toml
[[channels]]
type = "signal"
enabled = true

[channels.signal]
api_url = "http://localhost:8080"
phone_number = "+1234567890"
```

## Setup Steps

### 1. Run signal-cli REST API

```bash
docker run -d --name signal-api \
  -p 8080:8080 \
  -v signal-cli-config:/home/.local/share/signal-cli \
  bbernhard/signal-cli-rest-api
```

### 2. Register or Link a Number

Register a new number:
```bash
curl -X POST "http://localhost:8080/v1/register/+1234567890"
```

Or link to an existing Signal account:
```bash
curl -X GET "http://localhost:8080/v1/qrcodelink?device_name=savfox"
```

### 3. Verify Registration

```bash
curl -X POST "http://localhost:8080/v1/register/+1234567890/verify/123456"
```

## Features

| Feature | Support |
|---------|---------|
| Text messages | Full |
| Images | Receive + Send |
| Group messages | Full |
| Reactions | Receive |
| Read receipts | Send |
| Typing indicators | Send |

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `api_url` | Required | signal-cli REST API URL |
| `phone_number` | Required | Registered Signal number |
| `poll_interval_secs` | `2` | Polling interval for new messages |
| `allowed_numbers` | `[]` | Restrict to specific phone numbers |
| `trust_all_keys` | `true` | Auto-trust new identity keys |

## Security Notes

- Signal provides end-to-end encryption
- The signal-cli REST API should only be accessible locally
- Phone numbers in Signal are sensitive data — handle with care

## Troubleshooting

- **Not receiving messages**: Check signal-cli REST API is running and accessible
- **Registration failed**: Ensure the phone number can receive SMS
- **Identity key errors**: Set `trust_all_keys = true` or manually trust keys
