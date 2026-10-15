### Added
- A bounded, allocation-free Rust view decodes self-relative security descriptor headers and optional owner/group SIDs, retaining every control bit, the full 48-bit SID authority and the original bytes. ACL decoding and authorization remain outside this API.
