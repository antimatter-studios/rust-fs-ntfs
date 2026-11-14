### Fixed

- **A write to a file with an `$ATTRIBUTE_LIST` moves its quota charge
  (#1092, part of #853).** The recharge after a write left any file with
  extension records at its old charge, so a stream whose first extent sits
  in an extension record could grow or shrink without its owner's
  `$Extend\$Quota:$Q` usage following. The recharge now reads each
  stream's first extent through the list, wherever it sits. Extension
  records themselves are not charged until what Windows charges for them
  is measured (#1093).
