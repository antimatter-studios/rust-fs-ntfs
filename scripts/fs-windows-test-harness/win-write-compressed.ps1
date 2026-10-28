# scripts/fs-windows-test-harness/win-write-compressed.ps1 -- have Windows
# write compressed files, or check compressed files someone else wrote, on a
# mounted NTFS volume.
#
# Each entry of -Files is `/name@bytes`: a file of `bytes` bytes whose byte at
# offset i is `0x41 + ((i -shr 12) % 26)`, one letter per 4 KiB block. That
# compresses well, so a compressed file stores fewer clusters than it spans,
# and the host can write the same content and compare what each is charged.
#
# -Mode write: the file is written whole and then compressed with `compact
# /c`, so how it is laid out and what it is charged is Windows' own answer.
#
# -Mode verify: each file is read through ntfs.sys and must hold exactly that
# content and carry the compressed attribute. A file this crate wrote must
# read on Windows as the bytes it was given.
#
# Either way, `compact /q` and `fsutil file layout` for each file go to stdout
# and to <Diag>: what Windows itself says is stored.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Files      Space-separated `/name@bytes` entries.
#   -Mode       `write` or `verify`.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for compressed-<mode>.txt + compressed-error.txt.
#
# Exit code:
#   0 on success
#   1 on a failed write or a file that does not read back (compressed-error.txt)
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
    [Console]::Error.WriteLine("win-write-compressed: -Mode must be 'write' or 'verify'; got '$Mode'")
    exit 2
}

# Parse every entry before the volume is touched, so a typo is an argument
# error rather than half a volume.
$specs = @()
foreach ($entry in ($Files -split '\s+' | Where-Object { $_ -ne '' })) {
    $parts = $entry -split '@'
    if ($parts.Count -ne 2) {
        [Console]::Error.WriteLine("win-write-compressed: '$entry' is not /name@bytes")
        exit 2
    }
    $relPath = $parts[0] -replace '^[/\\]+', ''
    if ($relPath -eq '' -or $relPath -match '[\\/]') {
        [Console]::Error.WriteLine("win-write-compressed: '$($parts[0])' must name a file in the volume root")
        exit 2
    }
    $size = 0
    if (-not [int]::TryParse($parts[1], [ref]$size) -or $size -lt 1) {
        [Console]::Error.WriteLine("win-write-compressed: '$($parts[1])' in '$entry' is not a positive size")
        exit 2
    }
    $specs += [pscustomobject]@{ RelPath = $relPath; Size = $size }
}
if ($specs.Count -eq 0) {
    [Console]::Error.WriteLine("win-write-compressed: -Files names no file")
    exit 2
}

function New-CompressibleContent($spec) {
    $bytes = New-Object byte[] $spec.Size
    for ($i = 0; $i -lt $spec.Size; $i++) {
        $bytes[$i] = [byte](0x41 + (($i -shr 12) % 26))
    }
    return ,$bytes
}

# A native tool's output and exit status. Native stderr merged under 'Stop'
# would throw on its first line in Windows PowerShell, so not here.
function Invoke-Tool([string]$Tool, [string[]]$ToolArgs) {
    $ErrorActionPreference = 'Continue'
    $out = & $Tool @ToolArgs 2>&1 | Out-String
    $text = if ($out) { $out.TrimEnd() } else { '' }
    return [pscustomobject]@{ Exit = $LASTEXITCODE; Text = $text }
}

# A native tool, where a failure is the op's failure.
function Assert-Tool([string]$Tool, [string[]]$ToolArgs) {
    $r = Invoke-Tool $Tool $ToolArgs
    if ($r.Exit -ne 0) {
        throw "$Tool $($ToolArgs -join ' ') exited $($r.Exit): $($r.Text)"
    }
}

# A native tool as evidence: what it said, or that it could not say.
function Show-Tool([string]$Tool, [string[]]$ToolArgs) {
    $r = Invoke-Tool $Tool $ToolArgs
    "--- $Tool $($ToolArgs -join ' ') (exit $($r.Exit))"
    $r.Text
}

function Get-Sha256([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($sha.ComputeHash($Bytes)) } finally { $sha.Dispose() }
}

function Assert-Compressed([string]$Target) {
    $item = Get-Item -LiteralPath $Target
    if (([int]$item.Attributes -band [int][System.IO.FileAttributes]::Compressed) -eq 0) {
        throw "$Target is not marked compressed (attributes: $($item.Attributes))"
    }
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
            $content = New-CompressibleContent $spec
            if ($Mode -eq 'write') {
                $fs = [System.IO.File]::Open($target, [System.IO.FileMode]::CreateNew,
                    [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
                try {
                    $fs.Write($content, 0, $content.Length)
                    $fs.Flush($true)
                } finally { $fs.Close() }
                Assert-Tool 'compact' @('/c', $target)
                Assert-Compressed $target
                $report += "wrote $target ($($spec.Size) bytes, compressed)"
            } else {
                Assert-Compressed $target
                $got = [System.IO.File]::ReadAllBytes($target)
                if ($got.Length -ne $content.Length) {
                    throw "$target is $($got.Length) bytes on Windows, not $($content.Length)"
                }
                if ((Get-Sha256 $got) -ne (Get-Sha256 $content)) {
                    throw "$target does not read back on Windows as the bytes it was written with"
                }
                $report += "verified $target ($($spec.Size) bytes, compressed)"
            }
            $report += Show-Tool 'compact' @('/q', $target)
            $report += Show-Tool 'fsutil' @('file', 'layout', $target)
        }
        $text = $report -join "`r`n"
        $text | Out-File "$Diag\compressed-$Mode.txt" -Encoding UTF8
        Write-Output $text
    } catch {
        $_.Exception.Message | Out-File "$Diag\compressed-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
