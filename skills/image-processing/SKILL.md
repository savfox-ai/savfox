---
name: image-processing
description: Process and manipulate images using ImageMagick.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🖼️"
    requires:
      bins:
        - magick
      env: []
    install:
      - id: brew
        kind: brew
        formula: imagemagick
        bins: [magick]
        label: Homebrew
      - id: apt
        kind: apt
        package: imagemagick
        bins: [magick]
        label: APT
      - id: choco
        kind: choco
        package: imagemagick
        bins: [magick]
        label: Chocolatey
---

# Image Processing Skill

Process and manipulate images with ImageMagick.

## Resize

```bash
magick input.png -resize 800x600 output.png
magick input.png -resize 50% output.png
```

Fit within bounds (maintain aspect ratio):
```bash
magick input.png -resize 800x600\> output.png
```

## Convert Format

```bash
magick input.png output.jpg
magick input.svg output.png
```

## Crop

```bash
magick input.png -crop 400x300+100+50 output.png
```

## Rotate

```bash
magick input.png -rotate 90 output.png
```

## Compress

JPEG quality:
```bash
magick input.jpg -quality 80 output.jpg
```

PNG optimization:
```bash
magick input.png -strip -quality 85 output.png
```

## Create Thumbnail

```bash
magick input.png -thumbnail 200x200^ -gravity center -extent 200x200 thumb.png
```

## Montage (Grid)

```bash
magick montage *.png -geometry 200x200+5+5 -tile 3x grid.png
```

## Get Info

```bash
magick identify input.png
magick identify -verbose input.png
```

## Batch Processing

```bash
for f in *.png; do magick "$f" -resize 800x "${f%.png}_small.png"; done
```

## Guidelines

- Use `magick` (v7+) instead of `convert` (v6)
- Use `-strip` to remove metadata for smaller files
- Use `\>` suffix on resize to only shrink (never enlarge)
- Use `-quality` to balance size vs quality for JPEGs
