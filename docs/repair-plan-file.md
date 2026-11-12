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
- its file-length field is shorter than a header and a trailer, a unit,
  write or image runs past the trailer, or bytes sit between the last unit
  and the trailer (`plan file malformed`);
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
the plan first. Only once the check passes does it refuse a target path that
exists, leaving what is there as it was, and copy the source to a new hidden
file beside the target, `.ntfs-copy-repair-<pid>-<n>.partial`. It flushes the
copy, moves it to the target path, flushes the directory, and applies the
plan's units there in order with `apply_plan_units`. The source is never
opened for writing, and a refused check leaves no target behind.

The move refuses a target that appeared during the copy, and removes the copy.
It claims the path by creating an empty file there with `create_new`, then
renames the copy over that file, so it needs no hard links, which exFAT and FAT
do not have. A process killed between those two steps leaves that empty file at
the target path and the hidden file beside it.

On Unix the hidden file, and so the target, is created with mode 0600 whatever
the source's mode, and the process's umask can only narrow that. The target is
then never open to more users than the source, including when its group differs
from the source's. The same holds for a source that is a device node, whose
mode describes who may use the device and not who may read a copy of it. An
owner who wants to share the target changes its mode afterwards. On Windows the
target takes the access its directory gives new files.

`apply_plan_units` writes each unit's writes in order and then flushes the
device: a flush at every unit boundary and none inside a unit, so the
device holds whole units whenever a flush has returned.

A copy that fails removes its hidden file and leaves nothing at the target
path, so the copy-repair can be run again. A process killed during the copy
leaves only the hidden file, which a later copy-repair does not reuse and which
can be removed.

A failure after the move undoes nothing, and the target stays at its path. A
write or flush that fails stops the apply: every earlier unit is whole and
flushed, the failing unit's earlier writes stay with no flush after them, and
no later unit is written. The target is then not a repaired volume, and a
copy-repair onto that path is refused until it is removed.

## Tests

`tests/repair_plan_file.rs` needs no fixture. It builds files by hand from
this layout, requires `save_plan` to write those bytes, and refuses every
prefix of a file, every changed byte, and, with the length and checksum made
right, each structural condition above, checking that each names its reason.

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

`tests/repair_plan_source.rs` also checks partial overlaps within and across
units, with the latest earlier write taking precedence. It checks that size
and serial refusals stop before reading any before-image, that the first
before-image mismatch names its unit, write and first differing byte, and
that read failures are returned. Its source panics on any write or flush
attempt, including on a successful check.

`a_plan_unit_is_checked_on_the_device_about_to_receive_it_at_its_turn`
applies a plan from each Windows capture to a copy unit by unit, checking
each unit first, and checks that every unit checks clean at its turn
without the copy changing. It then changes one byte, which no earlier
write of the same unit covers, inside the first, a middle and the last
unit's ranges after the earlier units are written, and checks that each is
refused naming that write's byte range and that the copy is not written.

`tests/repair_plan_unit.rs` also checks partial overlaps within a unit,
with the latest earlier write taking precedence, and proves earlier units
are read from the device rather than overlaid from the plan. It checks
first, middle and last unit refusals, the exact reported range, invalid
unit indices, and stopping at the first mismatch or read failure. Its
device panics on any write or flush attempt, including on a successful
check.

`tests/repair_plan_copy.rs` needs no fixture. It fails one write and one
flush of a plan's middle unit and checks what the target then holds and
which writes and flushes reached it, and copy-repairs a source whose file is
read-only and checks that the source is unchanged and the target holds every
unit. On Unix it runs `fsck.ntfs --apply` under a file-size limit smaller than
the source, and checks that the copy fails with no name left at the target or
beside it, that the source is unchanged and that a retry succeeds. It also
copy-repairs sources with modes 0600 and 0644 under umask 022 and checks that
each target has mode 0600.
The full pull-request CI runs this file; `chore test:repair-plan-copy` uses the
same command.

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
