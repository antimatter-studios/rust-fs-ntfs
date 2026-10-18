## Interfaces

| Operation-sequence refusals by reason (test support) | Experimental: the explorer writes `refusal-reasons.tsv`, every refused operation counted under its operation and error kind (a refused geometry as `format`), and `seed.txt` naming the explorer seed beside each finding's `geometry.txt` | Unreleased | #1031, #734 | `op_sequence_readback.rs::the_explorer_counts_refusals_by_operation_and_reason`, `op_sequence_readback.rs::the_explorer_counts_a_refused_geometry_with_its_reason`, `tests/scripts/op-sequence-explorer.sh` |
