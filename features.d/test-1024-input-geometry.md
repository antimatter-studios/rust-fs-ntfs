## Interfaces

| Operation-sequence input chooses its geometry (test support) | Experimental: the first three bytes of a raw operation-sequence input choose a 64-256 MiB volume and its cluster and MFT record sizes from every size `format_filesystem` supports; the explorer formats that geometry and records it in `geometry.txt`, and a capture pins it in the regression | Unreleased | #1024, #733 | `op_sequence_generator.rs::input_bytes_choose_a_volume_geometry_the_formatter_accepts` |
