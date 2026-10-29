# Repair journal file format

A repair journal records how far an in-place repair of a volume got, so
that a repair interrupted part way can be told apart from one that never
started or one that finished (#713, #918). It is written and read by the
`repair_journal` module, behind the `plan-file` feature, beside the plan
file it is for (`docs/repair-plan-file.md`).

The journal is external storage: it is never on the volume being
repaired, and it is not a `$LogFile` transaction. It is append-only: each
record is added to the end and nothing before it is ever rewritten.

## Records

A journal is a sequence of records and nothing else. All integers are
little-endian. Every record has the same frame:

| size | field |
|---|---|
| 4 | record type: `1` header, `2` intent, `3` applied |
| 4 | payload length `n` |
| `n` | payload |
| 32 | SHA-256 of the type, the length and the payload |

`repair_journal::header_record`, `intent_record` and `applied_record`
return the bytes of one whole record each.

### Header, once, first

Binds the journal to one plan file and one source.

| size | field |
|---|---|
| 8 | magic, the ASCII bytes `NTFSJRNL` |
| 4 | journal format version, `1` |
| 32 | SHA-256 of the whole plan file |
| 8 | source size in bytes, as the plan file records it |
| 8 | volume serial number, as the plan file records it |

`repair_journal::JournalBinding::of` takes these from a plan file, which
must load.

### Intent, one per unit, before the unit is written

| size | field |
|---|---|
| 4 | unit index in the plan |
| 4 | write count |
| per write: 8 | byte offset on the volume |
| per write: 4 | length `m` |
| per write: `m` | preimage: the plan's before-image for this write |
| per write: `m` | redo: the plan's after-image for this write |

### Applied, one per unit, once the unit is written and flushed

| size | field |
|---|---|
| 4 | unit index in the plan |

## Order

The records come in exactly this order: the header, then for unit 0 its
intent and its applied record, then the same for unit 1, and so on. A
journal may end after any whole record: after the header (nothing
started), after an intent (that unit may be partly written), or after an
applied record (every unit up to it is written and flushed).

## Reading a journal back

`repair_journal::read_journal` reads a journal against the binding of the
plan file it must be for, and returns its whole records in order.

### The one condition a reader tolerates

**A final record cut short**: the bytes after the last whole record are
fewer than a record's 8-byte frame, or fewer than the frame, payload and
checksum the record's own length field calls for. This is what a torn
append leaves. That record is reported as torn (`Journal::torn`, naming
its index and its first byte) and ignored: its contents are never
returned and never applied. The whole records before it are returned.

A cut is recognised only by the length field, so a final record whose
length field is itself damaged to run past the end of the file also reads
as torn. Whatever is behind it is ignored, not applied.

### What a reader must refuse

A reader refuses the whole journal, returning nothing, and names the
record (`journal record N: ...`) when:

- a whole record's checksum is not the SHA-256 of its type, length and
  payload (`checksum does not match its contents`). This includes the
  final record when it is whole: only a record cut short is tolerated;
- the first record is not a header, or a header's magic is not
  `NTFSJRNL` (`not a repair journal header`);
- the header's format version is not one it knows, including any newer
  version (`journal format version N is not supported`);
- the header names a plan file other than the one given, by its SHA-256
  (`the header names another plan file`);
- the header names a source other than the plan's, by size or volume
  serial number (`the header names another source`);
- a header appears after the first record (`a second header`);
- a record type is not one it knows (`unknown record type N`);
- a payload is shorter or longer than its type's fields
  (`malformed: ...`);
- an applied record has no intent before it (`applied record for unit U
  with no intent before it`), or names another unit than the open intent
  (`applied record for unit U, the open intent is unit V`);
- an intent names a unit that is not the next one, `0` first
  (`intent for unit U, the next unit is V`), or comes while the unit
  before it has no applied record (`intent for unit U while unit V has no
  applied record`).

The checksum is checked before anything inside the record, so any change
to a whole record is named as a checksum mismatch.

## Tests

`a_repair_journal_reads_back_to_its_last_whole_record_and_refuses_the_rest`
in `tests/windows_interrupted_fixture.rs` saves a plan from each Windows
interrupted capture, writes a journal for it (the header, then an intent
and an applied record for every unit), and checks that it reads back
record for record, each intent's preimage and redo equal to that unit's
before- and after-images in the loaded plan. It cuts the journal at every
record boundary, and checks the whole records before the cut come back
with no torn tail; one byte past each boundary, the same records come
back and the torn tail is named. It flips a byte in each record except
the last, writes an applied record with no intent and an intent that
skips a unit, and reads the journal against another capture's plan, and
checks that each is refused naming the record.
