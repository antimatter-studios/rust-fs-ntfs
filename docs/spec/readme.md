# NTFS storage and behavior reference

This reference explains how an NTFS volume represents files, names, streams,
allocation, security, and recovery. It combines public Microsoft documentation,
the structures and algorithms in this repository, and retained Windows experiments.
It is intended to be useful to readers implementing a parser or inspecting an image,
as well as people working on this driver.

Start with the [reading guide and contents](ntfs-specification.md). The six format
chapters describe bytes; the seventh follows operations across those structures.
The [coverage map](coverage.md) connects the chapters to source and tests, and the
[open questions](notes/open-questions.md) distinguish missing implementation from
missing knowledge.

## Evidence and scope

The source audit is against revision `ee4f530628f507976ddac261906f389823b99c26`
on 2026-10-11, updated on 2026-10-12 for the fixes merged through
`c4b32996b52c295786deb5081a6171eeb35ca5c7`. Paths refer to this repository. Function names are preferred over
line numbers, which drift after edits. A source link establishes what the code
reads, writes, accepts, or refuses; it does not establish all Windows behavior.

| Label | What it establishes |
| --- | --- |
| **DOCUMENTED** | A linked primary source describes the particular structure or behavior. Its version and API scope still apply. |
| **IMPLEMENTED** | Named source code implements the described behavior. This is a driver contract or restriction, not automatically an NTFS requirement. |
| **OBSERVED** | A retained independent experiment records the behavior, with the fixture or diagnostic named. Conclusions are bounded by that experiment. |
| **OPEN** | Knowledge or compatibility is incomplete. The question and a way to resolve it appear in the question register. |

Evidence is attached to a subsection or table as a whole unless a row states a
narrower basis. A test linked as a **regression check** is something to run; this
audit does not claim to have executed it. A source comment describing a historical
experiment is weaker evidence than retained inputs and outputs, and is identified
as such. Passing chkdsk on a volume does not separately prove every field meaning,
all crash states, or support for every NTFS version.

## Source policy

The prose and explanations here are written independently. External references
are linked rather than mirrored. Microsoft developer notes publish selected NTFS
3.0/3.1 structures; [MS-FSCC] describes filesystem control and information formats,
and [MS-DTYP] describes shared Windows data types. Neither is treated as a complete
specification of the NTFS disk format. See the [reference list](notes/references.md).

The old draft cited a supposed `[MS-NTFS]` document at an unresolved URL, and used
`[MS-FSCC §2.4]` as a blanket citation for unrelated disk internals. Those citations
cannot substantiate claims. This revision replaces them with specific pages or
source evidence and leaves unresolved details explicit.

## How to maintain the reference

1. Identify the bytes or behavior, including the version and addressing unit.
2. Read the implementing function, its callers, and relevant failure paths.
3. Seek a primary reference or a Windows-created fixture independent of our writer.
4. Describe the format separately from our chosen representation and restrictions.
5. Link source and the regression or oracle evidence. Record unresolved cases.
6. Update coverage when implementation or evidence changes; never upgrade an entire
   feature because a single happy-path scenario passed.

Documentation-only changes do not alter volume bytes. The runtime suite still has
prerequisites: a missing fixture fails the gate and is not a passing test. The
baseline attempted for this revision failed at `test-disks/check-fixtures.sh` before
running Rust tests; see the coverage page for the exact missing fixture names.
