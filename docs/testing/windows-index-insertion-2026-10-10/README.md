# Windows index allocation insertion regression

The old matrix scenario expected `touch` to refuse a Windows-created wide
root directory. The driver has supported bounded index allocation insertion
since 0.8.0; the previous full QEMU run failed because this expectation was
stale, not because insertion failed.

The replacement formats a 256 MiB image in Windows, creates 256 files there,
inserts `host-inserted.txt` with the unchanged host driver, and requires clean
Windows read-only chkdsk and /scan results. Windows enumeration must contain
the inserted file and every original file (`file_000.txt` through
`file_255.txt`). It uses the existing successful `mac-touch` operation.

## Result

The targeted run passed all nine steps: one scenario passed, none failed or
were ignored. Both chkdsk modes actually scanned and exited zero; neither
used a fallback. All 257 required names appeared among 263 listing entries
(the listing includes Windows metadata). Runtime: 346.39 seconds.

The 20-script baseline passed. A new regression guard failed on the old
scenario, then all 21 script tests passed with this correction. Matrix lint
and `git diff --check` passed. The matrix log was 104 lines / 5,901 bytes,
within the existing output budget. See `result.json` for input and evidence
hashes, step results and the unchanged driver binary hash. Committed Windows
text has normalized line endings and trailing whitespace; raw bytes remain
in the local evidence archive.

## Limits and next validation

This is targeted validation on Linux ARM64 with a Windows ARM64 QEMU guest.
It does not replace `test-diagnostics/matrix-results.json`, convert the
historical 71/72 full run into a pass, or prove VMware or macOS parity.
Re-run all 72 scenarios on this test branch with the QEMU harness branch,
then measure the complete green log before changing the matrix output budget.
No driver implementation was changed.

Run the scenario through the normal provider exec and budget wrapper:

```sh
python3 ../fs-windows-test-harness/scripts/local-vm.py --state "$VM_STATE" exec -- \
  bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored matrix -- \
  bash scripts/run-matrix.sh win-format-win-write-many-mac-insert-index-allocation-win-chkdsk
```

Use the same command without the final scenario filter for the complete matrix.
