### Fixed

- **A freshly formatted volume's `$Secure` passes Slate's checker (#837).**
  `mkfs` now allocates `$Secure:$SDS` as one contiguous run from the primary
  block through the mirror, with no attribute flags, and no longer marks
  `$Secure` sparse, matching what Windows `format` writes. The sparse layout
  it used before was accepted by Windows `chkdsk` but made Slate refuse the
  whole security-descriptor inventory with `InvalidSecurity`.
