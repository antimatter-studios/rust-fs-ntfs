## Checking and recovery

| What a replay leaves in `$LogFile`: the log a clean Windows dismount leaves (LFS 1.1 restart pages flagged clean, a checkpoint with empty tables and its tail copies), naming a current LSN one wrap past the last replayed, so Windows logs past every LSN the replay left on the volume | Supported; Windows' own LSNs after the replay are checked by the matrix scenario `cli-windows-interrupted-fsck-replay-win-write-lsn-after-windows` | Unreleased (#762) | #649 | `replay_leaves_clean_log.rs`, `logfile::tests` |
