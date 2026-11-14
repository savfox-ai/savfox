#!/usr/bin/env python3
"""Prepare workspace versions without changing unrelated manifest formatting."""

import argparse
import json
import re
import subprocess
from pathlib import Path
import tomllib


def prepare(root: Path, bump: str) -> str:
    manifest = root / "Cargo.toml"
    original = manifest.read_bytes().decode("utf-8")
    data = tomllib.loads(original)
    current = data["workspace"]["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", current):
        raise ValueError("release preparation requires a stable workspace version")
    parts = list(map(int, current.split(".")))
    index = {"major": 0, "minor": 1, "patch": 2}[bump]
    parts[index] += 1
    parts[index + 1:] = [0] * (2 - index)
    version = ".".join(map(str, parts))
    # Select keys using parsed TOML, then edit only their version literals.
    lines = original.splitlines(keepends=True)
    section = ""
    for i, line in enumerate(lines):
        match = re.match(r"\s*\[([^]]+)\]\s*(?:#.*)?$", line.strip())
        if match:
            section = match[1]
        if section == "workspace.package" and re.match(r"^version\s*=", line):
            lines[i] = re.sub(r'("[^"]+")', json.dumps(version), line, count=1)
        elif section == "workspace.dependencies":
            key = re.match(r"^([\w-]+)\s*=", line)
            if key:
                dep = data["workspace"]["dependencies"].get(key[1])
                if isinstance(dep, dict) and "path" in dep and "version" in dep:
                    lines[i] = re.sub(r'version\s*=\s*"[^"]+"', f'version = "{version}"', line, count=1)
    updated = "".join(lines)
    parsed = tomllib.loads(updated)
    if parsed["workspace"]["package"]["version"] != version:
        raise ValueError("workspace version was not updated")
    for name, dep in parsed["workspace"]["dependencies"].items():
        if isinstance(dep, dict) and "path" in dep and "version" in dep and dep["version"] != version:
            raise ValueError(f"internal dependency version was not updated: {name}")
    lock = root / "Cargo.lock"
    previous_lock = lock.read_bytes() if lock.exists() else None
    try:
        manifest.write_text(updated, encoding="utf-8", newline="")
        subprocess.run(["cargo", "update", "--workspace"], cwd=root, check=True)
        locked = tomllib.loads(lock.read_text(encoding="utf-8"))
        metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=root))
        for package in metadata["packages"]:
            if package["version"] != version or not any(p["name"] == package["name"] and p["version"] == version and "source" not in p for p in locked["package"]):
                raise ValueError(f"workspace/lock version mismatch: {package['name']}")
    except BaseException:
        manifest.write_text(original, encoding="utf-8", newline="")
        if previous_lock is not None:
            lock.write_bytes(previous_lock)
        elif lock.exists():
            lock.unlink()
        raise
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bump", choices=["major", "minor", "patch"])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    print(prepare(args.root.resolve(), args.bump))
