---
name: playwright
description: Browser automation and testing with Playwright.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🎭"
    requires:
      bins:
        - npx
      env: []
    install:
      - id: npm
        kind: npm
        package: playwright
        bins: [npx]
        label: npm
---

# Playwright Skill

Browser automation and end-to-end testing.

## Quick Start

Install browsers:
```bash
npx playwright install
```

## Take Screenshot

```bash
npx playwright screenshot https://example.com screenshot.png
```

Full page:
```bash
npx playwright screenshot --full-page https://example.com full.png
```

## Generate Code

Record user interactions:
```bash
npx playwright codegen https://example.com
```

## Run Tests

```bash
npx playwright test
npx playwright test --headed   # Show browser
npx playwright test --debug    # Step through
```

## PDF Export

```bash
npx playwright pdf https://example.com page.pdf
```

## Script Example

Create `test.mjs`:
```javascript
import { chromium } from 'playwright';
const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto('https://example.com');
console.log(await page.title());
await browser.close();
```

Run:
```bash
node test.mjs
```

## Guidelines

- Use `codegen` to generate test scripts from manual interaction
- Use `--headed` for debugging, headless for CI
- Playwright auto-waits for elements — no need for manual waits
- Supports Chromium, Firefox, and WebKit
