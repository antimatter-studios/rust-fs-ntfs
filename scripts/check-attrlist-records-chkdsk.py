#!/usr/bin/env python3
"""Require all four #826 read-only Windows grades, retaining disagreements."""

import json
import re
import sys
from pathlib import Path

CASES = ("clean", "base-reference", "sequence", "in-use")


def grade(root: Path) -> bool:
    comparisons = []
    for case in CASES:
        expected = "clean" if case == "clean" else "damaged"
        result = {"case": case, "expected": expected, "actual": "ungraded"}
        try:
            folder = root / case
            verdict = json.loads(
                (folder / "verdict.json").read_text(encoding="utf-8-sig")
            )
            report = (folder / "chkdsk-readonly.txt").read_text(encoding="utf-8-sig")
            code = int((folder / "chkdsk-readonly-exit.txt").read_text().strip())
            if set(verdict["modes"]) != {"readonly"} or set(verdict["exits"]) != {
                "readonly"
            }:
                raise ValueError("expected exactly one read-only pass, with no repair")
            mode = verdict["modes"]["readonly"]
            if mode["exit"] != code or verdict["exits"]["readonly"] != code:
                raise ValueError("verdict and chkdsk exit marker disagree")
            if verdict["verdict_shape"] != "clean" or verdict["passed"] != (code == 0):
                raise ValueError(
                    "the harness must record the unmodified read-only Clean verdict"
                )
            result["chkdsk_exit"] = code
            # These are outcome lines, not generic nonzero process failures.
            # The damage wording is also retained in tests/scripts/chkdsk-verdict.ps1.
            clean = re.search(r"found no problems", report, re.IGNORECASE)
            damaged = re.search(
                r"Errors found\.|Windows has (?:checked|scanned) the file system and found problems",
                report,
                re.IGNORECASE,
            )
            if code == 0 and mode["state"] == "scanned" and clean and not damaged:
                result["actual"] = "clean"
            elif (
                code in (1, 2, 3)
                and mode["state"] == "failed"
                and damaged
                and not clean
            ):
                result["actual"] = "damaged"
            else:
                raise ValueError(
                    "chkdsk did not report an unambiguous filesystem verdict; inspect its report"
                )
            findings = (root / f"{case}.findings.txt").read_text()
            if not findings.strip() or ("BadAttributeList" in findings) != (
                case != "clean"
            ):
                raise ValueError(
                    "check_io findings are missing or do not match the case"
                )
        except (OSError, ValueError, KeyError, TypeError) as error:
            result["error"] = str(error)
        result["agrees"] = result["actual"] == expected and "error" not in result
        comparisons.append(result)
        detail = result.get(
            "error", f"expected {expected}, Windows reported {result['actual']}"
        )
        print(f"{case}: {detail}")
    root.mkdir(parents=True, exist_ok=True)
    (root / "comparisons.json").write_text(json.dumps(comparisons, indent=2) + "\n")
    passed = all(item["agrees"] for item in comparisons)
    print(
        f"attribute-list records: {sum(item['agrees'] for item in comparisons)} of 4 Windows grades agree"
    )
    return passed


if __name__ == "__main__":
    sys.exit(
        0
        if grade(Path(sys.argv[1] if len(sys.argv) > 1 else "tmp/list-record-check"))
        else 1
    )
