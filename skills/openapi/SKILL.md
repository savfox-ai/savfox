---
name: openapi
description: Work with OpenAPI/Swagger specifications for REST APIs.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📋"
    requires:
      bins: []
      env: []
    install: []
---

# OpenAPI Skill

Work with OpenAPI/Swagger REST API specifications.

## Validate Spec

```bash
npx @apidevtools/swagger-cli validate openapi.yaml
```

## Generate Client

TypeScript:
```bash
npx openapi-typescript openapi.yaml -o types.ts
```

## Preview Docs

Start Swagger UI locally:
```bash
npx @redocly/cli preview-docs openapi.yaml
```

## Bundle Multiple Files

```bash
npx @redocly/cli bundle openapi.yaml -o bundled.yaml
```

## Lint

```bash
npx @redocly/cli lint openapi.yaml
```

## Convert Formats

YAML to JSON:
```bash
python3 -c "import yaml,json,sys; print(json.dumps(yaml.safe_load(open('openapi.yaml')),indent=2))"
```

## Mock Server

```bash
npx @stoplight/prism-cli mock openapi.yaml
```

## Guidelines

- Use OpenAPI 3.1 for the latest features
- Keep specs DRY with `$ref` for shared schemas
- Use `npx` to run tools without global install
- Validate specs in CI before merging
