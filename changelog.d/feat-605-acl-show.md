### Added
- `fs.ntfs acl show PATH` reports a file's persisted security descriptor as JSON: owner and group SIDs, raw control bits, and each ACL with an explicit absent, null, empty or present state and its ACEs in on-disk order with raw type, flags and bytes, unknown ACE types included. It is read-only and evaluates no access.

### Fixed
- Path-based readonly changes update only the addressed directory index key's readonly bit, preserving the lazy MFT `$FILE_NAME` copy and other names. Exact-head full-suite and Windows chkdsk validation remain pending.
