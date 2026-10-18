## Interfaces

| Slate Windows read manifests (test support) | Experimental: Windows records user paths, entry kinds and unnamed and named stream sizes and SHA-256 values for every advisory Slate image through a disposable mount; each manifest binds to the retained image hash, root metadata exclusions are explicit and unreadable paths fail; Slate comparison and expanded image coverage remain #857 and #858 | Unreleased (#859) | #856, #689, #580 | `tests/scripts/slate-windows-manifest.ps1` (11 contracts), `test-disks/capture-slate-windows-manifest.ps1`, `.github/workflows/slate-oracle.yml` |
