#!/usr/bin/env bash
# Decide CI job results from Actions' `needs` object. Missing, cancelled,
# failed and unexpected skipped jobs are all red. ASan is advisory and is
# deliberately not among the aggregate's dependencies: it carries
# `continue-on-error: true` and is declared non-gating to ci-gate.
#
# CODE is the changes job's `code` output, decided by scripts/ci-plan.sh. It
# must be `true` (the full pipeline) or `false` (a quick run: a pull request
# or a push to main); anything else, an absent decision included, is red.
# Only on a quick run is a skipped job expected rather than red, and only one
# of the full jobs named below: the Windows fixtures, the jobs that read
# them, and the kernel oracle. A quick job is never excused a skip.
set -euo pipefail

case "${1:-}" in
    windows)
        filter='(.changes.result == "success") and
            ((.changes.outputs.mkfs == "true" and
              .["validate-mkfs-windows-run"].result == "success") or
             (.changes.outputs.mkfs == "false" and
              .["validate-mkfs-windows-run"].result == "skipped"))'
        ;;
    aggregate)
        # Two halves. EVERY job in `needs` must be green, so a job added to
        # ci-ok's `needs:` is judged before anyone edits this list; and each
        # job named here must be PRESENT, so `needs:` cannot quietly shrink.
        # The list once left out `semver`, and a failing semver job left
        # ci-ok green (#409). rust-fs-core's ci-gate holds `needs:` to every
        # job in ci.yml.
        filter='def full: ["windows-native-read-fixtures", "integration",
                "cli", "kernel", "python-volume-info-windows"];
            def ok($job): .result == "success" or
                ($code == "false" and .result == "skipped" and
                 (full | index($job)) != null);
            ($code == "true" or $code == "false") and
            (.changes.result == "success") and
            (to_entries | all(.value | type == "object") and
                all(.value as $v | .key as $k | $v | ok($k))) and
            (["test", "windows-native-read-fixtures", "integration",
              "changes", "validate-mkfs-windows", "cli", "kernel",
              "semver"] | all(. as $k | $needs | has($k) and
                (.[$k] | ok($k))))'
        ;;
    *)
        echo 'usage: ci-verdict.sh windows|aggregate' >&2
        exit 2
        ;;
esac

if ! printf '%s' "${GATE_NEEDS_JSON:-}" | jq -e --arg code "${CODE:-}" ". as \$needs | $filter" >/dev/null; then
    echo "CI $1 verdict failed: a required job failed, was cancelled, or was unexpectedly skipped" >&2
    exit 1
fi
