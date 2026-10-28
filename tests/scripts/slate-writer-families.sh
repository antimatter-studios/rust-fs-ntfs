#!/usr/bin/env bash
# The Slate writer tier requires every mutation family to have an executed
# case, by name (#830, part of #688). `slate-oracle.sh families` reads the
# verdicts table compare wrote and tests/slate-writer-families.tsv, which says
# which captured matrix scenario writes which family, and fails naming each
# family left with no agreed or recorded case, even when compare's global
# floor was met. A duplicate case, or a row that is not a well-formed graded
# result, satisfies no family, and compare does not count a duplicate case
# towards its executed-case floor either.
#
# This drives the subcommand with stand-in verdict tables: the complete
# manifest passes, removing the last case of each family fails by that
# family's name, and duplicate or malformed evidence covers nothing. It also
# reads the family table and the workflow as data, so the table names only
# captured scenarios and the advisory job runs the gate.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/slate-writer-families.XXXXXX")"
trap 'chmod -R u+w "$WORK" 2>/dev/null; rm -rf "$WORK"' EXIT
TABLE="$ROOT/tests/slate-writer-families.tsv"
WORKFLOW="$ROOT/.github/workflows/slate-oracle.yml"
fails=0
ok() { :; }
fail() { fails=$((fails + 1)); printf 'FAIL %s\n' "$1"; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

# The families #688 and #1065 name. The gate holds its own list, so a family
# cannot be dropped by deleting its rows from the table.
required='create overwrite extend truncate mkdir rename unlink rmdir named-stream ea reparse object-id security-descriptor timestamps'

[ -f "$TABLE" ] || { fail "no family table at $TABLE"; echo 'slate-writer-families.sh: 1 checks failed'; exit 1; }

# scenarios_for FAMILY: the table's scenarios that write FAMILY.
scenarios_for() { awk -F'\t' -v f="$1" 'NR > 1 && $1 == f { sub(/\r$/, "", $2); print $2 }' "$TABLE"; }
scenarios="$(awk -F'\t' 'NR > 1 && $2 != "" { sub(/\r$/, "", $2); print $2 }' "$TABLE" | sort -u)"

# The table: its header, a row for every required family, known families only,
# and only scenarios the writer-cases job captures.
[ "$(head -n 1 "$TABLE" | tr -d '\r')" = $'family\tscenario' ] && ok ||
    fail "the family table's header is not 'family<TAB>scenario'"
for family in $required; do
    [ -n "$(scenarios_for "$family")" ] && ok || fail "the family table has no scenario for $family"
done
while IFS=$'\t' read -r family scenario; do
    [ "$family" = family ] && continue
    case " $required " in *" $family "*) ;; *) fail "the family table names an unknown family '$family'" ;; esac
    grep -q -- "--scenario ${scenario%$'\r'} " "$WORKFLOW" && ok ||
        fail "the family table names ${scenario%$'\r'}, which the writer-cases job does not capture"
done <"$TABLE"

# The workflow runs the gate on compare's table, whatever compare concluded,
# and changes to the gate re-run it.
grep -q 'bash scripts/slate-oracle.sh families tests/slate-writer-families.tsv tmp/slate-oracle/out/verdicts.tsv' "$WORKFLOW" &&
    ok || fail "the slate-check job does not run slate-oracle.sh families on compare's verdicts"
for path in tests/slate-writer-families.tsv tests/scripts/slate-writer-families.sh; do
    grep -q -- "- $path\$" "$WORKFLOW" && ok || fail "$path is not in the slate-oracle workflow's path filter"
done

HEADER=$'case\torigin\tsha256\twindows_verdict\twindows_exit\tslate_verdict\tslate_exit\tresult'
DIGEST=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

# verdicts [SKIP...]: a complete verdicts table, one agreed case per captured
# scenario plus the four format controls, without the scenarios named. Each
# skipped scenario is replaced by another format control, so the row count,
# and with it compare's floor, is unchanged.
verdicts() {
    local skip=" $* " name pad=0
    printf '%s\n' "$HEADER" >"$WORK/verdicts.tsv"
    for name in win-4k win-64k ours-4k ours-64k; do
        printf '%s\t%s\t%s\tclean\t0\tclean\t0\tagree\n' "$name" "${name%%-*}" "$DIGEST" >>"$WORK/verdicts.tsv"
    done
    for name in $scenarios; do
        case "$skip" in
            *" $name "*)
                pad=$((pad + 1))
                printf 'pad-%s\tours\t%s\tclean\t0\tclean\t0\tagree\n' "$pad" "$DIGEST" >>"$WORK/verdicts.tsv" ;;
            *) printf '%s\twriter:%s\t%s\tclean\t0\tclean\t0\tagree\n' "$name" "$name" "$DIGEST" >>"$WORK/verdicts.tsv" ;;
        esac
    done
}

families() {
    bash "$ROOT/scripts/slate-oracle.sh" families "${1:-$TABLE}" "$WORK/verdicts.tsv" >"$WORK/log" 2>&1
}

# The complete manifest passes and names every family it covered.
verdicts
families
rc=$?
[ "$rc" -eq 0 ] && ok || fail "the complete manifest: exit $rc, want 0: $(cat "$WORK/log")"
for family in $required; do
    grep -q "^covered $family:" "$WORK/log" && ok || fail "the complete manifest does not report $family covered"
done

# Removing the last case of each family fails naming that family, though
# the table still has as many rows as before.
for family in $required; do
    # shellcheck disable=SC2046 # one argument per scenario
    verdicts $(scenarios_for "$family")
    families
    rc=$?
    [ "$rc" -eq 1 ] && ok || fail "$family with no executed case: exit $rc, want 1"
    grep -q "^FAIL family $family: no executed case" "$WORK/log" && ok ||
        fail "$family with no executed case is not named: $(cat "$WORK/log")"
done

# Duplicate or malformed evidence satisfies no family. Each case rewrites
# the row of the only scenario writing object IDs.
objid="$(scenarios_for object-id | head -n 1)"
corrupt() {
    verdicts
    awk -F'\t' -v OFS='\t' -v c="$objid" -v field="$1" -v value="$2" -v twice="${3:-}" '
        NR > 1 && $1 == c { if (field) $field = value; print; if (twice) print; next } { print }
    ' "$WORK/verdicts.tsv" >"$WORK/v" && mv "$WORK/v" "$WORK/verdicts.tsv"
    families
    rc=$?
    [ "$rc" -eq 1 ] && ok || fail "$4: exit $rc, want 1"
    grep -q "^FAIL family object-id: no executed case" "$WORK/log" && ok ||
        fail "$4 still covers object-id: $(cat "$WORK/log")"
}
if [ -z "$objid" ]; then
    fail "no scenario writes object IDs, so the evidence checks cannot run"
else
    corrupt '' '' twice 'the case listed twice'
    corrupt 8 disagree '' 'a disagreeing case'
    corrupt 8 hash-mismatch '' 'a case on bytes Windows did not grade'
    corrupt 4 not-clean '' 'a case Windows rejected'
    corrupt 3 not-a-digest '' 'a case with a malformed SHA-256'
    corrupt 2 "writer:no-such-scenario" '' 'a case whose origin is not its scenario'
    corrupt 2 ours '' 'a case whose origin is not a writer capture'

    # The same scenario under two case names is one scenario, not two cases.
    verdicts
    awk -F'\t' -v OFS='\t' -v c="$objid" 'NR > 1 && $1 == c { print; $1 = c "-again" } { print }' \
        "$WORK/verdicts.tsv" >"$WORK/v" && mv "$WORK/v" "$WORK/verdicts.tsv"
    families
    rc=$?
    [ "$rc" -eq 1 ] && grep -q "^FAIL family object-id: no executed case" "$WORK/log" && ok ||
        fail "one scenario under two case names: exit $rc, want 1 naming object-id: $(cat "$WORK/log")"
fi

# A table that is not a verdicts table, a family table without a required
# family, or one naming an unknown family, fails by name.
verdicts
printf 'case\tresult\n' >"$WORK/verdicts.tsv"
families
[ "$?" -eq 1 ] && grep -q 'is not a verdicts table' "$WORK/log" && ok ||
    fail "a table that is not a verdicts table: $(cat "$WORK/log")"
verdicts
grep -v $'^timestamps\t' "$TABLE" >"$WORK/table.tsv"
families "$WORK/table.tsv"
[ "$?" -eq 1 ] && grep -q '^FAIL family timestamps: no scenario in' "$WORK/log" && ok ||
    fail "a family table without timestamps: $(cat "$WORK/log")"
{ cat "$TABLE"; printf 'teleport\t%s\n' "$objid"; } >"$WORK/table.tsv"
families "$WORK/table.tsv"
[ "$?" -eq 1 ] && grep -q "unknown family 'teleport'" "$WORK/log" && ok ||
    fail "a family table naming an unknown family: $(cat "$WORK/log")"

# compare does not count a duplicate case towards its executed-case floor.
mkdir -p "$WORK/images"
printf 'clean\n' >"$WORK/images/a.img"
printf '#!/bin/sh\nexit 0\n' >"$WORK/checker"
chmod +x "$WORK/checker"
printf 'case\tregenerate\twindows_chkdsk\twindows_reading\tassigned\tissue\n' >"$WORK/ledger.tsv"
{
    printf 'case\torigin\timage\tsha256\twindows_verdict\twindows_exit\n'
    printf 'a\tours\ta.img\t%s\tclean\t0\n' "$(sha "$WORK/images/a.img")"
    printf 'a\tours\ta.img\t%s\tclean\t0\n' "$(sha "$WORK/images/a.img")"
} >"$WORK/cases.tsv"
SLATE_LEDGER="$WORK/ledger.tsv" bash "$ROOT/scripts/slate-oracle.sh" compare "$WORK/cases.tsv" "$WORK/images" \
    "$WORK/checker" "$WORK/out" 2 >"$WORK/log" 2>&1
rc=$?
[ "$rc" -eq 1 ] && grep -q '^FAIL a: the case is listed more than once' "$WORK/log" && ok ||
    fail "compare counted one case listed twice towards a floor of 2: exit $rc: $(cat "$WORK/log")"

if [ "$fails" -gt 0 ]; then
    echo "slate-writer-families.sh: $fails checks failed"
    exit 1
fi
echo 'slate-writer-families.sh: all checks passed'
