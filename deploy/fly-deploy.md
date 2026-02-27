# Deploying Savfox Gateway on Fly.io

## Prerequisites

1. Install the Fly CLI: https://fly.io/docs/flyctl/install/
2. Authenticate: `fly auth login`

## Initial Setup

```bash
# Create the app (first time only)
fly apps create savfox-gateway

# Create a persistent volume for session data (1 GB, expand later if needed)
fly volumes create savfox_data --region iad --size 1

# Set required secrets
fly secrets set SAVFOX_GATEWAY_TOKEN="<your-gateway-token>"

# Set provider API keys (add whichever you need)
fly secrets set OPENAI_API_KEY="sk-..."
fly secrets set ANTHROPIC_API_KEY="sk-ant-..."
fly secrets set GROQ_API_KEY="gsk_..."
fly secrets set DEEPSEEK_API_KEY="..."
fly secrets set XAI_API_KEY="..."
fly secrets set MISTRAL_API_KEY="..."
fly secrets set TOGETHER_API_KEY="..."
```

## Deploy

```bash
# From the repository root
fly deploy -c deploy/fly.toml
```

The first deploy will build the Docker image on Fly's remote builders. Subsequent
deploys reuse cached layers and are faster.

## Verify

```bash
# Check app status
fly status -c deploy/fly.toml

# Stream logs
fly logs -c deploy/fly.toml

# Hit the health endpoint
curl https://savfox-gateway.fly.dev/health

# Validate your token
curl -H "Authorization: Bearer <your-token>" \
     https://savfox-gateway.fly.dev/api/token/validate
```

## Scaling

The default configuration runs 1 machine with 256 MB RAM on a shared CPU.
Adjust as needed:

```bash
# Scale memory
fly scale memory 512 -c deploy/fly.toml

# Scale to a dedicated CPU
fly scale vm shared-cpu-2x -c deploy/fly.toml

# Add machines in another region
fly scale count 2 --region iad,cdg -c deploy/fly.toml
```

## Volume Management

Session data, cron jobs, and memory entries are stored in the `/data` volume.

```bash
# List volumes
fly volumes list -c deploy/fly.toml

# Extend volume size
fly volumes extend <vol_id> --size 5
```

## Secrets Reference

| Secret                  | Required | Description                          |
|-------------------------|----------|--------------------------------------|
| SAVFOX_GATEWAY_TOKEN    | Yes      | Auth token for WebSocket/HTTP access |
| OPENAI_API_KEY          | No       | OpenAI API key                       |
| ANTHROPIC_API_KEY       | No       | Anthropic API key                    |
| GROQ_API_KEY            | No       | Groq API key                         |
| DEEPSEEK_API_KEY        | No       | DeepSeek API key                     |
| XAI_API_KEY             | No       | xAI (Grok) API key                   |
| MISTRAL_API_KEY         | No       | Mistral API key                      |
| TOGETHER_API_KEY        | No       | Together AI API key                  |
| OPENROUTER_API_KEY      | No       | OpenRouter API key                   |

## Troubleshooting

```bash
# SSH into the running machine
fly ssh console -c deploy/fly.toml

# Check disk usage
fly ssh console -c deploy/fly.toml -C "df -h /data"

# Restart the app
fly apps restart savfox-gateway
```

## Custom Domain

```bash
# Add a custom domain
fly certs add gateway.yourdomain.com -c deploy/fly.toml

# Then create a CNAME record: gateway.yourdomain.com -> savfox-gateway.fly.dev
```
