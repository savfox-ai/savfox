---
name: python
description: Run Python scripts and manage virtual environments.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🐍"
    requires:
      bins:
        - python3
      env: []
    install:
      - id: brew
        kind: brew
        formula: python@3
        bins: [python3]
        label: Homebrew
---

# Python Skill

Run Python scripts, manage packages, and virtual environments.

## Run Script

```bash
python3 script.py
python3 -c "print('Hello, World!')"
```

## Virtual Environments

Create:
```bash
python3 -m venv .venv
```

Activate:
```bash
source .venv/bin/activate  # Linux/macOS
.venv\Scripts\activate     # Windows
```

Install packages:
```bash
pip install requests pandas numpy
pip install -r requirements.txt
```

## Package Management

List installed:
```bash
pip list
```

Freeze requirements:
```bash
pip freeze > requirements.txt
```

## One-liner Utilities

JSON formatting:
```bash
echo '{"key":"value"}' | python3 -m json.tool
```

HTTP server:
```bash
python3 -m http.server 8000
```

Base64:
```bash
python3 -c "import base64; print(base64.b64encode(b'hello').decode())"
```

## Guidelines

- Always use `python3` (not `python`) for portability
- Use virtual environments to isolate dependencies
- Use `pip install --user` if not in a virtual environment
- Use `python3 -m pip` instead of bare `pip` to ensure correct version
