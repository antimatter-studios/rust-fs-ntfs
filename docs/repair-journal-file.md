# Repair journal file format

A repair journal records how far an in-place repair of a volume got, so
that a repair interrupted part way can be told apart from one that never
started or one that finished (#713, #918). Its bytes are encoded and decoded by the
`repair_journal` module, behind the `plan-file` feature, beside the plan
file it is for (`docs/repair-plan-file.md`).

The journal is external storage: it is never on the volume being
repaired, and it is not a `$LogFile` transaction. It is append-only: each
record is added to the end and nothing before it is ever rewritten.

The record encoders and byte-slice reader do not open or append to a file,
check its storage location, flush, or sync. Returning record bytes is not a
crash-durability guarantee.
The caller must write them to external storage, flush any userspace buffer,
sync the file to stable storage (for example, `File::sync_all`), and verify
the intent before writing the corresponding device unit. Creating a new
journal also requires making its directory entry durable. The in-place
writer and its ordering contract are separate work in #919 and #713.

## Records

A journal is a sequence of records and nothing else. All integers are
little-endian. Every record has the same frame:

| size | field |
|---|---|
| 4 | record type: `1` header, `2` intent, `3` applied |
| 4 | payload length `n` |
| 32 | SHA-256 of the type and payload length |
| `n` | payload |
| 32 | SHA-256 of all preceding bytes in this record, including the frame checksum |

`repair_journal::header_record`, `intent_record` and `applied_record`
return the bytes of one whole record each.

Version 2 adds the frame checksum so the reader verifies the length
before using it to identify an incomplete append (#1251). The previous
version 1 framing is not accepted by this reader.

### Header, once, first

Binds the journal to one plan file and one source.

| size | field |
|---|---|
| 8 | magic, the ASCII bytes `NTFSJRNL` |
| 4 | journal format version, `2` |
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

An empty file returns no records and no torn tail. It has not verified
the binding yet: callers must not treat it as a journal with a valid header.

### The one condition a reader tolerates

**A final record cut short**: the bytes after the last whole record are
fewer than a record's 40-byte checksummed frame, or fewer than the frame,
payload and final checksum its verified length calls for. This is what a torn
append leaves. That record is reported as torn (`Journal::torn`, naming
its index and its first byte) and ignored: its contents are never
returned and never applied. The whole records before it are returned.

A complete frame with a damaged type, length or frame checksum is refused,
even if its damaged length would run past the end of the file. A partial
frame cannot yet be verified and is reported as torn. A resume,
which has the plan, then refuses a torn tail that is not the start of the
record it expects: see below.

Before appending again, the caller must discard the incomplete suffix by
truncating the file to `Journal::torn.offset`. Appending behind an existing
torn suffix does not repair it. The encoders do not perform this recovery
or any file I/O; complete records before that offset remain unchanged.

### What a reader must refuse

A reader refuses the whole journal, returning nothing, and names the
record (`journal record N: ...`) when:

- a complete frame's checksum fails, or a whole record's checksum is not
  the SHA-256 of all preceding bytes in that record
  (`checksum does not match its contents`). This includes the
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

The frame checksum is checked before trusting the length; the whole-record
checksum is checked before parsing the payload. A single-byte change to a
complete record is named as a checksum mismatch, not a torn append.

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
is left after it.

Because the plan fixes that next record -- the header when no record is
whole, the open unit's applied record, or else the next unit's intent --
a torn tail must be the start of it, byte for byte (#934). Any other tail
was not left by a torn append: a whole record whose length field was
raised with its frame checksum to match, a torn record with a byte changed,
or any tail after the last unit's applied record. The resume refuses it (`journal record N: cut
short, and not the start of the record the plan expects next`) before
anything is written to the device or the journal. Then:

1. Every unit the journal marks applied is read back from the device:
   each write's range must hold its unit's redo with the later applied
   units' writes laid over it. Those units are skipped, never written
   again. Bytes the open unit also writes are left to its own check.
2. The open unit is redone when every write's range holds its whole
   preimage, its whole redo, or a write torn between the two: its writes
   reach the device, the device is flushed and read back, and its applied
   record is appended and flushed, as the apply's steps 3 to 5 do. Its
   intent is not appended again.

   A write is torn when a power cut stops it part way, and a device may
   land its sectors in any order, so a torn range can hold new bytes at
   its start, its end, or anywhere between. The rule is per byte: a range
   counts as torn when every byte in it equals the same byte of the
   write's preimage or of what the unit leaves there. The redo is then
   written over the whole range and read back, exactly as for a range
   holding the whole preimage.
3. Every later unit is applied exactly as the apply applies it.

A journal holding no whole record is started again from its header. A
range of the open unit holding anything else stops the resume, and so does
an applied unit that does not read back: something changed the device
while the apply was stopped. The refusal names the unit, the write and the
bytes that fail, from the first to the last, not the whole range. It comes
before anything is written to the device or appended to the journal, so
both are left exactly as the cut left them.

## Tests

The nine fixture-free tests in `tests/repair_journal.rs` additionally check
every byte cut, a byte flip at every position in every complete record
(including the final record and all length bytes), a damaged middle length,
empty input, an open intent, append behind a torn tail, recovery by
discarding that tail before reappending, a fixed version 2 byte stream,
and refusal of version 1 framing. The original seven tests are unchanged.
The full pull-request CI runs these tests with `plan-file` enabled;
`chore test:repair-journal` uses the same command.

The fixed byte streams are generated independently of the Rust encoders
and SHA-256 implementation with Python's `struct` and `hashlib`:

```python
import hashlib
import struct

binding = b'\x35' * 32 + struct.pack('<QQ', 4096, 17)
payloads = [
    b'NTFSJRNL' + struct.pack('<I', 2) + binding,
    struct.pack('<IIQI', 0, 1, 512, 16) + b'\x12' * 16 + b'\x34' * 16,
    struct.pack('<I', 0),
]
records = []
for kind, payload in enumerate(payloads, 1):
    prefix = struct.pack('<II', kind, len(payload))
    body = prefix + hashlib.sha256(prefix).digest() + payload
    records.append(body + hashlib.sha256(body).digest())
print(b''.join(records).hex())

payload = b'NTFSJRNL' + struct.pack('<I', 1) + binding
body = struct.pack('<II', 1, len(payload)) + payload
print((body + hashlib.sha256(body).digest()).hex())
```

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
`a_device_write_torn_by_a_cut_resumes_to_the_uninterrupted_volume` cuts
each capture's apply during every device write, landing the first half of
that write's new bytes with the rest unchanged, then only the second
half, and checks the resumed device is byte for byte where an
uninterrupted apply leaves it.
`a_resume_refuses_a_unit_whose_range_changed_while_stopped_and_names_the_byte`
cuts each capture's apply after every device write and changes one byte to
a value that is neither its preimage nor what the journal says belongs
there, first in the open unit's range and then in a unit marked applied.
The resume must refuse naming that unit, write and one-byte range, with
the device's SHA-256 and the journal's length unchanged.
