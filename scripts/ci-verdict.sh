#!/usr/bin/env bash
# Decide CI job results from Actions' `needs` object. Missing, cancelled,
# failed and unexpected skipped jobs are all red. ASan is advisory and is
# deliberately not among the aggregate's dependencies: it carries
# `continue-on-error: true` and is declared non-gating to ci-gate.
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
        filter='([.[] | .result] | all(. == "success")) and
            ([.test.result, .["windows-native-read-fixtures"].result,
              .integration.result, .changes.result,
              .["validate-mkfs-windows"].result, .cli.result,
              .semver.result] | all(. == "success"))'
        ;;
    *)
        echo 'usage: ci-verdict.sh windows|aggregate' >&2
        exit 2
        ;;
esac

if ! printf '%s' "${GATE_NEEDS_JSON:-}" | jq -e "$filter" >/dev/null; then
    echo "CI $1 verdict failed: a required job failed, was cancelled, or was unexpectedly skipped" >&2
    exit 1
fi
