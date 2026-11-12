## Interfaces

| Windows chkdsk grading of retained operation samples (test support) | Experimental: a windows-latest job runs read-only `chkdsk` on every retained `sample.img` and fails on any reported error or any ungraded sample, keeping `chkdsk.txt` beside `ntfsfix.txt` and `geometry.txt` in the `op-sequence-chkdsk` artifact | Unreleased | #1035, #1032 | `scripts/check-op-sequence-chkdsk.sh`, `.github/workflows/ci.yml` (`op-sequence-chkdsk`) |
