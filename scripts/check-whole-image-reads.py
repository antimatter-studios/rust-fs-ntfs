#!/usr/bin/env python3
"""Refuse whole-file reads in Rust tests unless a small-file exception names them."""

import argparse
import pathlib
import re
import sys


def masked(source):
    pattern = re.compile(
        r'//[^\n]*|/\*|r(\#*)".*?"\1|'
        r"'(?:\\(?:u\{[0-9A-Fa-f_]+\}|x[0-9A-Fa-f]{2}|[^\n])|[^'\\\n])'|"
        r'"(?:\\.|[^"\\])*"',
        flags=re.S,
    )
    pieces = []
    position = 0
    while match := pattern.search(source, position):
        pieces.append(source[position:match.start()])
        end = match.end()
        if match[0] == "/*":
            depth = 1
            while depth and end < len(source):
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
        pieces.append(re.sub(r"[^\n]", " ", source[match.start():end]))
        position = end
    pieces.append(source[position:])
    return "".join(pieces)


def normalized(expression):
    return re.sub(r"\s+", "", expression)


def calls(source):
    code = masked(source)
    modules = {"fs"}
    names = set()
    for imports in re.findall(r"use\s+(?:::)?std\s*::([^;]+);", code):
        if not re.search(r"\bfs\b", imports):
            continue
        modules.update(re.findall(r"\bfs\s+as\s+(\w+)", imports))
        for match in re.finditer(r"\bread\b(?:\s+as\s+(\w+))?", imports):
            names.add(match[1] or "read")
    qualified = r"(?:std\s*::\s*)?(?:" + "|".join(sorted(modules)) + r")\s*::\s*read"
    alternatives = [qualified, *map(re.escape, sorted(names))]
    pattern = r"(?<![\w:.])(?:::)?(?:" + "|".join(alternatives) + r")(?:\s*::\s*<[^;(){}]*>)?\s*\("
    for match in re.finditer(pattern, code):
        position = match.end()
        depth = 1
        while position < len(code) and depth:
            depth += (code[position] == "(") - (code[position] == ")")
            position += 1
        yield source.count("\n", 0, match.start()) + 1, normalized(source[match.start():position])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    allowlist = args.root / "tests/scripts/whole-read-allowlist.txt"
    allowed = {}
    for number, line in enumerate(allowlist.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        entry, separator, reason = line.partition(" # ")
        parts = entry.split(":", 2)
        if not separator or not reason.strip() or len(parts) != 3 or not parts[1].isdigit():
            print(f"{allowlist}:{number}: expected path:line:call # small-file reason", file=sys.stderr)
            return 1
        path, line, expression = parts
        allowed[(path, int(line), normalized(expression))] = reason
    violations = 0
    seen = set()
    paths = list((args.root / "tests").rglob("*.rs"))
    paths.extend(path for path in [
        args.root / "src/mft_inventory.rs",
        args.root / "src/cli/ntfs/inventory.rs",
        args.root / "src/rescue.rs",
        args.root / "src/cli/ntfs/device.rs",
        args.root / "src/index_slack.rs",
    ] if path.exists())
    for path in sorted(paths):
        relative = path.relative_to(args.root).as_posix()
        for line, expression in calls(path.read_text(encoding="utf-8")):
            key = (relative, line, expression)
            seen.add(key)
            if key not in allowed:
                print(f"{relative}:{line}: whole-file read {expression}; use common's streaming image helpers", file=sys.stderr)
                violations += 1
    for path, line, expression in sorted(allowed.keys() - seen):
        print(f"{allowlist}: stale exception {path}:{line}:{expression}", file=sys.stderr)
        violations += 1
    if violations:
        return 1
    print("whole-image reads: every whole-file read has a small-file exception")
    return 0


if __name__ == "__main__":
    sys.exit(main())
