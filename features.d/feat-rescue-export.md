## Checking and recovery

| Rescue export (`fs_ntfs::rescue::export_to_path`): a complete rescue archive written to a new image through a temporary file that is flushed and read back against every data record before it is renamed into place; an incomplete archive refused naming its first missing span, an existing destination refused, the image's byte count and SHA-256 reported | Experimental; the Windows oracle and the CLI are not yet written | Unreleased (#993) | #724 | `rescue_export.rs` |
