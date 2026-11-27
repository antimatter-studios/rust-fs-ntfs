### Fixed

- **A record whose update sequence array is misplaced is refused.** An array lying over the 8-byte record header, or reaching the last word of the first stride, is now refused by name for FILE records, index blocks and log pages. Such an array overlapped the bytes it protects (#1392).
