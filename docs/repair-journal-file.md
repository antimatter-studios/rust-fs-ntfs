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

## Writing a journal: an in-place repair

`repair_journal::apply_in_place` applies a plan to the device it was made
from through an empty journal outside that device. The header is written,
flushed and read back first. Then, for each unit in order:

1. The unit's ranges are checked on the device (`check_unit_on_device`);
   a mismatch stops the apply with nothing of the unit written.
2. The unit's intent record is appended, the journal flushed, and the
   record read back and compared with what was written.
3. The unit's writes go to the device, and the device is flushed.
4. Every write's range is read back from the device and compared with
   what the unit leaves there.
5. Only then is the unit's applied record appended and the journal
   flushed.

A unit whose device read-back differs stops the apply, naming the unit,
the write and its byte range; its intent is in the journal and its applied
record is not. Nothing reaches the device outside the plan's ranges, and
the plan is never written into the volume's `$LogFile` as a transaction.

## Where an apply stopped

`repair_journal::progress` reads a journal `read_journal` has accepted
against its plan and says where the apply stopped. It reads no device. A
journal that stops

- **inside its header** holds no whole record: nothing is applied and
  nothing is open, and a resume starts the journal again from its header;
- **after its header, or after an applied record**, has every unit up to
  and including the last applied record marked applied, and nothing open;
- **after an intent record** has the units before that intent marked
  applied, and the intent's unit open: its writes may have reached the
  device whole, in part, or not at all.

A torn final record changes none of this: it is ignored. Every intent must
carry exactly its plan unit's writes, the same count and each with the
same offset, preimage and redo; an intent that does not, or names a unit
the plan does not hold, is refused naming the unit and write, even though
its checksum is whole.

## Resuming a stopped apply

`repair_journal::resume_in_place` continues an in-place apply that
stopped, on the same device and the same journal. It reads the journal
back with `read_journal` and `progress`, drops a torn final record and
appends the next record where the torn one starts. That next record is
always the one that was torn, the same bytes, so nothing of the torn tail
is left after it. Then:

1. Every unit the journal marks applied is read back from the device:
   each write's range must hold its unit's redo with the later applied
   units' writes laid over it. Those units are skipped, never written
   again. Bytes the open unit also writes are left to its own check.
2. The open unit is redone when every write's range holds its whole
   preimage or its whole redo: its writes reach the device, the device is
   flushed and read back, and its applied record is appended and flushed,
   as the apply's steps 3 to 5 do. Its intent is not appended again.
3. Every later unit is applied exactly as the apply applies it.

A journal holding no whole record is started again from its header. A
range of the open unit holding anything else stops the resume, naming the
unit, the write and its byte range, with nothing of it written again; an
applied unit that does not read back stops it the same way.

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

`an_in_place_repair_reaches_what_windows_recovered_and_equals_the_copy_repair`
applies each capture's plan in place, with the journal in another
directory, and checks the device is byte for byte the target copy-repair
makes from the same plan, and reaches Windows' manifest and recovered
metadata. `an_in_place_repair_makes_each_unit_durable_in_the_journal_before_writing_it`
records every write, flush and read on a device and journal pair and
checks they arrive in exactly the order above, unit by unit.
`an_in_place_repair_stops_at_a_unit_the_device_reads_back_wrong_and_never_marks_it_applied`
drops one device write and checks the apply stops naming that unit and
range, with no applied record for it in the journal.
`a_repair_journal_reports_the_units_applied_and_the_unit_left_open_at_every_cut`
cuts each capture's journal at every record boundary, one byte either
side of it and the middle of each record, and checks `progress` against
the answer the plan's own record lengths give; and replaces unit 0's
intent with one whose redo differs by a byte, its checksum recomputed,
and checks it is refused naming unit 0 write 0.
`an_in_place_repair_stopped_after_any_write_resumes_to_the_uninterrupted_volume`
cuts each capture's in-place apply after every device write and every
journal append, resumes it on the same device and journal, and checks the
device is byte for byte where an uninterrupted apply leaves it and the
journal holds exactly one intent and one applied record per unit, in
order; the resumed device must reach Windows' manifest and recovered
metadata. `a_resumed_in_place_repair_writes_nothing_to_the_device_for_a_unit_marked_applied`
cuts the apply right after each unit's applied record and checks the
resume writes exactly the later units' writes to the device, and that
resuming a finished apply writes nothing to the device or the journal.
