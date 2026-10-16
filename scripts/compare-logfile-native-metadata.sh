#!/usr/bin/env bash
# CI-only: freeze independent selection before any production comparison.
set -euo pipefail
mkdir -p tmp/logfile-record-oracle
source scripts/logfile-native-extraction.sh
source scripts/logfile-checkpoint-captures.sh
checkpoints=$(logfile_checkpoints_mode)
bash tests/scripts/logfile-native-extraction.sh > tmp/logfile-record-oracle/native-extraction-controls.stderr 2>&1 || {
  echo 'native-extraction controls failed: tmp/logfile-record-oracle/native-extraction-controls.stderr' >&2
  exit 1
}
export CARGO_BUILD_JOBS=2
nice -n 10 cargo build --quiet --locked --manifest-path scripts/logfile-record-oracle/Cargo.toml --target-dir tmp/logfile-record-oracle/target
nice -n 10 cargo test --quiet --locked --manifest-path scripts/logfile-record-oracle/Cargo.toml --target-dir tmp/logfile-record-oracle/target --bin logfile-record-oracle native_metadata_controls > tmp/logfile-record-oracle/native-metadata-controls.stderr 2>&1
oracle=tmp/logfile-record-oracle/target/debug/logfile-record-oracle
if [[ "$checkpoints" == live ]]; then
python3 - <<'PY'
import hashlib, json, pathlib
root = pathlib.Path('test-disks')
p = json.loads((root / 'logfile-checkpoint-provenance.json').read_text())
assert len(p['candidates']) == 3, 'native-provenance: three workload candidates required'
for candidate, row in enumerate(p['candidates'] + [p['checkpoint_probe']], 1):
    assert row['candidate'] == candidate, f'native-provenance: candidate {candidate} identity mismatch'
    assert row['image'] == f'logfile-checkpoint-{candidate}.img', f'native-provenance: candidate {candidate} image mismatch'
    path = root / row['image']
    assert path.is_file(), f'native-provenance: candidate {candidate} missing; run test-disks/build-windows-logfile-checkpoints.ps1'
    assert 0 < row['partition_bytes'] <= 128 * 1024 * 1024, f'native-provenance: candidate {candidate} partition bound'
    assert path.stat().st_size == row['partition_bytes'], f'native-provenance: candidate {candidate} size mismatch'
    assert hashlib.file_digest(path.open('rb'), 'sha256').hexdigest() == row['sha256'], f'native-provenance: candidate {candidate} SHA-256 mismatch'
PY
fi
metadata=
length=
status=0
inventory=tmp/logfile-record-oracle/native-extraction.tsv
printf 'candidate\timage\texit\tbytes\tstate\tstderr\n' > "$inventory"
for candidate in 1 2 3 4; do
  raw="tmp/logfile-record-oracle/native-$candidate.bin"
  if [[ "$checkpoints" == pinned ]]; then
    diagnostic="tmp/logfile-record-oracle/native-$candidate-extraction.stderr"
    logfile_pinned_capture "$candidate" "$raw" 2> "$diagnostic" || {
      echo "pinned capture $candidate refused; see native-$candidate-extraction.stderr" >&2
      exit 1
    }
    printf '%s\t%s\t0\t%s\tpinned\t%s\n' "$candidate" "test-disks/logfile-checkpoint-$candidate.logfile.gz" \
      "$(wc -c < "$raw")" "$diagnostic" >> "$inventory"
  elif ! logfile_native_extract "$candidate" "test-disks/logfile-checkpoint-$candidate.img" "$raw" "tmp/logfile-record-oracle/native-$candidate-extraction.stderr" "$inventory"; then
    # A capture the independent extractor cannot read is named and its
    # evidence retained, and it supplies no selection: ntfs-3g refused the
    # first snapshot ("Failed to open $Secure") in four of five merge-group
    # runs (38925071276, 38933834125, 38933837162, 38933840377), a snapshot
    # taken before NTFS had written $Secure out. A mode no readable capture
    # supplies still fails below, and the decoder never sees empty or
    # partial data.
    echo "native candidate $candidate refused by the independent extractor; evidence retained in native-$candidate-extraction.stderr" >&2
    for mode in metadata checkpoint-length; do
      printf '%s precondition: native candidate %s extraction refused; see native-%s-extraction.stderr\n' "$mode" "$candidate" "$candidate" > "tmp/logfile-record-oracle/native-$candidate-$mode-selection.tsv.stderr"
    done
    continue
  fi
  for mode in metadata checkpoint-length; do
    report="tmp/logfile-record-oracle/native-$candidate-$mode-selection.tsv"
    if "$oracle" "--select-$mode" "$raw" > "$report" 2> "$report.stderr"; then
      if [[ "$mode" == metadata && -z "$metadata" ]]; then metadata=$candidate; fi
      if [[ "$mode" == checkpoint-length && -z "$length" ]]; then length=$candidate; fi
    fi
  done
done
for mode in metadata checkpoint-length; do
  if [[ "$mode" == metadata ]]; then selected=$metadata; else selected=$length; fi
  if [[ -z "$selected" ]]; then
    echo "$mode precondition absent in all native captures; inspect selection stderr and quiescent volume-flush probe" >&2
    status=1
    continue
  fi
  raw="tmp/logfile-record-oracle/native-$selected.bin"
  report="tmp/logfile-record-oracle/native-$selected-$mode-selection.tsv"
  sha=$(awk -F '\t' 'NR==2 {print $2}' "$report")
  [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || { echo 'Missing independent selection digest' >&2; exit 1; }
  wrong_sha=0000000000000000000000000000000000000000000000000000000000000000
  if "$oracle" "--select-$mode" "$raw" "$wrong_sha" > /dev/null 2> "tmp/logfile-record-oracle/native-$mode-wrong-sha.stderr"; then
    echo 'Native selector accepted a wrong capture digest' >&2; exit 1
  fi
  grep -qF -- 'capture SHA-256 mismatch' "tmp/logfile-record-oracle/native-$mode-wrong-sha.stderr" \
    || { echo "Wrong-digest refusal for $mode lacked the capture SHA-256 mismatch diagnostic" >&2; exit 1; }
  "$oracle" "--select-$mode" "$raw" "$sha" > "$report.frozen"
  cmp "$report" "$report.frozen"
  # Both routes execute even if production refuses one. Never try a replacement.
  if ! "$oracle" "--compare-$mode" "$raw" "$sha" > "tmp/logfile-record-oracle/native-$mode-comparison.tsv" 2> "tmp/logfile-record-oracle/native-$mode-comparison.stderr"; then status=1; fi
done
exit "$status"
