### Added

- **`usn::decode_usn_record_v2` decodes one change-journal record from
  Microsoft's documented `USN_RECORD_V2` layout (#609).** It keeps the full
  64-bit file and parent references, all 32 reason bits and the UTF-16 name,
  reads nothing past `RecordLength` or the buffer, and refuses a version
  other than 2.0, a truncated buffer, a record shorter than its header and a
  name outside the record. `usn::decode_usn_records_v2` decodes consecutive
  records up to a caller's bound and returns where it stopped. The Windows CI
  job captures real records with `FSCTL_READ_USN_JOURNAL` and checks the
  decoded file reference against `fsutil file queryfileid`.
