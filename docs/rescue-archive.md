# The rescue archive

`fs_ntfs::rescue::scan` copies every readable byte of a failing source into
an external archive and records every byte it could not read (#720, part of
#595). This page is the archive's layout, and what a reader must refuse.

The archive is evidence, not a volume. A data record holds bytes the source
returned. An unreadable record names a span the source refused and carries no
payload, so nothing in the archive stands in for a byte the source never gave.
Nothing here is an NTFS structure, and an archive is never a repair plan.

## How a scan fills it

- The source is opened read-only and is never written. The archive is created
  new: an existing file, the source included, is refused.
- The scan reads in bulk chunks (1 MiB by default). When a bulk read fails
  with a media error (`EIO`; on Windows `ERROR_CRC`, `ERROR_SECTOR_NOT_FOUND`,
  `ERROR_READ_FAULT` or `ERROR_IO_DEVICE`), the chunk is read again one
  sector at a time. One bad sector costs that sector, not the chunk.
- Records are written in source order. Adjacent unreadable sectors become one
  unreadable record, across chunk boundaries too.
- Any other read failure stops the scan, with what was archived so far kept:
  recording healthy bytes as lost would be worse than stopping.
- The header is flushed before the first record, and each record is flushed
  before the scan reads past it. An interruption can lose only the record
  being written, which is then cut short at the end of the file.

## Layout

All integers are little-endian. CRC-32 is the one zlib, gzip and PNG compute:
reflected polynomial `0xEDB88320`, initial value and final XOR `0xFFFFFFFF`.

The file is a 32-byte header followed by records, back to back.

### Header

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Magic: the ASCII bytes `NTRESCUE` |
| 8 | 4 | Version: `1` |
| 12 | 4 | Sector size of the scan: a power of two from 512 to 4096 |
| 16 | 8 | Source length in bytes |
| 24 | 4 | Reserved: zero |
| 28 | 4 | CRC-32 of bytes 0 to 27 |

### Record

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Source offset of the span |
| 8 | 8 | Span length in bytes, not zero |
| 16 | 4 | Kind: `1` data, `2` unreadable |
| 20 | 4 | CRC-32 of bytes 0 to 19 of this header, then the payload |
| 24 | length (data only) | The source's bytes over the span |

An unreadable record has no payload. Every span is a whole number of sectors,
except one that ends at the end of a source whose length is not.

## What a reader must refuse

- A magic other than `NTRESCUE`, a version other than `1`, nonzero reserved
  bytes, or a header whose CRC-32 does not match.
- A record of any other kind, or with a zero length.
- A record whose CRC-32 does not match. A complete record with a bad checksum
  is damage, not an interruption.
- Records that do not tile the source in order: each starts where the one
  before it ended, and none runs past the source length. The records of a
  finished scan end exactly at the source length.

A file that ends partway through a record header or payload was interrupted
while that record was being written. Everything before that record stands.
Resuming such an archive is #722; refusing a foreign or changed source on
resume is #723.

## Tests

`tests/rescue_scan.rs` scans the Windows interrupted captures and the copies
Windows recovered, parses the archives by this page rather than through the
crate, checks every CRC-32 with `crc32fast`, rebuilds each image with plain
file writes, and requires the capture's SHA-256. It reads the rebuilt
recovered copies with the independent `ntfs` crate against Windows'
`Get-FileHash` manifests, and injects unreadable sectors to check the spans
recorded.
