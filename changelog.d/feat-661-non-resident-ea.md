### Added

- **An EA set too large for a resident `$EA` is stored non-resident, as
  Windows stores it (#661, part of #480).** A 2000-byte EA on 1 KiB records
  was refused, although a file's EAs may total 64 KiB. `$EA` now goes into
  clusters when no record holds it resident, through the file's
  `$ATTRIBUTE_LIST` where it has one, and stays there as the set is
  rewritten; removing every EA gives the clusters back. A set over 64 KiB
  is still refused, with nothing written.
