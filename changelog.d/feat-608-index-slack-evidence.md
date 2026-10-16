### Added
- A bounded internal scanner reports complete directory-index allocation slack entries with their offsets, names, and unverified references.

### Fixed
- Complete leaf terminal headers in index slack are consumed before scanning resumes, preventing their length and flags from being reported as a file reference.
