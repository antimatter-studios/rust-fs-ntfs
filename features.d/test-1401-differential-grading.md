## Interfaces

| Operation-sequence geometry gate (test support) | Pinned operation sequences replay on 512, 1024, 2048, 4096 and 65536 byte clusters at 1024 and 4096 byte records, each as a fixed CI case compared with the independent model, the `ntfs` parser and `check_io`; the seeds at 1024, 2048 and 4096 byte clusters also cover directory EAs, a case-only non-ASCII rename and a 255-unit name on 64 to 256 MiB volumes; cross-parent rename is not modeled | Unreleased | #1401 | `tests/op_sequence_readback.rs` (`every_cluster_and_record_pair_has_a_committed_seed`) |
