# Complete Windows matrix — 2026-10-10

## Result

The full matrix passed **72/72 scenarios**, with all **438 recipe steps**
completed, zero ignored or skipped cases, and no retries. Runtime was
3,442.65 seconds (57 minutes 23 seconds), excluding preparation and the wait
for host capacity. Windows kept the same boot throughout the run. The host was a Raspberry Pi 5;
the Windows ARM64 guest used two vCPUs, 4 GiB RAM and a 128 GiB disk.

The NTFS test branch was `test/windows-index-allocation-insertion` at
`6f2c132ea24e64340c7d62cebbe4419ffaad2719`. The harness was
`357a6e98e2fb8ea86a5e3028647f41b7369c964b` from `feat/qemu-local-vm`.
Both host driver binaries were reused unchanged from source revision
`fe450f1798993cf1c8d7b4866a0855761d9aa5c6`. No Rust driver implementation
was changed. `result.json` records their hashes, the matrix/config hashes,
fixture hashes, Windows metadata, all scenario results and oracle verdicts.

Windows verified all 256 original names and `host-inserted.txt` in the
corrected insertion case. Both of its chkdsk modes scanned and exited zero.
All 64 scenarios that require chkdsk have a passing structured Windows
verdict; the remaining eight cases check host-side operations directly.

## Output budget

The passing run printed **1,381 lines / 100,339 bytes**, exceeding the old
1,300-line / 90,000-byte limits, so the initial tier wrapper exited **65**
after the test command exited zero. The measured caps are now **1,850 lines /
135,000 bytes**, approximately one third of headroom.

The exact retained output passes through `tier.sh` with the new limits. This
was an output-budget replay; the physical matrix was run once. The driver
binaries and scenario definitions stayed unchanged between the run and replay.

## Evidence collector correction

Concurrent stdout and stderr interleaved two libtest result lines. The old
collector therefore reported 70 passes and two unknowns on this 72-pass run.
The collector now reads the runner's structured `results.json`, rejects
missing/malformed/duplicate/unsupported records, and decorates only cases
that ran. Stale Windows verdicts cannot create phantom cases. The VM verdict
collector also receives the configured workdir's `diag` root explicitly.

A new regression guard failed before correction; all **22 script tests** now
pass. Running the corrected collector against the real Windows guest produced
`test-diagnostics/matrix-results.json` with 72 passes, zero failures and zero
unknowns. The collector and budget edits were made after the physical run;
they do not change driver code or test recipes.

## Online scan fallbacks

Four clean-volume cases could not create an online scan snapshot. Each recorded
`/scan` exit 11 as `not-scanned`, then passed the existing offline `/F /X`
fallback with exit zero and nothing to repair:

- `cli-windows-interrupted-index-vcn-fsck-replay-win-verify-chkdsk`
- `foreign-fragmented-indx-512-win-enumerate-chkdsk`
- `mac-format-tiny-32mib`
- `mac-format-volume-32mib-cluster-512`

These are recorded fallbacks under the current oracle contract. This run does
not establish successful online scans for those four cases. Expected failures
and repairs in the corruption/dirty-flag canaries are retained separately in
the verdicts.

## Host startup and remaining comparison

The host was busy with other builds, so the controller waited for load below
four before booting. Two earlier KVM launches failed with `KVM_CREATE_VM` /
`Cannot allocate memory`. Memory compaction, reclaiming T3-service file cache,
and a task-level preference for NUMA node 1 preceded the successful launch.
The inherited task policy had interleaved allocation across eight virtual NUMA
nodes. This host workaround is recorded; it is not implemented as automatic
recovery in the harness. The existing Windows disk was reused, SSH became
ready in 33 seconds, and the guest stayed running after image cleanup.

This is Linux ARM64 / Windows ARM64 validation. No matching VMware run or
macOS run was performed here. Compare the Mac run's revisions, matrix/config
hashes, completed steps and oracle states before retiring the VMware backend.

## Reproduce

Use this NTFS branch beside the tested harness revision and rust-fs-core
v0.3.7. Configure the persistent local guest in the ignored `.test-env`, supply
the documented Windows snapshot fixtures, and build the host driver binaries.
After `local-vm.py up` and readiness, run:

```sh
python3 ../fs-windows-test-harness/scripts/local-vm.py --state "$VM_STATE" exec -- \
  bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored matrix -- \
  bash scripts/run-matrix.sh

python3 ../fs-windows-test-harness/scripts/local-vm.py --state "$VM_STATE" exec -- \
  bash scripts/matrix-fetch-diag.sh

python3 ../fs-windows-test-harness/scripts/local-vm.py --state "$VM_STATE" exec -- \
  bash scripts/_matrix-collect-vm.sh "$FULL_MATRIX_LOG"
```

`FULL_MATRIX_LOG` must name the retained full raw run, such as
`tmp/logs/matrix.log`. The raw evidence archive is retained on the Pi under the
local VM state's directory; its filename, size and SHA-256 are in `result.json`.
