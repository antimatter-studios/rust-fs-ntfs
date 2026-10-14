# scripts/fs-windows-test-harness/win-write-sparse.ps1 -- have Windows write
# sparse files, or check sparse files someone else wrote, on a mounted NTFS
# volume.
#
# Each entry of -Files is `/name@bytes@regions`: a file of `bytes` bytes that
# holds data only in `regions` (`OFFSET+LENGTH[,OFFSET+LENGTH...]`, or `none`)
# and zeros everywhere else. The byte at offset i of a region is
# `(i -band 0xFF) -bor 1`, which `rust-ntfs sparse --regions` writes too, so
# the host can store the same content in the same volume and compare how the
# two are laid out.
#
# -Mode write: the file is created, marked sparse (`fsutil sparse setflag`),
# written whole, and every zero gap between the regions is handed back with
# `fsutil sparse setrange` (FSCTL_SET_ZERO_DATA). That is how Windows makes
# a file with known content sparse, and what it frees is Windows' own answer
# to which ranges of such a file are holes.
#
# -Mode verify: each file is read through ntfs.sys and must hold exactly that
# content and carry the sparse attribute. A file this crate wrote must read
# on Windows as the bytes it was given.
#
# Either way, `fsutil sparse queryrange` and `fsutil file layout` for each
# file go to stdout and to <Diag>: what Windows itself says is allocated.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Files      Space-separated `/name@bytes@regions` entries.
#   -Mode       `write` or `verify`.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for sparse-<mode>.txt + sparse-error.txt.
#
# Exit code:
#   0 on success
#   1 on a failed write or a file that does not read back (sparse-error.txt)
#   2 for arg errors

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Files,
    [Parameter(Mandatory=$true)] [string]$Mode,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

if ($Mode -ne 'write' -and $Mode -ne 'verify') {
    [Console]::Error.WriteLine("win-write-sparse: -Mode must be 'write' or 'verify'; got '$Mode'")
    exit 2
}

# Parse every entry before the volume is touched, so a typo is an argument
# error rather than half a volume.
$specs = @()
foreach ($entry in ($Files -split '\s+' | Where-Object { $_ -ne '' })) {
    $parts = $entry -split '@'
    if ($parts.Count -ne 3) {
        [Console]::Error.WriteLine("win-write-sparse: '$entry' is not /name@bytes@regions")
        exit 2
    }
    $relPath = $parts[0] -replace '^[/\\]+', ''
    if ($relPath -eq '' -or $relPath -match '[\\/]') {
        [Console]::Error.WriteLine("win-write-sparse: '$($parts[0])' must name a file in the volume root")
        exit 2
    }
    $size = 0
    if (-not [int]::TryParse($parts[1], [ref]$size) -or $size -lt 1) {
        [Console]::Error.WriteLine("win-write-sparse: '$($parts[1])' in '$entry' is not a positive size")
        exit 2
    }
    $regions = @()
    if ($parts[2] -ne 'none') {
        foreach ($r in ($parts[2] -split ',')) {
            $offLen = $r -split '\+'
            $off = 0; $len = 0
            if ($offLen.Count -ne 2 -or -not [int]::TryParse($offLen[0], [ref]$off) -or
                -not [int]::TryParse($offLen[1], [ref]$len) -or $off -lt 0 -or $len -lt 1 -or
                ($off + $len) -gt $size) {
                [Console]::Error.WriteLine("win-write-sparse: region '$r' in '$entry' is not OFFSET+LENGTH inside $size bytes")
                exit 2
            }
            $regions += [pscustomobject]@{ Off = $off; Len = $len }
        }
    }
    $specs += [pscustomobject]@{ RelPath = $relPath; Size = $size; Regions = $regions }
}
if ($specs.Count -eq 0) {
    [Console]::Error.WriteLine("win-write-sparse: -Files names no file")
    exit 2
}

function New-SparseContent($spec) {
    $bytes = New-Object byte[] $spec.Size
    foreach ($r in $spec.Regions) {
        for ($i = $r.Off; $i -lt ($r.Off + $r.Len); $i++) {
            $bytes[$i] = [byte](($i -band 0xFF) -bor 1)
        }
    }
    return ,$bytes
}

# The ranges of the file outside every region, as objects with Off and Len.
function Get-ZeroGaps($spec) {
    $pos = 0
    foreach ($r in ($spec.Regions | Sort-Object Off)) {
        if ($r.Off -gt $pos) { [pscustomobject]@{ Off = $pos; Len = $r.Off - $pos } }
        $pos = [Math]::Max($pos, $r.Off + $r.Len)
    }
    if ($pos -lt $spec.Size) { [pscustomobject]@{ Off = $pos; Len = $spec.Size - $pos } }
}

# fsutil's output and exit status. Native stderr merged under 'Stop' would
# throw on its first line in Windows PowerShell, so not here.
function Invoke-Fsutil([string[]]$FsutilArgs) {
    $ErrorActionPreference = 'Continue'
    $out = & fsutil @FsutilArgs 2>&1 | Out-String
    $text = if ($out) { $out.TrimEnd() } else { '' }
    return [pscustomobject]@{ Exit = $LASTEXITCODE; Text = $text }
}

# fsutil, where a failure is the op's failure.
function Assert-Fsutil([string[]]$FsutilArgs) {
    $r = Invoke-Fsutil $FsutilArgs
    if ($r.Exit -ne 0) {
        throw "fsutil $($FsutilArgs -join ' ') exited $($r.Exit): $($r.Text)"
    }
}

# fsutil as evidence: what it said, or that it could not say.
function Show-Fsutil([string[]]$FsutilArgs) {
    $r = Invoke-Fsutil $FsutilArgs
    "--- fsutil $($FsutilArgs -join ' ') (exit $($r.Exit))"
    $r.Text
}

function Get-Sha256([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($sha.ComputeHash($Bytes)) } finally { $sha.Dispose() }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath
$report = @()

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    try {
        foreach ($spec in $specs) {
            $target = "${letter}:\$($spec.RelPath)"
            $content = New-SparseContent $spec
            if ($Mode -eq 'write') {
                [System.IO.File]::Open($target, [System.IO.FileMode]::CreateNew,
                    [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None).Close()
                Assert-Fsutil @('sparse', 'setflag', $target)
                $fs = [System.IO.File]::Open($target, [System.IO.FileMode]::Open,
                    [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
                try {
                    $fs.Write($content, 0, $content.Length)
                    $fs.Flush($true)
                } finally { $fs.Close() }
                foreach ($g in (Get-ZeroGaps $spec)) {
                    Assert-Fsutil @('sparse', 'setrange', $target, "$($g.Off)", "$($g.Len)")
                }
                $report += "wrote $target ($($spec.Size) bytes, $($spec.Regions.Count) regions)"
            } else {
                $item = Get-Item -LiteralPath $target
                if (([int]$item.Attributes -band [int][System.IO.FileAttributes]::SparseFile) -eq 0) {
                    throw "$target is not marked sparse (attributes: $($item.Attributes))"
                }
                $got = [System.IO.File]::ReadAllBytes($target)
                if ($got.Length -ne $content.Length) {
                    throw "$target is $($got.Length) bytes on Windows, not $($content.Length)"
                }
                if ((Get-Sha256 $got) -ne (Get-Sha256 $content)) {
                    throw "$target does not read back on Windows as the bytes it was written with"
                }
                $report += "verified $target ($($spec.Size) bytes, $($spec.Regions.Count) regions)"
            }
            $report += Show-Fsutil @('sparse', 'queryrange', $target)
            $report += Show-Fsutil @('file', 'layout', $target)
        }
        $text = $report -join "`r`n"
        $text | Out-File "$Diag\sparse-$Mode.txt" -Encoding UTF8
        Write-Output $text
    } catch {
        $_.Exception.Message | Out-File "$Diag\sparse-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
