#!/usr/bin/env bash
# The Windows checkpoint captures the $LogFile comparisons read (#620).
#
# Pinned by default: the $LogFile bytes of one Windows capture, extracted by
# ntfs-3g and committed with their provenance in
# test-disks/logfile-checkpoint-captures.json, so a commit gets the same
# verdict on every run. LOGFILE_CHECKPOINTS=live reads this run's own Windows
# images instead, as .github/workflows/logfile-checkpoint-drift.yml does on a
# schedule. Those snapshots are taken while Windows is writing, and what each
# one holds depends on timing, which is why they do not gate a merge.

logfile_checkpoints_mode() {
  case "${LOGFILE_CHECKPOINTS:-pinned}" in
    pinned | live) printf '%s\n' "${LOGFILE_CHECKPOINTS:-pinned}" ;;
    *)
      echo "LOGFILE_CHECKPOINTS must be pinned or live, not ${LOGFILE_CHECKPOINTS}" >&2
      return 1
      ;;
  esac
}

# logfile_pinned_capture CANDIDATE RAW: the committed $LogFile bytes of
# CANDIDATE, written to RAW only if they are the bytes the manifest names.
logfile_pinned_capture() {
  python3 - "$1" "$2" <<'PY'
import gzip, hashlib, json, pathlib, sys

candidate, raw = int(sys.argv[1]), pathlib.Path(sys.argv[2])
root = pathlib.Path('test-disks')
manifest = json.loads((root / 'logfile-checkpoint-captures.json').read_text())
rows = [r for r in manifest['captures'] if r['candidate'] == candidate]
if len(rows) != 1:
    sys.exit(f'pinned capture {candidate}: {len(rows)} manifest rows, not one')
row = rows[0]
data = gzip.decompress((root / row['logfile']).read_bytes())
if len(data) != row['logfile_bytes']:
    sys.exit(f'pinned capture {candidate}: {len(data)} bytes, the manifest says {row["logfile_bytes"]}')
if hashlib.sha256(data).hexdigest() != row['logfile_sha256']:
    sys.exit(f'pinned capture {candidate}: SHA-256 differs from the manifest')
raw.write_bytes(data)
PY
}
