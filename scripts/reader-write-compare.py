#!/usr/bin/env python3
"""reader-write-compare.py -- our reader's read of each Slate-written copy beside Windows' (#1115).

    reader-write-compare.py GRADED WINDOWS_DIR READER_DIR OUT

GRADED is the Windows job's slate-writes-graded.tsv (#864): `case image
sha256 windows_verdict windows_exit`, one row per Slate-written copy.
WINDOWS_DIR holds Windows' `<case>.json` read manifests of the copies it
accepted (schema 1, see docs/slate-windows-manifests.md) and READER_DIR the
manifests of the same shape made from this crate's read-only reader of the
same copies. Windows is the reference.

Only an `accepted` row is compared. A `slate-defect` or `not-written` row is
a Slate writer outcome, never evidence about our reader: it is listed in
OUT/cases.tsv as not compared. Both manifests of an accepted case must name
that case and the SHA-256 its row gives, or the case is refused before any
entry is compared. Then every path, its kind, and each stream's size and
SHA-256 must agree; order is not compared. Nothing is excused: Windows read
these bytes from a copy it accepted, so every disagreement is ours.

Manifest loading and the entry comparison are slate-read-compare.py's
(#1103), so both comparisons read one schema the same way.

OUT/diff.tsv gets one `case path stream problem windows reader` row per
disagreement, OUT/cases.tsv one `case windows_entries reader_entries result`
row per graded row. Exit 0 when every accepted case agrees, 1 when any
disagrees or cannot be compared or no case was accepted, 2 for a usage error
or a table that is not the grading table. Every case is compared before the
verdict, so one run names all of its problems.
"""

import importlib.util
import os
import sys

HEADER = ["case", "image", "sha256", "windows_verdict", "windows_exit"]


def slate_read_compare():
    here = os.path.dirname(os.path.abspath(__file__))
    spec = importlib.util.spec_from_file_location(
        "slate_read_compare", os.path.join(here, "slate-read-compare.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main(argv):
    if len(argv) != 5:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    graded_path, windows_dir, reader_dir, out = argv[1:]
    with open(graded_path, encoding="utf-8") as handle:
        rows = [line.rstrip("\r\n").split("\t") for line in handle if line.strip()]
    if not rows or rows[0] != HEADER:
        print(
            f"reader-write-compare: {graded_path} is not slate-writes-graded.tsv: "
            f"header {rows[0] if rows else []!r}, want {HEADER!r}",
            file=sys.stderr,
        )
        return 2
    shared = slate_read_compare()
    os.makedirs(out, exist_ok=True)

    problems, diff_rows, case_rows = [], [], []
    compared = 0
    for row in rows[1:]:
        case, digest, verdict = row[0], row[2], row[3]
        if verdict != "accepted":
            case_rows.append((case, "", "", f"not-compared {verdict}"))
            continue
        compared += 1
        windows = shared.load_manifest(windows_dir, case, digest, "Windows", problems)
        reader = shared.load_manifest(reader_dir, case, digest, "our reader", problems)
        if windows is None or reader is None:
            case_rows.append((case, "", "", "refused"))
            continue
        found = shared.compare(windows, reader)
        diff_rows.extend((case,) + disagreement for disagreement in found)
        case_rows.append(
            (
                case,
                str(len(windows)),
                str(len(reader)),
                "disagree" if found else "agree",
            )
        )
    if not compared:
        problems.append(
            f"no copy in {graded_path} was accepted by Windows, so nothing was compared"
        )

    def write(name, header, body):
        with open(os.path.join(out, name), "w", encoding="utf-8") as handle:
            handle.writelines("\t".join(fields) + "\n" for fields in [header] + body)

    write(
        "diff.tsv",
        ("case", "path", "stream", "problem", "windows", "reader"),
        diff_rows,
    )
    write(
        "cases.tsv", ("case", "windows_entries", "reader_entries", "result"), case_rows
    )

    for problem in problems:
        print(f"reader-write-compare: {problem}", file=sys.stderr)
    for case, path, stream, problem, wvalue, rvalue in diff_rows:
        print(
            f"reader-write-compare: {case} {path} [{stream}] {problem}: Windows {wvalue!r}, ours {rvalue!r}",
            file=sys.stderr,
        )
    if problems or diff_rows:
        print(
            f"reader-write-compare: {len(problems)} problems, {len(diff_rows)} disagreements",
            file=sys.stderr,
        )
        return 1
    print(
        f"reader-write-compare: {compared} accepted Slate-written copies agree with Windows"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
