#!/usr/bin/env python3
"""slate-read-compare.py -- Slate's read of each image beside Windows' (#1103).

    slate-read-compare.py CASES WINDOWS_DIR SLATE_DIR CAPABILITIES OUT

CASES is the Windows job's cases.tsv (`case origin image sha256 ...`).
WINDOWS_DIR holds Windows' `<case>.json` read manifests (schema 1, see
docs/slate-windows-manifests.md) and SLATE_DIR the manifests of the same
shape made from Slate's read of the same images. Windows is the reference:
Slate's manifest is compared with it, never the other way round.

Both manifests of a case must name that case and the SHA-256 its cases.tsv
row gives, or the case is refused before any entry is compared: a manifest
of different bytes is not evidence about these. Then every path, its kind,
and each stream's size and SHA-256 must agree. Order is not compared.

CAPABILITIES is a tab-separated `case path stream reason` list of entries
Slate is known not to read (stream `*` for every stream of the path, path
`*` for the whole case). A listed entry is excused and written to
OUT/excluded.tsv by name; nothing else is.

OUT/diff.tsv gets one `case path stream problem windows slate` row per
disagreement, OUT/cases.tsv one `case windows_entries slate_entries result`
row per case. Exit 0 when every case agrees, 1 when any case disagrees or
cannot be compared, 2 for a usage error. Every case is compared before the
verdict, so one run names all of its problems.
"""

import json
import os
import sys


def load_manifest(directory, case, digest, side, problems):
    path = os.path.join(directory, case + ".json")
    try:
        with open(path, encoding="utf-8") as handle:
            manifest = json.load(handle)
    except (OSError, ValueError) as error:
        problems.append(f"{case}: no readable {side} manifest at {path}: {error}")
        return None
    if manifest.get("case") != case:
        problems.append(
            f"{case}: the {side} manifest names case {manifest.get('case')!r}"
        )
        return None
    if manifest.get("image_sha256") != digest:
        problems.append(
            f"{case}: the {side} manifest is bound to image {manifest.get('image_sha256')}, "
            f"not {digest} from cases.tsv"
        )
        return None
    entries = {}
    for entry in manifest.get("entries", []):
        streams = {
            s["name"]: (s["size"], s["sha256"]) for s in entry.get("streams", [])
        }
        entries[entry["path"]] = (entry["kind"], streams)
    return entries


def load_capabilities(path):
    excused = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\r\n")
            if not line or line.startswith("#"):
                continue
            fields = line.split("\t")
            if len(fields) != 4:
                sys.exit(
                    f"slate-read-compare: capability line is not `case path stream reason`: {line!r}"
                )
            excused.append(tuple(fields))
    return excused


def excuse(excused, case, path, stream):
    for row in excused:
        if row[0] == case and row[1] in ("*", path) and row[2] in ("*", stream):
            return row
    return None


def compare(windows, slate):
    """Every (path, stream, problem, windows, slate) disagreement."""
    found = []
    for path in sorted(set(windows) | set(slate)):
        if path not in slate:
            found.append((path, "", "missing-path", windows[path][0], ""))
            continue
        if path not in windows:
            found.append((path, "", "extra-path", "", slate[path][0]))
            continue
        (wkind, wstreams), (skind, sstreams) = windows[path], slate[path]
        if wkind != skind:
            found.append((path, "", "kind", wkind, skind))
        for name in sorted(set(wstreams) | set(sstreams)):
            if name not in sstreams:
                found.append((path, name, "missing-stream", str(wstreams[name][0]), ""))
            elif name not in wstreams:
                found.append((path, name, "extra-stream", "", str(sstreams[name][0])))
            else:
                if wstreams[name][0] != sstreams[name][0]:
                    found.append(
                        (
                            path,
                            name,
                            "size",
                            str(wstreams[name][0]),
                            str(sstreams[name][0]),
                        )
                    )
                if wstreams[name][1] != sstreams[name][1]:
                    found.append(
                        (path, name, "sha256", wstreams[name][1], sstreams[name][1])
                    )
    return found


def main(argv):
    if len(argv) != 6:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    cases_path, windows_dir, slate_dir, capabilities, out = argv[1:]
    excused = load_capabilities(capabilities)
    os.makedirs(out, exist_ok=True)

    problems, diff_rows, case_rows, excluded_rows = [], [], [], []
    with open(cases_path, encoding="utf-8") as handle:
        rows = [line.rstrip("\r\n").split("\t") for line in handle]
    for row in rows:
        if not row or not row[0] or row[0] == "case":
            continue
        case, digest = row[0], row[3]
        windows = load_manifest(windows_dir, case, digest, "Windows", problems)
        slate = load_manifest(slate_dir, case, digest, "Slate", problems)
        if windows is None or slate is None:
            case_rows.append((case, "", "", "refused"))
            continue
        disagreements = 0
        for path, stream, problem, wvalue, svalue in compare(windows, slate):
            reason = excuse(excused, case, path, stream)
            if reason:
                excluded_rows.append((case, path, stream, reason[3], problem))
                continue
            disagreements += 1
            diff_rows.append((case, path, stream, problem, wvalue, svalue))
        case_rows.append(
            (
                case,
                str(len(windows)),
                str(len(slate)),
                "disagree" if disagreements else "agree",
            )
        )

    def write(name, header, body):
        with open(os.path.join(out, name), "w", encoding="utf-8") as handle:
            handle.writelines("\t".join(fields) + "\n" for fields in [header] + body)

    write(
        "diff.tsv", ("case", "path", "stream", "problem", "windows", "slate"), diff_rows
    )
    write(
        "cases.tsv", ("case", "windows_entries", "slate_entries", "result"), case_rows
    )
    write(
        "excluded.tsv", ("case", "path", "stream", "reason", "problem"), excluded_rows
    )

    for problem in problems:
        print(f"slate-read-compare: {problem}", file=sys.stderr)
    for case, path, stream, problem, wvalue, svalue in diff_rows:
        print(
            f"slate-read-compare: {case} {path} [{stream}] {problem}: Windows {wvalue!r}, Slate {svalue!r}",
            file=sys.stderr,
        )
    for case, path, stream, reason, problem in excluded_rows:
        print(
            f"slate-read-compare: excused {case} {path} [{stream}] {problem}: {reason}"
        )
    if problems or diff_rows:
        print(
            f"slate-read-compare: {len(problems)} cases refused, {len(diff_rows)} disagreements",
            file=sys.stderr,
        )
        return 1
    print(
        f"slate-read-compare: {len(case_rows)} cases agree with Windows, {len(excluded_rows)} entries excused"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
