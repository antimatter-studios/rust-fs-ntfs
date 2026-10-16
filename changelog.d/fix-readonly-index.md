### Fixed
- Path-based readonly changes update only the addressed directory index key's readonly bit, preserving the lazy MFT `$FILE_NAME` copy and other names. Exact-head full-suite and Windows chkdsk validation remain pending.
