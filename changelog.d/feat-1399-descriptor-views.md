### Added

- C ABI `fs_ntfs_sd_parse` and `fs_ntfs_sd_ace` decode a caller-held self-relative security descriptor into offsets over the same buffer, with ACL states, ACEs in stored order and a located error for malformed input (#1399).
- `security_descriptor::Ace::fields` decodes basic, object, audit and callback ACE layouts and keeps callback application data as raw bytes (#1399).
