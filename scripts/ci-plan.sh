#!/usr/bin/env bash
# Decide the CI run's depth from its event: writes `code=true` (the full
# pipeline) or `code=false` (the quick checks) to GITHUB_OUTPUT.
#
# Pull requests, every layer of a stack included, run everything. The merge
# queue also runs everything on the exact tree it is about to land, as do
# tags, manual dispatches and anything unexpected. Only a push to main does
# not repeat the queue's work: what lands on main is what the queue ran in full.
#
# EVENT is github.event_name and REF is github.ref. tests/scripts/ci-plan.sh
# holds every event to this table, and scripts/ci-verdict.sh refuses a run
# whose decision is missing or is neither `true` nor `false`.
set -euo pipefail

code=true
case "${EVENT:-}" in
    pull_request|merge_group) code=true ;;
    push) [ "${REF:-}" != refs/heads/main ] || code=false ;;
esac
echo "full pipeline: $code (${EVENT:-no event} ${REF:-no ref})"
echo "code=$code" >> "${GITHUB_OUTPUT:?GITHUB_OUTPUT is not set}"
