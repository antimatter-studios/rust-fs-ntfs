### Added

- An incomplete rescue export's error manifest names, beside each missing
  span, the MFT records whose bytes the span covers (`records=N,...`), and
  `ExportReport::mft_records` reports them. A span outside the `$MFT`, or
  any span when the `$MFT` record itself is missing, names no owner (#1145).
