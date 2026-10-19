# Repair plan file format

A repair plan (`repair_plan::RepairPlan`, #709) can be saved to a file of its
own with `repair_plan::save_plan` and read back with `repair_plan::load_plan`
(#905). The file is kept outside the volume it repairs: a plan is never
written onto its source, and it is not a `$LogFile` transaction.

The file binds the plan to the source it was made from (its size and the
volume serial number from its boot sector) and to the bytes each unit
expects to find there (the SHA-256 of each unit's before-image). A SHA-256
over the whole file guards every byte of it.

Both functions are behind the `plan-file` feature, which `cli` turns on.

## Layout

All integers are little-endian.

| offset | size | field |
|---|---|---|
| 0 | 8 | magic, the ASCII bytes `NTFSPLAN` |
| 8 | 4 | format version, `1` |
| 12 | 4 | unit count |
| 16 | 8 | file length in bytes, including the trailer |
| 24 | 8 | source size in bytes |
| 32 | 8 | volume serial number: the source's boot sector bytes `0x48..0x50` |
| 40 | ... | the units, in the order they are applied |
| length - 32 | 32 | SHA-256 of every byte before it |

Each unit, one per durable boundary of the repair (the writes between two
flushes):

| size | field |
|---|---|
| 4 | write count |
| 32 | SHA-256 of the unit's before-image: every write's before-image, concatenated in order |
| per write: 8 | byte offset on the volume |
| per write: 4 | length `n` |
| per write: `n` | before-image: what the volume holds there at this write's turn |
| per write: `n` | after-image: what the write leaves there |

## What a reader must refuse

A reader refuses the whole file, and applies none of it, when:

- it does not start with the magic (`not a repair plan file`);
- the version is not one it knows, including any newer version
  (`plan file format version N is not supported`);
- it is shorter than its file-length field says, or than a header
  (`plan file truncated`);
- it is longer than its file-length field says (`plan file has trailing
  bytes`);
- the trailer is not the SHA-256 of the bytes before it (`plan file checksum
  does not match its contents`);
- a unit, write or image runs past the trailer, or bytes sit between the
  last unit and the trailer (`plan file malformed`);
- a unit's before-image does not hash to the SHA-256 it carries (`plan file
  unit N: before-image does not match its hash`).

The version and length are checked before the checksum so that a newer or
truncated file is named as such; any other change is caught by the
checksum. Loading a file proves only that the file is intact: whoever
applies it must still check that the target's size and volume serial
number match, and that each unit's before-image is what the target holds,
before writing anything.

## Checking a plan against its source

`repair_plan::check_plan_source` (#908) is that check. It only reads the
source, and refuses it, naming what differs, when:

- its size in bytes is not the plan's source size (`source size is N
  bytes, the plan was made from M`);
- its volume serial number, boot-sector bytes `0x48..0x50`, is not the
  plan's (`source volume serial is ..., the plan was made from ...`);
- a write's before-image is not what the source holds at that write's
  turn (`plan unit U write W: the source does not hold its before-image at
  byte N`). A write's turn is the source's own bytes with the after-images
  of every earlier write of the plan over them, so a write to bytes an
  earlier write already changed is checked against what that write left.

The first mismatch, in plan order, is the one named. Size and serial
number are checked before any before-image.

## Checking a unit on the device about to receive it

`repair_plan::check_unit_on_device` (#913) checks one unit immediately
before it is written. It only reads the device, as it stands at that
unit's turn: the plan's earlier units are already written there, so only
the unit's own earlier writes are laid over what the device holds. It
refuses the unit when:

- a write's range does not hold its before-image (`plan unit U write W:
  the device does not hold its before-image in bytes START..END`), naming
  the write's whole byte range;
- the plan has no unit at that index (`the plan has N units, no unit U`).

The first mismatch, in the unit's write order, is the one named.

## Applying a plan by copy-repair

`repair_plan::copy_repair` opens the source read-only and checks it against
the plan first. Only once the check passes does it create the target, with
`create_new`, so a path that already exists is refused and left as it was;
it copies the source into the target, flushes the copy, and applies the
plan's units there in order with `apply_plan_units`. The source is never
opened for writing, and a refused check leaves no target behind.

`apply_plan_units` writes each unit's writes in order and then flushes the
device: a flush at every unit boundary and none inside a unit, so the
device holds whole units whenever a flush has returned. Before the first
unit, `check_device_identity` checks the device being written, not only the
source: its size in bytes and the volume serial number at boot-sector bytes
`0x48..0x50` must be the plan's. A short copy, or another device behind the
path, is refused naming the size or serial, with nothing of the plan
written. Immediately before
a unit is written, it is checked on the device with `check_unit_on_device`.
A unit whose ranges do not hold their before-images stops the apply before
it: every earlier unit stays written and flushed, nothing of the refused
unit is written, and the refusal names the unit, the write and its byte
range.

## Tests

`a_saved_plan_loads_back_to_what_windows_recovered_and_only_when_intact` in
`tests/windows_interrupted_fixture.rs` saves a plan from each Windows
interrupted capture, loads it back, applies it by its own writes and
compares the result with Windows' own restart. It computes the section
boundaries from this layout, independently of the reader, truncates the
file at each one and one byte past it, flips a byte in every section, and
writes a version 2 header, and checks that each is refused.

`a_loaded_plan_is_checked_against_its_source_and_refused_on_the_first_mismatch`
checks a plan from each Windows capture against that capture, then against
copies with a byte changed under the first write and under the last write
no earlier write covers, one sector shorter, and with another volume serial
number, and checks that each is refused, naming what differs, and that no
copy is written.

`a_plan_unit_is_checked_on_the_device_about_to_receive_it_at_its_turn`
applies a plan from each Windows capture to a copy unit by unit, checking
each unit first, and checks that every unit checks clean at its turn
without the copy changing. It then changes one byte, which no earlier
write of the same unit covers, inside the first, a middle and the last
unit's ranges after the earlier units are written, and checks that each is
refused naming that write's byte range and that the copy is not written.

`a_copy_repair_reaches_what_windows_recovered_and_never_writes_its_source`
copy-repairs each Windows capture into a new target and checks that the
capture keeps its SHA-256 and the target reaches the files Windows'
`.recovered.manifest` lists and the metadata of Windows' `.recovered`
image. It then copy-repairs onto the target that now exists, and from a
copy of the capture with one before-image byte changed, and checks that
each is refused, that the existing target keeps its SHA-256 and that the
refused check leaves no target.

`a_copy_repair_syncs_after_every_unit_and_never_inside_one` applies a plan
from each Windows capture to a counting target and checks that every
unit's writes arrive in order with one flush after them and none among
them.

`applying_a_plan_rechecks_each_unit_on_the_target_and_stops_with_earlier_units_intact`
applies a plan from each Windows capture to a target that changes one
byte, which no earlier write of the same unit covers, inside the first, a
middle and the last unit's ranges after the previous unit's flush. It
checks that each apply is refused naming that unit and range, that the
target holds exactly the earlier units' writes and the changed byte, that
the capture keeps its SHA-256, and that a plan made again from the target
as it now stands applies without refusal.

`applying_a_plan_refuses_a_target_of_another_size_or_serial_before_the_first_unit`
applies a plan from each Windows capture to a copy one sector short and to
a copy whose boot-sector volume serial changed after the copy. It checks
that each apply is refused naming the size or the serial, that no write or
flush reaches the target, and that the capture keeps its SHA-256.
