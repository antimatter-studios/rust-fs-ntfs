#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$ROOT" <<'PY'
import pathlib
import re
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest = tomllib.loads((root / "Cargo.toml").read_text())
source = (root / "tests/windows_interrupted_fixture.rs").read_text()
workflow = (root / ".github/workflows/ci.yml").read_text()
chores = (root / "chores.yml").read_text()
test_name = "a_dry_run_plans_the_writes_that_reach_what_windows_recovered_and_makes_none"
checks = [
    (
        manifest["profile"].get("test-opt") == {"inherits": "dev", "lto": False}
        and manifest["profile"]["dev"].get("opt-level") == 1,
        "test-opt inherits dev's opt-level 1 and default codegen units without LTO",
    ),
    (
        manifest["profile"]["release"]
        == {"lto": True, "codegen-units": 1, "opt-level": 3, "panic": "unwind"},
        "the shipping release profile retains its optimization settings",
    ),
    (
        any(binary["name"] == "rust-fs-ntfs" and "cli" in binary.get("required-features", [])
            for binary in manifest["bin"]),
        "the CLI binary declares its cli prerequisite",
    ),
    (
        re.search(r'#\[cfg\(feature = "cli"\)\]\s*#\[test\]\s*fn ' + test_name + r'\(', source)
        is not None,
        "the interrupted-fixture dry-run test requires cli to build rust-fs-ntfs",
    ),
    (
        '#![cfg(feature = "cli")]' not in source
        and not any(target["name"] == "windows_interrupted_fixture"
                    and target.get("required-features") for target in manifest.get("test", [])),
        "the non-CLI interrupted-fixture tests remain available with plan-file alone",
    ),
]
for document, label in [(workflow, "CI"), (chores, "chore test:suite")]:
    checks.append((
        "cargo test --profile test-opt --locked --features cli,harness --no-fail-fast" in document,
        f"{label} runs every interrupted-fixture test with cli",
    ))
failures = [description for passed, description in checks if not passed]
for description in failures:
    print(f"FAIL {description}", file=sys.stderr)
print(f"interrupted-fixture-features: {len(checks) - len(failures)} passed, {len(failures)} failed")
sys.exit(bool(failures))
PY
status=$?
if [ "$status" -eq 0 ]; then
    echo "interrupted-fixture-features: all checks passed"
fi
exit "$status"
