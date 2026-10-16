#!/usr/bin/env bash
# Stand-in transport controls, never a fabricated native decoder positive.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$ROOT/scripts/logfile-native-extraction.sh"
mkdir -p "$ROOT/tmp/logfile-record-oracle"
WORK=$(mktemp -d "$ROOT/tmp/logfile-record-oracle/extraction-controls.XXXXXX")
fails=0
ok() { printf 'ok %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }
# POSIX grep only: CI runners carry no rg, and a missing matcher must never read as absence.
has_text() {
  [[ -r "$2" ]] || { printf 'missing diagnostic file %s\n' "$2" >&2; return 2; }
  grep -qF -- "$1" "$2"
}
printf 'present\n' > "$WORK/matcher-control.txt"
has_text present "$WORK/matcher-control.txt" && ok 'matcher finds present text' || fail 'matcher missed present text'
has_text absent "$WORK/matcher-control.txt"; [[ $? == 1 ]] && ok 'matcher refuses absent text' || fail 'matcher accepted absent text'
has_text present "$WORK/no-such-diagnostic" 2> /dev/null; [[ $? == 2 ]] && ok 'matcher names missing file' || fail 'matcher hid missing file'
ntfscat() {
  [[ "$1 $2 $3" == '-f -i 2' && "$#" == 4 ]] || return 91
  case "$4" in
    refused.img) printf 'Failed to open $Secure: No such file or directory\n' >&2; return 1 ;;
    partial.img) printf 'partial'; printf 'native refusal\n' >&2; return 7 ;;
    empty.img) return 0 ;;
    valid.img) printf 'transport-control-bytes' ;;
    *) return 92 ;;
  esac
}
status=0
for image in refused.img valid.img partial.img empty.img; do
  if logfile_native_extract "$image" "$image" "$WORK/$image.bin" "$WORK/$image.stderr" "$WORK/inventory.tsv" 2> "$WORK/$image.diagnostic"; then
    [[ "$image" == valid.img ]] && ok 'successful nonempty extraction' || fail "accepted $image"
  else
    status=1
    [[ "$image" != valid.img ]] && ok "refused $image" || fail 'refused valid transport'
    has_text "native-extraction precondition: candidate=$image image=$image exit=" "$WORK/$image.diagnostic" && ok "named failure $image" || fail "missing diagnostic $image"
  fi
done
[[ "$status" == 1 && $(wc -l < "$WORK/inventory.tsv") -eq 4 ]] && ok 'first refusal stays red while all captures retain evidence' || fail 'lost refusal or stopped inventory'
[[ $(cat "$WORK/valid.img.bin") == transport-control-bytes ]] && ok 'valid bytes preserved after first refusal' || fail 'valid transport lost'
[[ $(cat "$WORK/partial.img.bin") == partial ]] && ok 'refused partial bytes retained as evidence' || fail 'partial bytes lost'
has_text 'Failed to open $Secure' "$WORK/refused.img.diagnostic" && ok 'native refusal surfaced' || fail 'tool error hidden'
has_text 'native-extraction precondition' "$WORK/valid.img.diagnostic"; [[ $? == 1 ]] && ok 'successful extraction carries no refusal' || fail 'successful extraction reported a refusal'
if (( fails > 0 )); then exit 1; fi
echo 'logfile-native-extraction: all checks passed'
