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
    foreach ($name in @('deleted-first.txt', 'reused-name.txt', 'deleted-second.txt', 'reused-name.txt', 'live-final.txt')) {
        $path = Join-Path $root $name
        [System.IO.File]::WriteAllText($path, "Windows-authored $name`n")
        $sequence.Add([ordered]@{ operation = 'create'; name = $name; sha256 = (Get-FileHash $path -Algorithm SHA256).Hash.ToLowerInvariant() })
        if ($name -ne 'live-final.txt') {
            Remove-Item -LiteralPath $path
            $sequence.Add([ordered]@{ operation = 'delete'; name = $name })
        }
    }
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
        operations = $sequence
        image = 'ntfs-i30-slack.img'
        image_size_bytes = (Get-Item $image).Length
        image_sha256 = (Get-FileHash $image -Algorithm SHA256).Hash.ToLowerInvariant()
        purpose = 'Raw Windows-authored volume evidence for $I30 create/delete/reuse slack analysis.'
        reader_status = 'not implemented; capture only'
    }
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding utf8
} finally {
    Get-DiskImage -ImagePath $state.Vhd -ErrorAction SilentlyContinue |
        Where-Object Attached | Dismount-DiskImage -ErrorAction SilentlyContinue | Out-Null
    Remove-Item $state.Vhd -Force -ErrorAction SilentlyContinue
}
if ((Get-Item $image).Length -ne 256MB) { throw 'Windows $I30 capture image size is incorrect' }
if (-not (Test-Path $manifestPath)) { throw 'Windows $I30 capture manifest was not written' }
