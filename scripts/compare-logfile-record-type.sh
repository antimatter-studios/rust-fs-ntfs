#!/usr/bin/env bash
# Run only in CI: seven checkpoint fields and current-record client-data length.
set -euo pipefail
oracle_repo_root=$(pwd -P)
mkdir -p tmp/logfile-record-oracle
source scripts/logfile-checkpoint-captures.sh
checkpoints=$(logfile_checkpoints_mode)
manifest=test-disks/logfile-record-checkpoint.tsv
[[ $(wc -l < "$manifest") -eq 2 ]] || { echo 'Missing or ambiguous checkpoint manifest' >&2; exit 1; }
IFS=$'\t' read -r capture_name capture_sha oracle_revision lfs_major checkpoint_lsn < <(tail -n +2 "$manifest")
[[ "$capture_name" == windows-interrupted-logfile-1 && "$capture_sha" =~ ^[0-9a-f]{64}$ ]] || { echo 'Invalid immutable checkpoint identity' >&2; exit 1; }
[[ -f "test-disks/$capture_name.bin.gz" ]] || { echo "Missing immutable Windows capture: restore test-disks/$capture_name.bin.gz from git (producer: test-disks/capture-interrupted-logfile.ps1)" >&2; exit 1; }
cp "$manifest" tmp/logfile-record-oracle/checkpoint-manifest.tsv
gzip -dc "test-disks/$capture_name.bin.gz" > tmp/logfile-record-oracle/captured.bin
export CARGO_BUILD_JOBS=2
nice -n 10 cargo build --quiet --locked --manifest-path scripts/logfile-record-oracle/Cargo.toml \
  --target-dir tmp/logfile-record-oracle/target
oracle=tmp/logfile-record-oracle/target/debug/logfile-record-oracle
nice -n 10 cargo test --quiet --locked --manifest-path scripts/logfile-record-oracle/Cargo.toml \
  --target-dir tmp/logfile-record-oracle/target --bin logfile-record-oracle selector_controls \
  > tmp/logfile-record-oracle/selector-controls.stderr 2>&1
"$oracle" --select-checkpoint tmp/logfile-record-oracle/captured.bin \
  "$capture_sha" \
  > tmp/logfile-record-oracle/captured-identity.tsv
IFS=$'\t' read -r actual_sha actual_revision actual_major actual_minor actual_lsn actual_current < <(tail -n +2 tmp/logfile-record-oracle/captured-identity.tsv)
[[ "$actual_sha" == "$capture_sha" && "$actual_revision" == "$oracle_revision" && "$actual_major" == "$lfs_major" && "$actual_lsn" == "$checkpoint_lsn" ]] || { echo 'Independent checkpoint identity differs from manifest' >&2; exit 1; }
"$oracle" --checkpoint-only tmp/logfile-record-oracle/captured.bin \
  "$capture_sha" \
  > tmp/logfile-record-oracle/captured.tsv
if [[ "$checkpoints" == pinned ]]; then
  cp test-disks/logfile-checkpoint-captures.json tmp/logfile-record-oracle/
else
# Verify Windows' image digest before external extraction. No implementation is linked.
cp test-disks/logfile-checkpoint-provenance.json tmp/logfile-record-oracle/
python3 - <<'PY'
import hashlib, json, pathlib
root = pathlib.Path('test-disks')
rows = json.loads((root / 'logfile-checkpoint-provenance.json').read_text())['candidates']
assert len(rows) == 3
for row in rows:
    path = root / row['image']
    assert path.stat().st_size == row['partition_bytes']
    assert hashlib.file_digest(path.open('rb'), 'sha256').hexdigest() == row['sha256']
PY
fi
selected=
selected_sha=
pair_selected=
pair_sha=
pair_report=
for candidate in 1 2 3; do
  raw="tmp/logfile-record-oracle/candidate-$candidate.bin"
  if [[ "$checkpoints" == pinned ]]; then
    logfile_pinned_capture "$candidate" "$raw" \
      2> "tmp/logfile-record-oracle/candidate-$candidate-extraction.stderr" || {
      echo "pinned capture $candidate refused; see candidate-$candidate-extraction.stderr" >&2
      exit 1
    }
  elif ! ntfscat -f -i 2 "test-disks/logfile-checkpoint-$candidate.img" > "$raw" \
      2> "tmp/logfile-record-oracle/candidate-$candidate-extraction.stderr"; then
    echo "Windows candidate $candidate extraction failed; evidence retained" >&2
    continue
  fi
  if "$oracle" --select-checkpoint "$raw" \
      > "tmp/logfile-record-oracle/candidate-$candidate-identity.tsv" \
      2> "tmp/logfile-record-oracle/candidate-$candidate.stderr"; then
    if [ -z "$selected" ]; then
      selected=$raw
      selected_sha=$(awk -F '\t' 'NR == 2 {print $1}' "tmp/logfile-record-oracle/candidate-$candidate-identity.tsv")
      [[ "$selected_sha" =~ ^[0-9a-f]{64}$ ]] || { echo 'Missing selected digest' >&2; exit 1; }
    fi
  fi
  # This independent route examines all three existing Windows candidates.
  # It never changes the checkpoint choice or asks production to select a pair.
  candidate_report="tmp/logfile-record-oracle/candidate-$candidate-pair.tsv"
  if "$oracle" --select-complete-pair "$raw" > "$candidate_report" \
      2> "tmp/logfile-record-oracle/candidate-$candidate-pair.stderr"; then
    if [ -z "$pair_selected" ]; then
      pair_selected=$raw
      pair_report=$candidate_report
      pair_sha=$(awk -F '\t' 'NR == 2 {print $1}' "$pair_report")
      [[ "$pair_sha" =~ ^[0-9a-f]{64}$ ]] || { echo 'Missing complete-pair digest' >&2; exit 1; }
    fi
  fi
done
if [ -z "$selected" ]; then
  echo 'No independently verified LFS 2.x checkpoint among three Windows captures' >&2
  exit 1
fi
# Freeze the independently chosen pair and recheck the source digest. A
# disagreement never falls through to another candidate or relaxes lookup.
if [ -n "$pair_selected" ]; then
  "$oracle" --select-complete-pair "$pair_selected" "$pair_sha" \
    > tmp/logfile-record-oracle/fresh-complete-pair.tsv \
    2> tmp/logfile-record-oracle/fresh-complete-pair.stderr
  cmp "$pair_report" tmp/logfile-record-oracle/fresh-complete-pair.tsv
  # Cargo runs this test in its package directory, so cross that boundary
  # with absolute paths to the already frozen repository-root evidence.
  LOGFILE_PAIR_RAW="$oracle_repo_root/$pair_selected" LOGFILE_PAIR_SHA="$pair_sha" \
    LOGFILE_PAIR_REPORT="$oracle_repo_root/tmp/logfile-record-oracle/fresh-complete-pair.tsv" \
    nice -n 10 cargo test --quiet --locked --manifest-path scripts/logfile-record-oracle/Cargo.toml \
      --target-dir tmp/logfile-record-oracle/target --bin logfile-record-oracle \
      fresh_pair_report_matches_genuine_independent_records \
      > tmp/logfile-record-oracle/fresh-pair-validation.stderr 2>&1
fi
# Never select a different candidate because our lookup disagrees.
"$oracle" --checkpoint-only "$selected" "$selected_sha" > tmp/logfile-record-oracle/fresh-windows.tsv
echo 'logfile headers: 16 independent comparisons passed (tmp/logfile-record-oracle/)'
if [ -z "$pair_selected" ]; then
  echo 'No complete independently decoded nonzero record pair among three fresh Windows captures; pointer occurrences do not qualify' >&2
  exit 1
fi
echo 'logfile complete pair: complete physical bytes and both nonzero traversals passed (tmp/logfile-record-oracle/fresh-pair-physical.tsv)'
