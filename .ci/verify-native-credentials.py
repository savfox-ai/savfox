#!/usr/bin/env python3
"""Verify OS storage, and cross-process persistence."""
import json
import os
from pathlib import Path
import subprocess
import uuid

subprocess.run(["cargo", "build", "--locked", "-p", "savfox-keyring-store", "--examples"], check=True)
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
suffix = ".exe" if os.name == "nt" else ""
examples = Path(metadata["target_directory"]) / "debug/examples"
native = examples / ("native-credential-smoke" + suffix)
# All modes share one executable identity so macOS Keychain ACLs remain valid.
# Each invocation still runs in a fresh process against the actual OS vault.
environment = os.environ.copy()
environment["SAVFOX_KEYRING_SMOKE_SERVICE"] = "savfox-ci-" + uuid.uuid4().hex

def execute(binary, *args, **variables):
    print("credential check:", binary.name, *args, flush=True)
    subprocess.run([str(binary), *args], env={**environment, **variables}, check=True)

try:
    execute(native, "seed")
    execute(native, SAVFOX_KEYRING_SMOKE_SEEDED="1", SAVFOX_KEYRING_SMOKE_RETAIN="1")
    execute(native, "updated")
    execute(native, "cleanup")
    execute(native, "absent")
    execute(native)
    execute(native, "absent")
finally:
    execute(native, "cleanup")
print("OS credential lifecycle and cross-process persistence passed", flush=True)
