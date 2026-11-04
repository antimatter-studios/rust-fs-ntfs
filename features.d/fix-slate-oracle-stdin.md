## Interfaces

| Slate oracle case-table input isolation (test support) | Supported: the offline checker and copy-only writer receive an empty standard input instead of the case table, so a program that drains standard input cannot omit later cases; two-row regressions with a floor of one require both output rows and reject the second row's disagreement | Unreleased (#1259) | #1243 | `tests/scripts/slate-oracle.sh` |
