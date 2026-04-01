---
name: spotify
description: Control Spotify playback, search music, and manage playlists.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🎵"
    requires:
      bins:
        - spotify_player
      env:
        - SPOTIFY_CLIENT_ID
    install:
      - id: cargo
        kind: cargo
        crate_name: spotify_player
        bins: [spotify_player]
        label: Cargo (cross-platform)
      - id: brew
        kind: brew
        formula: spotify_player
        bins: [spotify_player]
        label: Homebrew (macOS)
---

# Spotify Player Skill

Control Spotify playback using `spotify_player` CLI or the Spotify Web API.

## Using spotify_player CLI

Search for a track:
```bash
spotify_player search "bohemian rhapsody"
```

Play a track:
```bash
spotify_player play --name "Bohemian Rhapsody"
```

Pause/Resume:
```bash
spotify_player playback pause
spotify_player playback resume
```

Current playback:
```bash
spotify_player playback
```

## Using Spotify Web API

Get current playback:
```bash
curl -s https://api.spotify.com/v1/me/player \
  -H "Authorization: Bearer $SPOTIFY_TOKEN" | jq '{track: .item.name, artist: .item.artists[0].name, is_playing}'
```

Search:
```bash
curl -s "https://api.spotify.com/v1/search?q=bohemian+rhapsody&type=track&limit=5" \
  -H "Authorization: Bearer $SPOTIFY_TOKEN" | jq '.tracks.items[] | {name, artist: .artists[0].name}'
```

## Guidelines

- spotify_player requires Spotify Premium for playback control
- Web API requires OAuth token with appropriate scopes
- Use `user-modify-playback-state` scope for play/pause/skip
- Use `user-read-playback-state` scope for current track info
