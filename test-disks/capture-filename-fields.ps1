# Captures what Windows itself leaves in the $FILE_NAME data_size,
# allocated_size and file_attributes fields, for the three probes that
# tests/field_exhaustion_fn.rs keeps ignored (issue #629):
#   * data_size_matches_written_byte_count
#   * allocated_size_is_cluster_multiple_for_nonresident_file
#   * readonly_attribute_reflected_in_file_name
#
# Windows may update those fields lazily, so the expected values are not
# assumed. Each step of the operation sequence below gets its own image,
# built from scratch and holding every step up to and including it, so a
# decoder can read the raw $FILE_NAME bytes after each step independently:
#   1 create        data.bin, big.bin, ro.txt and ctl.txt, all empty
#   2 write-close   200 bytes into data.bin, 6000 into big.bin, 2 into ctl.txt
#   3 reopen        each file opened and closed again without a write
#   4 readonly      FILE_ATTRIBUTE_READONLY set on ro.txt only
#   5 rename        ro.txt renamed to ro2.txt, data.bin to data2.bin
#   6 create-refresh  a sibling created in the same directory, which makes
#                   Windows rewrite the directory's index entries
#
# ctl.txt is the negative control: resident, never readonly, never renamed.
# big.bin is the positive control for allocation: non-resident at 4 KiB
# clusters.
#
# Outputs, in test-disks/:
#   * ntfs-fn-fields-step<N>.img for N = 1..6
#   * ntfs-fn-fields.manifest.tsv: step, operation, image SHA-256, then per
#     file the size, allocation and attributes Windows reports through
#     Get-Item and fsutil, and the Windows build that produced it
#
# Run from the repository root on an elevated Windows host. rust-img-vhd must
# be on PATH, as for build-windows-native-read-fixtures.ps1.

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"
. "$PSScriptRoot\windows-fixture.ps1"

$diagnostics = New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot '..\tmp\fn-fields')

$steps = @('create', 'write-close', 'reopen', 'readonly', 'rename', 'create-refresh')
$build = (Get-CimInstance Win32_OperatingSystem)
$windows = "$($build.Caption) $($build.Version) build $($build.BuildNumber)"
$manifest = Join-Path $PSScriptRoot 'ntfs-fn-fields.manifest.tsv'
"step`toperation`timage_sha256`tpath`tlength`tallocation`tattributes`twindows" |
    Set-Content -Path $manifest -Encoding ascii

function Invoke-Step {
    param([string]$Root, [int]$Upto)

    if ($Upto -ge 1) {
        foreach ($name in 'data.bin', 'big.bin', 'ro.txt', 'ctl.txt') {
            [System.IO.File]::Create((Join-Path $Root $name)).Dispose()
        }
    }
    if ($Upto -ge 2) {
        [System.IO.File]::WriteAllBytes((Join-Path $Root 'data.bin'), [byte[]](0..199))
        [System.IO.File]::WriteAllBytes((Join-Path $Root 'big.bin'), [byte[]]::new(6000))
        [System.IO.File]::WriteAllBytes((Join-Path $Root 'ctl.txt'), [byte[]](104, 105))
    }
    if ($Upto -ge 3) {
        foreach ($name in 'data.bin', 'big.bin', 'ro.txt', 'ctl.txt') {
            [System.IO.File]::Open((Join-Path $Root $name), 'Open', 'ReadWrite').Dispose()
        }
    }
    if ($Upto -ge 4) {
        $ro = Get-Item (Join-Path $Root 'ro.txt')
        $ro.Attributes = $ro.Attributes -bor [System.IO.FileAttributes]::ReadOnly
    }
    if ($Upto -ge 5) {
        Rename-Item (Join-Path $Root 'ro.txt') 'ro2.txt'
        Rename-Item (Join-Path $Root 'data.bin') 'data2.bin'
    }
    if ($Upto -ge 6) {
        [System.IO.File]::Create((Join-Path $Root 'sibling.txt')).Dispose()
    }
}

function Assert-VolumeClean {
    param([string]$Root, [int]$Step)

    # Read-only chkdsk while the volume is still mounted, so a stage image is
    # never recorded from a volume Windows itself reports as damaged.
    if ($Root -notmatch '^([A-Za-z]):\\$') {
        throw "step ${Step}: chkdsk needs a drive letter, got '$Root'"
    }
    $log = Join-Path $diagnostics "chkdsk-step$Step.txt"
    & chkdsk "$($Matches[1]):" 2>&1 | Out-File -FilePath $log -Encoding ascii
    if ($LASTEXITCODE -ne 0) {
        throw "step ${Step}: chkdsk exited $LASTEXITCODE; see $log"
    }
}

function Get-Observations {
    param([string]$Root)

    # Runs while the volume is still mounted; the image hash is added later.
    foreach ($item in Get-ChildItem -LiteralPath $Root -File -Force) {
        $layout = (& fsutil file layout $item.FullName 2>&1 | Out-String)
        $allocation = if ($layout -match 'Allocated Size\s*:\s*(\S+)') { $Matches[1] } else { 'unknown' }
        "$($item.Name)`t$($item.Length)`t$allocation`t$([int]$item.Attributes)"
    }
}

for ($step = 1; $step -le $steps.Count; $step++) {
    $image = Join-Path $PSScriptRoot "ntfs-fn-fields-step$step.img"
    # The populate block runs in a child scope of this loop, so it sees $step
    # and $rows; a list is filled because a child scope cannot reassign them.
    $rows = [System.Collections.Generic.List[string]]::new()
    New-WindowsFixture -ImagePath $image -Label "FNFIELDS$step" -Populate {
        param($root)
        Invoke-Step -Root $root -Upto $step
        Assert-VolumeClean -Root $root -Step $step
        foreach ($row in Get-Observations -Root $root) { $rows.Add($row) }
    }
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $image).Hash.ToLowerInvariant()
    Add-Content -Path (Join-Path $diagnostics "chkdsk-step$step.txt") -Encoding ascii `
        -Value "image_sha256`t$hash`noperation`t$($steps[$step - 1])"
    Add-Content -Path $manifest -Encoding ascii -Value "$step`t$($steps[$step - 1])`t$hash`t(image)`t`t`t`t$windows"
    foreach ($row in $rows) {
        Add-Content -Path $manifest -Encoding ascii -Value "$step`t$($steps[$step - 1])`t$hash`t$row`t$windows"
    }
}
