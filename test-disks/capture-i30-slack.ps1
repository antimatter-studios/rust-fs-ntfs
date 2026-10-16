# Captures one Windows-authored NTFS directory index after create/delete/reuse.
# The image is evidence for the future $I30 slack reader; this script does not
# parse or claim to validate index slack.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"

$image = [System.IO.Path]::GetFullPath('test-disks/ntfs-i30-slack.img')
$manifestPath = [System.IO.Path]::GetFullPath('test-disks/ntfs-i30-slack.manifest.json')
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
    # Enough long control names that the index cannot stay resident in
    # $INDEX_ROOT; Windows must allocate INDX blocks.
    $controls = 0..63 | ForEach-Object { 'control-{0:D3}-{1}.txt' -f $_, ('x' * 40) }
    foreach ($name in $controls) {
        $path = Join-Path $root $name
        [System.IO.File]::WriteAllText($path, "control $name`n")
        $sequence.Add([ordered]@{ operation = 'create'; name = $name; role = 'control-live'; file_id = (Get-NativeFileId $path) })
    }
    foreach ($name in @('deleted-first.txt', 'reused-name.txt', 'deleted-second.txt', 'reused-name.txt', 'live-final.txt')) {
        $path = Join-Path $root $name
        [System.IO.File]::WriteAllText($path, "Windows-authored $name`n")
        $role = if ($name -eq 'live-final.txt') { 'live' } else { 'deleted' }
        $sequence.Add([ordered]@{ operation = 'create'; name = $name; role = $role; file_id = (Get-NativeFileId $path); sha256 = (Get-FileHash $path -Algorithm SHA256).Hash.ToLowerInvariant() })
        if ($name -ne 'live-final.txt') {
            Remove-Item -LiteralPath $path
            $sequence.Add([ordered]@{ operation = 'delete'; name = $name })
        }
    }
    $directoryId = Get-NativeFileId $root
    $layout = (& fsutil file layout $root) -join "`n"
    if ($layout -notmatch 'INDEX_ALLOCATION') { throw "Windows did not allocate INDX blocks for ${root}:`n$layout" }
    Sync-VhdToImg -Vhd $state.Vhd -ImagePath $image
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
        image_sha256 = (Get-FileHash $image -Algorithm SHA256).Hash.ToLowerInvariant()
        purpose = 'Raw Windows-authored volume evidence for $I30 create/delete/reuse slack analysis.'
        reader_status = 'compared by index_slack::windows_capture under the windows-i30-oracle feature'
    }
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding utf8
} finally {
    Get-DiskImage -ImagePath $state.Vhd -ErrorAction SilentlyContinue |
        Where-Object Attached | Dismount-DiskImage -ErrorAction SilentlyContinue | Out-Null
    Remove-Item $state.Vhd -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $diag -Recurse -Force -ErrorAction SilentlyContinue
}
if ((Get-Item $image).Length -ne 256MB) { throw 'Windows $I30 capture image size is incorrect' }
if (-not (Test-Path $manifestPath)) { throw 'Windows $I30 capture manifest was not written' }
