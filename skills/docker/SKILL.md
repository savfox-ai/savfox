---
name: docker
description: Manage Docker containers, images, and compose services.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🐳"
    requires:
      bins:
        - docker
      env: []
    install:
      - id: brew
        kind: brew
        formula: docker
        bins: [docker]
        label: Homebrew
      - id: apt
        kind: apt
        package: docker.io
        bins: [docker]
        label: APT
      - id: winget
        kind: winget
        package: Docker.DockerDesktop
        bins: [docker]
        label: Winget
---

# Docker Skill

Manage Docker containers, images, and compose services.

## Container Management

List running containers:
```bash
docker ps
```

List all containers (including stopped):
```bash
docker ps -a
```

Start/stop/restart a container:
```bash
docker start <container>
docker stop <container>
docker restart <container>
```

View container logs:
```bash
docker logs --tail 100 -f <container>
```

Execute command in running container:
```bash
docker exec -it <container> bash
```

## Image Management

List images:
```bash
docker images
```

Pull an image:
```bash
docker pull <image>:<tag>
```

Build from Dockerfile:
```bash
docker build -t <name>:<tag> .
```

Remove unused images:
```bash
docker image prune -a
```

## Docker Compose

Start services:
```bash
docker compose up -d
```

Stop services:
```bash
docker compose down
```

View logs:
```bash
docker compose logs -f <service>
```

Rebuild and restart:
```bash
docker compose up -d --build
```

## Inspect and Debug

Inspect container details:
```bash
docker inspect <container> | jq '.[0].NetworkSettings.IPAddress'
```

Show resource usage:
```bash
docker stats --no-stream
```

## Guidelines

- Always use specific image tags in production (not `latest`)
- Use `docker compose` (v2) instead of `docker-compose` (v1)
- Clean up unused resources with `docker system prune`
- Use `--rm` for one-off containers to auto-remove on exit
