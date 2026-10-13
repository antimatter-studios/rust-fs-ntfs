### Fixed

- **A replay stopped after any write is safe to run again** (#137). An
  undo applied twice is not undone once: undo stamped a block with the
  undone record's older LSN, so a rerun rolled it back again, and on a
  volume Windows left mid-write a stop from the 70th write on left an index
  block with a name entered twice. Undone blocks now carry one past the
  log's last LSN, which a rerun leaves alone. And emptying the log erased
  its restart pages first: a stop after that left records no replay could
  read, and the volume could no longer be written. The restart pages are
  now marked clean first, as Windows marks them on a clean dismount, then
  the records are erased, and the restart pages last.
