#!/usr/bin/env bash
# Runs tests/scripts/slate-control-size.ps1: the Slate tier's Windows
# controls reach exactly 256 MiB whether or not their scratch partition
# carries slack (#1284). The PowerShell stands in for the disk cmdlets, so
# it runs on any host with pwsh; without pwsh this fails, it does not skip.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if ! command -v pwsh >/dev/null 2>&1; then
    echo "slate-control-size: FAIL pwsh is required to run slate-control-size.ps1: install PowerShell on the runner" >&2
    exit 1
fi
pwsh -NoProfile -NonInteractive -File "$here/slate-control-size.ps1"
