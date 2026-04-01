---
name: whisper
description: Transcribe and translate audio files using OpenAI Whisper.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🎤"
    requires:
      env:
        - OPENAI_API_KEY
    install: []
---

# OpenAI Whisper Skill

Transcribe audio to text and translate audio to English using the OpenAI Audio API.

## Transcribe Audio

```bash
curl -s https://api.openai.com/v1/audio/transcriptions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -F file=@audio.mp3 \
  -F model=whisper-1 \
  -F response_format=text
```

## Translate Audio to English

```bash
curl -s https://api.openai.com/v1/audio/translations \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -F file=@foreign_audio.mp3 \
  -F model=whisper-1
```

## With Timestamps

Get word-level or segment-level timestamps:
```bash
curl -s https://api.openai.com/v1/audio/transcriptions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -F file=@audio.mp3 \
  -F model=whisper-1 \
  -F response_format=verbose_json \
  -F timestamp_granularities[]=word
```

## Supported Formats

- mp3, mp4, mpeg, mpga, m4a, wav, webm
- Maximum file size: 25 MB

## Response Formats

- `json` — JSON with text field
- `text` — plain text
- `srt` — SubRip subtitles
- `verbose_json` — JSON with timestamps and metadata
- `vtt` — WebVTT subtitles

## Guidelines

- For long audio, split into chunks under 25 MB
- Provide `language` parameter for better accuracy on known languages
- Use `prompt` parameter to guide the model (e.g., proper nouns, technical terms)
