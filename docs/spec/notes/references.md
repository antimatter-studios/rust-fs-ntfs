# References and evidence sources

## Microsoft primary documentation

Use the direct page for the claimed structure. These sources publish selected
interfaces and layouts, not a complete version-independent NTFS disk specification.

- [Master File Table developer notes](https://learn.microsoft.com/en-us/windows/win32/devnotes/master-file-table): record/attribute model.
- [FILE_RECORD_SEGMENT_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/file-record-segment-header): selected NTFS 3.0/3.1 header fields; many fields explicitly reserved.
- [ATTRIBUTE_RECORD_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header): attribute envelope, forms, mapping pairs. Its count-sign/unallocated-LCN wording needs care; see Q07.
- [MULTI_SECTOR_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/multi-sector-header): USA purpose and transfer protection. Its prose uses a 512-byte stride and contains count/size wording requiring reconciliation with real headers.
- [File streams](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams): per-stream sizes/states, stream naming, selected type purposes.
- [Change journals](https://learn.microsoft.com/en-us/windows/win32/fileio/change-journals): consumer notification history.
- [USN_RECORD_V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2): versioned consumer record fields and runtime record sizing.
- [fsutil sparse](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/fsutil-sparse): sparse reads return zeros for unallocated content.
- [fsutil dirty](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/fsutil-dirty): a set dirty bit marks a possibly inconsistent volume, and chkdsk runs on it at the next restart.
- [[MS-FSCC]: File System Control Codes](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/): Windows filesystem protocol/control information. Cite an actual relevant type, not a blanket section number for disk internals.
- [[MS-DTYP]: Windows Data Types](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/): FILETIME, SID, ACL, ACE, and self-relative security descriptor type family.

- [FileFullEaInformation](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/0eb94f48-6aac-41df-a878-79f4dcfd8989): query/set entry envelope and four-byte alignment.
- [REPARSE_DATA_BUFFER](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/content/ntifs/ns-ntifs-_reparse_data_buffer): common, symbolic-link and mount-point forms.
- [REPARSE_GUID_DATA_BUFFER](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_reparse_guid_data_buffer): third-party GUID form.

The specific developer/API pages above were checked during this audit. Specification-family roots
are navigation pointers; a root link alone does not verify a particular disk field.
The earlier `[MS-NTFS]` URL could not be resolved or corroborated in this audit and
is no longer treated as evidence of a published specification.

## Repository source and regressions

The [coverage map](../coverage.md) identifies source functions and relevant checks.
The upstream `ntfs` crate is a test-side independent reader, not the production
reader at this revision. Agreement with it is useful cross-validation, but Windows
is still the oracle for write compatibility.

A source comment may report an older Windows observation whose raw artifacts are
not committed. Such claims remain historical attribution, not equivalent to a
reproducible experiment supplied with this document. Stale module introductions
are not used to override actual function behavior.

## Retained Windows observations

- [Windows index insertion, 2026-10-10](../../testing/windows-index-insertion-2026-10-10/README.md): one independently graded insertion into a Windows-created directory; required-name enumeration and both successful chkdsk scan modes.
- [Full Windows matrix, 2026-10-10](../../testing/windows-full-matrix-2026-10-10/README.md): 72 scenarios / 438 steps, platform and revision bounds, four online-scan fallbacks, output-budget replay, and evidence hashes.
- [Interrupted Windows fixtures](../../../tests/windows_interrupted_fixture.rs): capture/recovered-image provenance, hashes, and exact recovery comparisons. Fixture availability is a test prerequisite, not implied by the source file existing.
- [Native reader fixtures](../../../tests/native_read_fixtures.rs): known compressed content and independent attribute-list/ADS reader comparisons.
- [chkdsk improvement findings](../../chkdsk-improvement-findings.md) and [formatter bug catalog](../../mkfs-bug-catalog.md): historical investigations. Recheck their claims against current functions before treating them as present capabilities.
