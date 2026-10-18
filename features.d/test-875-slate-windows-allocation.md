## Interfaces

| Slate Windows allocation manifests (test support) | Experimental: Windows records each advisory Slate image's cluster size and total, free and reserved clusters, and every user stream's extents and allocated clusters, through the same disposable mount as the read manifest; holes are explicit, reparse points and unreadable streams fail by path; Slate comparison remains #877 | Unreleased | #875, #692, #580 | `tests/scripts/slate-windows-allocation.ps1` (14 contracts), `test-disks/capture-slate-windows-allocation.ps1`, `.github/workflows/slate-oracle.yml` |
