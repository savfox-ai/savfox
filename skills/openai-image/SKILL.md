---
name: openai-image
description: Generate, edit, and create variations of images using OpenAI DALL-E.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🎨"
    requires:
      env:
        - OPENAI_API_KEY
    install: []
---

# OpenAI Image Generation Skill

Generate images using the OpenAI Images API (DALL-E 3 / gpt-image-1).

## Generate an Image

```bash
curl -s https://api.openai.com/v1/images/generations \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-image-1",
    "prompt": "A serene mountain landscape at sunset with a reflective lake",
    "n": 1,
    "size": "1024x1024"
  }' | jq '.data[0].url'
```

## Available Sizes

- `1024x1024` (square, default)
- `1792x1024` (landscape)
- `1024x1792` (portrait)
- `256x256` (small, DALL-E 2 only)
- `512x512` (medium, DALL-E 2 only)

## Quality Options

- `standard` — default quality
- `hd` — enhanced detail (DALL-E 3+ only)

## Guidelines

- Be specific and detailed in prompts for best results
- Include art style, lighting, composition, and mood descriptions
- DALL-E 3 auto-rewrites prompts for better results
- Response includes URL that expires after 1 hour — download immediately if needed
- Use `response_format: "b64_json"` for base64-encoded response
