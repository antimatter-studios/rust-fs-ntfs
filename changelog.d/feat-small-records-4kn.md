### Added

- **Volumes on 4096-byte sectors with 1024-byte file records are read.** Windows formats a disk presenting 4096-byte sectors this way by default and keeps one update sequence word per 512 bytes of each record and index block. The boot sector is no longer refused for a record smaller than its sector, and the fixup reader takes the stride from each record's own array, the sector or 512 bytes, so every stride's tail is still checked. `PrimitiveAPI` inspects such records too (#1384).

### Changed

- A writable mount of a volume whose file records are smaller than its sectors is refused by name, and a write of such a record is refused, because the writers build update sequence arrays with the sector stride (#1384).
