### Added
- `fs.ntfs acl show` decodes ACCESS_ALLOWED and ACCESS_DENIED ACEs: the numeric `mask`, its documented rights by exact Windows constant name, the `unknown_mask` bits no name covers, and the trustee `sid`. Every other ACE type reports those fields as null and keeps its raw bytes; no effective access is computed.
