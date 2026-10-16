#!/usr/bin/env bash
# Extraction contract only: success here does not attest native record validity.
logfile_native_extract() {
  local candidate=$1 image=$2 raw=$3 diagnostic=$4 inventory=$5
  local result=0 bytes state
  ntfscat -f -i 2 "$image" > "$raw" 2> "$diagnostic" || result=$?
  bytes=$(wc -c < "$raw")
  state=accepted
  if (( result != 0 || bytes == 0 || bytes > 67108864 )); then state=refused; fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$candidate" "$image" "$result" "$bytes" "$state" "$diagnostic" >> "$inventory"
  if [[ "$state" == refused ]]; then
    printf 'native-extraction precondition: candidate=%s image=%s exit=%s bytes=%s; regenerate with test-disks/build-windows-logfile-checkpoints.ps1; evidence=%s\n' "$candidate" "$image" "$result" "$bytes" "$diagnostic" >&2
    # Show bounded tool evidence, including refusals that emitted partial bytes.
    head -c 2048 "$diagnostic" >&2
    printf '\n' >&2
    return 1
  fi
  return 0
}
