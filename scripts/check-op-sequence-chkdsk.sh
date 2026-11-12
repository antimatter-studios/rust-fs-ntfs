#!/usr/bin/env bash
# Every retained operation sample (#1034) graded by Windows chkdsk (#1035).
# Each sample directory must hold chkdsk's read-only report as chkdsk.txt and
# chkdsk's own exit code as chkdsk-exit.txt beside geometry.txt and
# ntfsfix.txt. A sample chkdsk reported errors on, or one it never graded,
# fails the check, so a run that graded fewer samples than the retained set
# cannot pass.
set -euo pipefail
root="${1:-tmp/op-sequence-model}"
IFS=, read -r -a samples <<<"$(bash scripts/check-op-sequence-samples.sh --list)"
fails=0
graded=0
for sample in "${samples[@]}"; do
    dir="${root}/${sample}"
    if [ ! -s "${dir}/chkdsk.txt" ] || [ ! -s "${dir}/chkdsk-exit.txt" ]; then
        echo "retained sample ${sample}: not graded by chkdsk (chkdsk.txt or chkdsk-exit.txt missing)" >&2
        fails=$((fails + 1))
        continue
    fi
    graded=$((graded + 1))
    code="$(tr -d '\r\n\t ' <"${dir}/chkdsk-exit.txt")"
    if [ "$code" != "0" ]; then
        echo "retained sample ${sample}: chkdsk exited ${code}; see ${dir}/chkdsk.txt" >&2
        fails=$((fails + 1))
    fi
done
if [ "$fails" -gt 0 ]; then
    echo "op-sequence chkdsk: ${fails} problems, ${graded} of ${#samples[@]} samples graded" >&2
    exit 1
fi
echo "op-sequence chkdsk: ${graded} of ${#samples[@]} samples clean"
