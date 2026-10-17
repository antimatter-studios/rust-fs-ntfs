## Writing

| An EA set too large for a resident `$EA`: `$EA` is stored non-resident, in clusters, as Windows stores it, through the file's `$ATTRIBUTE_LIST` where it has one; the set is rewritten in clusters as it grows and shrinks, and removing every EA gives the clusters back | Experimental: a regular file's EAs, up to 64 KiB | Unreleased (#661) | #480 | `non_resident_ea.rs`; Windows matrix `mac-format-mac-write-big-eas-win-chkdsk`, `mac-format-mac-write-big-eas-remove-all-win-chkdsk`, `win-format-win-write-streams-mac-write-big-eas-win-chkdsk` |
