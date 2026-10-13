### Added

- **`chore test:cli:host`: the Windows matrix's cli steps that run on this
  machine, without the VM** (#546). Every step a `cli` scenario runs here
  before its first Windows step (`scripts/cli-oracle.sh`) used to run only in
  the Windows job, so a break in our own half first showed ten minutes into
  it. The commands and expected exits are read from
  `fs-windows-test-harness.toml` and `test-matrix.json`, so a new scenario is
  covered unedited; CI's `cli` job and `chore test` run it.
