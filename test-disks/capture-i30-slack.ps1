# Captures one Windows-authored NTFS directory index after create/delete/reuse.
# The image is evidence for the future $I30 slack reader; this script does not
# parse or claim to validate index slack.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"
. "$PSScriptRoot\i30-slack-evidence.ps1"

$image = [System.IO.Path]::GetFullPath('test-disks/ntfs-i30-slack.img')
$manifestPath = [System.IO.Path]::GetFullPath('test-disks/ntfs-i30-slack.manifest.json')
$diagnosticPath = [System.IO.Path]::GetFullPath('test-disks/ntfs-i30-slack.diagnostic.txt')
Remove-Item -LiteralPath $manifestPath,$diagnosticPath -Force -ErrorAction SilentlyContinue
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$diag = Join-Path $tempRoot 'ntfs-i30-slack'
New-Item -ItemType Directory -Path (Split-Path $image) -Force | Out-Null
New-Item -ItemType Directory -Path $diag -Force | Out-Null
Remove-Item $image,([System.IO.Path]::ChangeExtension($image, '.vhd')) -Force -ErrorAction SilentlyContinue

$stream = [System.IO.File]::Create($image)
try { $stream.SetLength(256MB) } finally { $stream.Close() }
$state = Initialize-VhdFromImg -ImagePath $image -Diag $diag
try {
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
    Format-Volume -DriveLetter $letter -FileSystem NTFS -AllocationUnitSize 4096 `
        -NewFileSystemLabel 'I30SLACK' -Force -Confirm:$false | Out-Null
    $root = "${letter}:\index-slack"
    New-Item -ItemType Directory -Path $root | Out-Null
    $sequence = [System.Collections.Generic.List[object]]::new()
    # Native file IDs come from Windows (fsutil), never from our reader.
    function Get-NativeFileId([string]$p) {
        $out = (& fsutil file queryFileID $p) -join ' '
        if ($out -notmatch '0x([0-9a-fA-F]+)') { throw "fsutil gave no file ID for ${p}: $out" }
        return [Convert]::ToUInt64($Matches[1].Substring([Math]::Max(0, $Matches[1].Length - 16)), 16)
    }
    function Add-Created([string]$name, [string]$role) {
        $path = Join-Path $root $name
        [System.IO.File]::WriteAllText($path, "Windows-authored $name`n")
        $sequence.Add([ordered]@{ operation = 'create'; name = $name; role = $role; file_id = (Get-NativeFileId $path) })
    }
    function Remove-Created([string]$name) {
        Remove-Item -LiteralPath (Join-Path $root $name)
        $sequence.Add([ordered]@{ operation = 'delete'; name = $name })
    }
    # The to-be-deleted names sort after every control and are created first,
    # so the control inserts that follow split their leaf and leave complete
    # copies of these entries behind in an older block's slack. Whether any
    # copy survives is Windows' choice; it is observed below, never assumed.
    $doomed = @('deleted-first.txt', 'deleted-second.txt', 'reused-name.txt')
    foreach ($name in $doomed) { Add-Created $name 'deleted' }
    # Enough long control names that the index cannot stay resident in
    # $INDEX_ROOT; Windows must allocate INDX blocks.
    $controls = 0..63 | ForEach-Object { 'control-{0:D3}-{1}.txt' -f $_, ('x' * 40) }
    foreach ($name in $controls) { Add-Created $name 'control-live' }
    foreach ($name in $doomed) { Remove-Created $name }
    Add-Created 'reused-name.txt' 'deleted'
    Remove-Created 'reused-name.txt'
    Add-Created 'live-final.txt' 'live'
    $directoryId = Get-NativeFileId $root
    $layout = (& fsutil file layout $root) -join "`n"
    if ($layout -notmatch 'INDEX_ALLOCATION') { throw "Windows did not allocate INDX blocks for ${root}:`n$layout" }
    Sync-VhdToImg -Vhd $state.Vhd -ImagePath $image
    # Independent raw observation of the bytes Windows wrote: every 4096-byte
    # aligned block carrying the INDX magic has its update sequence applied
    # here, then each deleted name's UTF-16LE bytes are searched for. A hit is
    # attested only when its FILE_NAME parent is the capture directory and
    # its entry's file reference is the native ID fsutil gave that name. The
    # hash is taken from the same bytes, so the evidence is bound to them.
    $bytes = [System.IO.File]::ReadAllBytes($image)
    $imageSha = ([System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::HashData($bytes)) -replace '-', '').ToLowerInvariant()
    $dirRecord = $directoryId -band 0xFFFFFFFFFFFF
    $deletedIds = @{}
    foreach ($op in $sequence) { if ($op.operation -eq 'create' -and $op.role -eq 'deleted') { $deletedIds[$op.file_id] = $op.name } }
    $evidence = [System.Collections.Generic.List[object]]::new()
    $indxBlocks = 0; $nameHits = 0
    for ($off = 0; $off -le $bytes.Length - 4096; $off += 4096) {
        if ($bytes[$off] -ne 0x49 -or $bytes[$off+1] -ne 0x4E -or $bytes[$off+2] -ne 0x44 -or $bytes[$off+3] -ne 0x58) { continue }
        $indxBlocks++
        $blk = [byte[]]::new(4096); [Array]::Copy($bytes, $off, $blk, 0, 4096)
        $usaOff = [BitConverter]::ToUInt16($blk, 4); $usaCount = [BitConverter]::ToUInt16($blk, 6)
        for ($i = 1; $i -lt $usaCount -and ($i * 512) -le 4096; $i++) {
            $blk[$i*512-2] = $blk[$usaOff + 2*$i]; $blk[$i*512-1] = $blk[$usaOff + 2*$i + 1]
        }
        foreach ($name in ($deletedIds.Values | Sort-Object -Unique)) {
            $needle = [System.Text.Encoding]::Unicode.GetBytes($name)
            for ($p = 0x52; $p -le 4096 - $needle.Length; $p++) {
                $hit = $true
                for ($k = 0; $k -lt $needle.Length; $k++) { if ($blk[$p+$k] -ne $needle[$k]) { $hit = $false; break } }
                if (-not $hit -or $blk[$p-2] -ne $name.Length) { continue }
                $nameHits++
                $entry = $p - 0x52
                $ref = [BitConverter]::ToUInt64($blk, $entry) -band 0xFFFFFFFFFFFF
                $parent = [BitConverter]::ToUInt64($blk, $entry + 0x10) -band 0xFFFFFFFFFFFF
                $native = $deletedIds.Keys | Where-Object { ($_ -band 0xFFFFFFFFFFFF) -eq $ref -and $deletedIds[$_] -eq $name }
                if ($parent -eq $dirRecord -and $native) {
                    $evidence.Add([ordered]@{ name = $name; file_id = [uint64]@($native)[0]; record_number = $ref; block_image_offset = $off; vcn = [BitConverter]::ToUInt64($blk, 0x10); entry_offset_in_block = $entry })
                }
            }
        }
    }
    Write-Host "i30 raw scan: indx_blocks=$indxBlocks deleted_name_hits=$nameHits attested=$($evidence.Count)"
    $ntfs = Get-CimInstance Win32_OperatingSystem
    $manifest = [ordered]@{
        schema = 'rust-fs-ntfs-i30-slack-capture-v1'
        authored_by = 'Microsoft Windows NTFS via Format-Volume and Win32 file create/delete'
        captured_at_utc = [DateTime]::UtcNow.ToString('o')
        windows_caption = $ntfs.Caption
        windows_version = $ntfs.Version
        windows_build = $ntfs.BuildNumber
        filesystem = 'NTFS'
        cluster_size_bytes = 4096
        directory = '/index-slack'
        directory_file_id = $directoryId
        directory_index_allocation = 'fsutil file layout reported $INDEX_ALLOCATION'
        never_created_control_name = 'never-created.txt'
        operations = $sequence
        image = 'ntfs-i30-slack.img'
        image_size_bytes = (Get-Item $image).Length
        image_sha256 = $imageSha
        raw_deleted_evidence = $evidence
        raw_scan = [ordered]@{ indx_blocks = $indxBlocks; deleted_name_hits = $nameHits }
        purpose = 'Raw Windows-authored volume evidence for $I30 create/delete/reuse slack analysis.'
        reader_status = 'compared by index_slack::windows_capture under the windows-i30-oracle feature'
    }
    # The manifest and diagnostic are written before the precondition is
    # judged, so a failing capture keeps them for the always-run upload.
    Write-I30SlackEvidence -Manifest $manifest -ManifestPath $manifestPath -DiagnosticPath $diagnosticPath
} finally {
    Get-DiskImage -ImagePath $state.Vhd -ErrorAction SilentlyContinue |
        Where-Object Attached | Dismount-DiskImage -ErrorAction SilentlyContinue | Out-Null
    Remove-Item $state.Vhd -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $diag -Recurse -Force -ErrorAction SilentlyContinue
}
if ((Get-Item $image).Length -ne 256MB) { throw 'Windows $I30 capture image size is incorrect' }
if (-not (Test-Path $manifestPath)) { throw 'Windows $I30 capture manifest was not written' }
