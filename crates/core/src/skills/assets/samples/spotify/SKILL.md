---
name: spotify
description: Control Spotify playback, manage playlists, search for music, and get recommendations. Use when the user wants to play music, pause/resume playback, skip tracks, manage their Spotify library, or discover new music.
metadata:
  short-description: Control Spotify playback and playlists
---

# Spotify Control

Control Spotify through the Spotify Web API.

## Authentication

Requires OAuth authentication with Spotify. The user must authorize access to their Spotify account.

## Playback Control

### Basic Controls

```
Play music
Pause playback
Skip to next track
Go to previous track
Set volume to <0-100>
```

### Track Operations

```
Play <song name>
Play <song name> by <artist>
Play album <album name>
Play playlist <playlist name>
Queue <song name>
```

## Playlist Management

### Create and Edit

```
Create playlist <name>
Add <song> to <playlist>
Remove <song> from <playlist>
Delete playlist <name>
```

### Viewing

```
Show my playlists
What's in <playlist name>?
```

## Search and Discovery

```
Search for <song/artist/album>
Show top tracks by <artist>
Recommend similar songs to <song>
What's new from <artist>?
```

## Information Queries

```
What's currently playing?
Show my recently played
What are my top tracks?
Show my saved albums
```

## Best Practices

1. Confirm actions before modifying playlists
2. Handle cases where songs/artists have similar names
3. Provide alternatives if exact match not found
4. Respect the user's music preferences when making recommendations
