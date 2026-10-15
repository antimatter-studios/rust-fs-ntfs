### Added
- A bounded, allocation-free Rust view decodes self-relative security descriptor headers and optional owner/group SIDs, retaining every control bit, the full 48-bit SID authority and the original bytes. ACL and ACE lengths are validated against their declared bounds, with ordered ACE types, flags and raw payloads preserved. ACE semantics and authorization remain outside this API.
