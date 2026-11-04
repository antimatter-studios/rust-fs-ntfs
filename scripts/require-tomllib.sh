#!/usr/bin/env bash
# require-tomllib.sh NAME
#
# The one check a guard needing tomllib makes first, as
#   bash "$ROOT/scripts/require-tomllib.sh" "$(basename "$0")" || exit 1
# Silent when python3 on PATH can import tomllib. Otherwise one line naming the
# required version, what provides it and what was found, and status 1: the
# caller fails rather than skips, and its own checks never run to report the
# same cause as a traceback or a list of unrelated failures (#1265).
#
# scripts/fs-windows-test-harness/run-host-steps and check-matrix say the same
# from their own import, because each also runs alone, outside any guard.
python3 -c 'import tomllib' >/dev/null 2>&1 && exit 0
found="$(python3 --version 2>&1)" || found='no python3 on PATH'
printf '%s: needs Python 3.11 or newer, whose standard library provides tomllib; found %s. Put a newer python3 first on PATH (on a Mac, brew install python@3.12).\n' \
    "${1:-require-tomllib.sh}" "$found" >&2
exit 1
