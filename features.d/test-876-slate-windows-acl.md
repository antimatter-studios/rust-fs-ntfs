## Interfaces

| Slate Windows security descriptor manifests (test support) | Experimental: Windows records each advisory Slate image's user files' and directories' owner and group SIDs, SDDL, DACL protection and every DACL ACE with its inherited flag, through the same disposable mount as the read manifest; unreadable descriptors and reparse points fail by path; Slate comparison remains open under #692 | Unreleased | #876, #692, #580 | `tests/scripts/slate-windows-acl.ps1` (16 contracts), `test-disks/capture-slate-windows-acl.ps1`, `.github/workflows/slate-oracle.yml` |
