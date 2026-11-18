#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$REPO/chores.yml" <<'PY'
import sys
import yaml

with open(sys.argv[1], encoding="utf-8") as source:
    tasks = yaml.safe_load(source)["tasks"]
dependencies = tasks["test:cli:host"].get("deps", [])
if "build:cli" not in dependencies:
    raise SystemExit("FAIL test:cli:host must build the rust-ntfs harness through build:cli")
print("cli-host-harness-build: the host tier builds both CLIs")
PY
