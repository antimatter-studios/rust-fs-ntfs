### Fixed

- **A truncate to zero of a stream reached through the `$ATTRIBUTE_LIST`
  frees every cluster.** The stream used to keep one cluster allocated for
  no bytes; it now stays non-resident with no runs, as a truncate to zero
  leaves a file without a list.
