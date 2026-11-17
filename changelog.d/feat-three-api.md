### Added
- Separate Rust FileAPI, PrimitiveAPI, and VolumeAPI interfaces sharing the existing NTFS readers, writers, and device transport, while retaining the existing C ABI and facade.
- Persistent normal filesystem handles with volume-instance and MFT-sequence validation, read-only capability enforcement, and explicit writable mount checks.
- Raw FILE-record evidence and bounded extraction from resident or complete uncompressed, unencrypted non-resident streams without requiring a working namespace; torn fixups and unresolved attribute extents remain explicit limitations.
