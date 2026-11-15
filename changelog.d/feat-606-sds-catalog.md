### Added

- `sds::catalog` walks a `$Secure:$SDS` stream copy by copy, allocating
  nothing. Each copy is reported with:
  - its physical and logical offset, security id and unpadded size;
  - its stored and computed hash, and whether it is a primary or a mirror.

  The walk follows the 256 KiB primary and mirror regions. After a region's
  last entry the rest must be zero padding. A truncated entry, a size below
  the header, an entry crossing its region and bytes that are not padding
  are each a located error that ends the walk. `sds::find_descriptor`
  returns a verified primary, else its verified mirror, and refuses when
  neither verifies; `sds::mirror_state` compares a primary with its
  mirror (#606).

### Fixed

- `fs.ntfs acl show` looks a file's security id up through the checked
  catalog. It used to take the first entry carrying the id, whether or not
  that copy's hash and offset held, so a torn primary was reported instead
  of its intact mirror (#606).
