## Interfaces

| Windows 512e format capture beside 512n (test support) | Experimental: hosted Windows formats a 512e and a 512n disk of the same size, refuses a disk whose `Get-Disk` sector sizes differ from the request, records each format's row with its partition offset, and writes `sector-512e-diff.json` naming every compared field that differs; `$LogFile`'s first LCN and clusters are not captured (#901), and the formatter does not yet use the rows (#902) | Unreleased (#904) | #901, #702, #588 | `tests/scripts/windows-format-geometry.ps1`, `.github/workflows/format-geometry.yml` |
