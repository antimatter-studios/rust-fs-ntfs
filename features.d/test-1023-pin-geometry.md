## Interfaces

| Operation-sequence pins replay on their geometry (test support) | Experimental: a captured regression pin replays on the `format_filesystem` geometry it records instead of being required to match the default 64 MiB, 4096-byte-cluster, 1024-byte-record image, and the corpus carries a pin on 512-byte clusters and 4096-byte MFT records | Unreleased | #1023, #733, #685 | `op_sequence_readback.rs::committed_corpus_replays_every_seed` |
