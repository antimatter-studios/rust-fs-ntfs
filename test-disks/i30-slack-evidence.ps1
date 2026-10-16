# Writes the $I30 capture manifest and a small raw-scan diagnostic, then judges
# the i30-raw-deleted-candidate precondition. Both files are written before the
# judgement, so a capture that fails the precondition still leaves its
# evidence for the always-run diagnostic upload. The disk image is never part
# of the diagnostic: it is bound to it by image_sha256 alone.
function Write-I30SlackEvidence {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary]$Manifest,
        [Parameter(Mandatory)][string]$ManifestPath,
        [Parameter(Mandatory)][string]$DiagnosticPath
    )
    $Manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $ManifestPath -Encoding utf8
    $evidence = @($Manifest.raw_deleted_evidence)
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add("schema=$($Manifest.schema)")
    $lines.Add("image_sha256=$($Manifest.image_sha256)")
    $lines.Add("directory_file_id=$($Manifest.directory_file_id)")
    $lines.Add("indx_blocks=$($Manifest.raw_scan.indx_blocks)")
    $lines.Add("deleted_name_hits=$($Manifest.raw_scan.deleted_name_hits)")
    $lines.Add("attested=$($evidence.Count)")
    $i = 0
    foreach ($op in $Manifest.operations) {
        $lines.Add("operation[$i]=$($op.operation) name=$($op.name) file_id=$($op.file_id)")
        $i++
    }
    foreach ($e in $evidence) {
        $lines.Add("evidence=$($e.name) record=$($e.record_number) block_image_offset=$($e.block_image_offset) entry_offset_in_block=$($e.entry_offset_in_block)")
    }
    Set-Content -LiteralPath $DiagnosticPath -Value $lines -Encoding utf8
    if ($evidence.Count -eq 0) {
        $dirRecord = [uint64]$Manifest.directory_file_id -band 0xFFFFFFFFFFFF
        throw "PRECONDITION i30-raw-deleted-candidate: Windows left no deleted entry in allocated INDX slack whose name, parent ($dirRecord) and native file ID all match (indx_blocks=$($Manifest.raw_scan.indx_blocks) deleted_name_hits=$($Manifest.raw_scan.deleted_name_hits)); see $ManifestPath and $DiagnosticPath"
    }
}
