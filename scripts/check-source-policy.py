#!/usr/bin/env python3
"""Check repository references and source-policy claims without fetching sources."""

import argparse
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit


def implementation_reference(url):
    parsed = urlsplit(unquote(url))
    host, path = parsed.netloc.lower(), parsed.path.lower()
    if host in {"github.com", "raw.githubusercontent.com", "gitlab.com"}:
        source_link = host == "raw.githubusercontent.com" or any(
            part in path for part in ("/blob/", "/tree/", "/raw/")
        )
        if source_link and "/ntfs-3g/" in path:
            return True
        if source_link and re.search(r"/fs/ntfs(?:3)?(?:/|$)", path):
            return True
    if host in {"git.kernel.org", "elixir.bootlin.com"}:
        return bool(re.search(r"/fs/ntfs(?:3)?(?:/|$)", path))
    return False


def violations(path, text):
    for match in re.finditer(r"https?://[^\s<>\]\)]+", text):
        if implementation_reference(match.group()):
            line = text.count("\n", 0, match.start()) + 1
            yield f"{path}:{line}: implementation-source reference; use verified documentation or Windows observations"

    # Join wrapped prose and Rust documentation lines before judging a claim.
    prose = " ".join(re.sub(r"^\s*//[!/ ]*", "", text, flags=re.MULTILINE).split())
    header_citations = (
        r"ntfs-3g.{0,80}layout\.h",
        r"(?:Linux ntfs3|fs/ntfs3).{0,80}ntfs\.h",
    )
    if any(re.search(pattern, prose, re.IGNORECASE) for pattern in header_citations):
        yield f"{path}: implementation-header citation; use verified documentation or Windows observations"
    claims = (
        r"\bno GPL\b.{0,120}\bconsulted\b",
        r"\bconsulted no GPL\b",
        r"\bsources NOT consulted\b",
        r"\bno mention of\b.{0,100}\bntfs-3g\b",
        r"\bno GPL tool(?:ing)? (?:name-drops|references)\b",
        r"\bGPL-tainted citations\b",
    )
    for pattern in claims:
        if re.search(pattern, prose, re.IGNORECASE):
            yield f"{path}: replace historical source guarantees or tool-name bans with the current source policy"
            break


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parent.parent
    )
    args = parser.parse_args()
    root = args.root.resolve()
    required = [root / name for name in ("README.md", "AGENTS.md")]
    missing = [str(path.relative_to(root)) for path in required if not path.is_file()]
    if missing:
        print(
            "source-policy: missing required files: " + ", ".join(missing),
            file=sys.stderr,
        )
        return 1
    files = set(required)
    for directory in ("src", "include", "scripts", "docs"):
        files.update(
            path
            for path in (root / directory).rglob("*")
            if path.is_file() and path.suffix in {".rs", ".h", ".sh", ".ps1", ".md"}
        )
    errors = [
        error
        for path in sorted(files)
        for error in violations(
            path.relative_to(root), path.read_text(encoding="utf-8")
        )
    ]
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        print(f"source-policy: {len(errors)} violation(s)", file=sys.stderr)
        return 1
    print(f"source-policy: {len(files)} files checked")
    return 0


if __name__ == "__main__":
    sys.exit(main())
