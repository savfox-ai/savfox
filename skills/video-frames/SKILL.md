---
name: video-frames
description: Extract and analyze frames from video files using ffmpeg.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🎬"
    requires:
      bins:
        - ffmpeg
      env: []
    install:
      - id: brew
        kind: brew
        formula: ffmpeg
        bins: [ffmpeg]
        label: Homebrew
      - id: apt
        kind: apt
        package: ffmpeg
        bins: [ffmpeg]
        label: APT
      - id: choco
        kind: choco
        package: ffmpeg
        bins: [ffmpeg]
        label: Chocolatey
      - id: winget
        kind: winget
        package: Gyan.FFmpeg
        bins: [ffmpeg]
        label: Winget
---

# Video Frames Skill

Extract frames from video files for visual analysis.

## Extract Single Frame

Extract a frame at a specific timestamp:
```bash
ffmpeg -ss 00:00:05 -i input.mp4 -vframes 1 -q:v 2 frame.jpg
```

## Extract Multiple Frames

Extract one frame per second:
```bash
ffmpeg -i input.mp4 -vf "fps=1" frames_%04d.jpg
```

Extract one frame every 10 seconds:
```bash
ffmpeg -i input.mp4 -vf "fps=1/10" frames_%04d.jpg
```

## Extract Key Frames Only

Extract only keyframes (I-frames):
```bash
ffmpeg -i input.mp4 -vf "select=eq(pict_type\,I)" -vsync vfr keyframe_%04d.jpg
```

## Extract with Resize

Extract frames resized to max 1024px width:
```bash
ffmpeg -i input.mp4 -vf "fps=1,scale=1024:-1" frames_%04d.jpg
```

## Get Video Info

```bash
ffprobe -v quiet -print_format json -show_format -show_streams input.mp4
```

## Create Thumbnail Grid

Create a contact sheet (4x4 grid):
```bash
ffmpeg -i input.mp4 -vf "select=not(mod(n\,30)),scale=320:-1,tile=4x4" -frames:v 1 grid.jpg
```

## Guidelines

- Use `-q:v 2` for high quality JPEG output
- For analysis, resize frames to reasonable size (1024px max) to save disk space
- Key frame extraction is fastest as it doesn't need to decode every frame
- Use `-ss` before `-i` for fast seeking (may be less accurate)
- Use `-ss` after `-i` for accurate seeking (slower)
- Supported formats: mp4, avi, mkv, mov, webm, and most video formats
- Output formats: jpg, png, bmp, tiff
