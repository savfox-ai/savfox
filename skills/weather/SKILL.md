---
name: weather
description: "Look up current weather and forecasts for any location using wttr.in"
version: "1.0.0"
metadata:
  savfox:
    emoji: "\u26C5"
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
# Weather Skill

You can look up current weather conditions and short-term forecasts for any location using the wttr.in service via `curl`.

## Quick Current Weather

To get a concise one-line summary for a location:

```bash
curl -s "wttr.in/{location}?format=3"
```

Example output: `London: +12°C`

## Detailed Forecast

For a full 3-day forecast with ASCII art:

```bash
curl -s "wttr.in/{location}"
```

## Specific Format Strings

wttr.in supports custom format strings. Useful placeholders:

| Placeholder | Meaning             |
|-------------|---------------------|
| `%c`        | Weather condition icon |
| `%C`        | Weather condition text |
| `%t`        | Temperature (actual) |
| `%f`        | "Feels like" temperature |
| `%h`        | Humidity             |
| `%w`        | Wind speed           |
| `%p`        | Precipitation (mm)   |
| `%l`        | Location             |

Example: detailed one-liner:

```bash
curl -s "wttr.in/{location}?format=%l:+%c+%C+%t+(feels+like+%f)+humidity+%h+wind+%w"
```

## JSON Output

For programmatic processing, request JSON:

```bash
curl -s "wttr.in/{location}?format=j1"
```

This returns a JSON object with `current_condition`, `nearest_area`, and `weather` (3-day forecast) arrays.

## Location Formats

- **City name:** `curl -s "wttr.in/Paris"`
- **Airport code:** `curl -s "wttr.in/JFK"`
- **Coordinates:** `curl -s "wttr.in/40.7,-74.0"`
- **Zip code:** `curl -s "wttr.in/10001"`
- **IP-based (auto):** `curl -s "wttr.in/"`

## Moon Phase

```bash
curl -s "wttr.in/Moon"
```

## Guidelines

1. Always URL-encode spaces in location names: use `+` or `%20` (e.g. `New+York`).
2. Use `format=3` for a quick answer when the user just wants the current temperature.
3. Use `format=j1` when you need to extract specific numeric values for further processing.
4. If the user asks about multiple locations, make separate requests for each.
5. The service is rate-limited; avoid making more than a few requests per minute.
6. For indoor comfort recommendations, interpret the "feels like" temperature and humidity together.
