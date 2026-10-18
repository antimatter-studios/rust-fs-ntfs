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

## Tests

`a_saved_plan_loads_back_to_what_windows_recovered_and_only_when_intact` in
`tests/windows_interrupted_fixture.rs` saves a plan from each Windows
interrupted capture, loads it back, applies it by its own writes and
compares the result with Windows' own restart. It computes the section
boundaries from this layout, independently of the reader, truncates the
file at each one and one byte past it, flips a byte in every section, and
writes a version 2 header, and checks that each is refused.
