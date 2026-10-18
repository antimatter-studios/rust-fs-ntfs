## Writing

| Quota charges in `$Extend\$Quota:$Q` on a volume tracking quotas | Partial: unlinking a file's last name, removing a directory and renaming over a file take the file's charge off its owner's usage, and rewriting, truncating or extending a file moves its charge by the change in its allocation (#675); creates do not yet charge a file (#676), and enforced limits are not checked (#679) | Unreleased (#686) | #510 | `quota_charge.rs`; Windows matrix `win-format-win-quota-mac-delete-win-chkdsk`, `win-format-win-quota-mac-write-win-chkdsk` |
