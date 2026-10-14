# scripts/fs-windows-test-harness/win-write-junctions.ps1 -- make junctions
# on a mounted NTFS volume, so that Windows itself grows
# $Extend\$Reparse:$R past its root into INDX blocks.
#
# Each junction is `<Dir>\j<N>` (N zero-padded to three digits, from 000),
# an empty directory carrying a mount-point reparse point that names
# `<Dir>\target`, an empty directory beside it. $R holds one 32-byte entry
# per reparse point, so a few dozen fill a 1 KiB record's $INDEX_ROOT and
# Windows spills the index into blocks: the shape a host-side insert of a
# reparse point has to find its way through (#571).
#
# A junction names its target by drive letter, which means nothing once
# the volume is mounted elsewhere. A scenario using this op does not walk
# the volume afterwards, so nothing follows one.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Dir        Directory inside the volume, e.g. "/junctions". Created.
#   -Count      Positive integer: how many junctions.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for write-junctions-result.txt + write-junctions-error.txt.
#
# Exit code:
#   0 on success
#   1 on a failure to make a junction (logged to write-junctions-error.txt)
#   2 for arg errors

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Dir,
    [Parameter(Mandatory=$true)] [string]$Count,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

$countInt = 0
if (-not [int]::TryParse($Count, [ref]$countInt) -or $countInt -lt 1 -or $countInt -gt 999) {
    [Console]::Error.WriteLine("invalid -Count: '$Count' (expected 1 to 999)")
    exit 2
}

$relDir = $Dir -replace '^[/\\]+', ''
if ($relDir -eq '') {
    [Console]::Error.WriteLine("win-write-junctions: -Dir must name a directory under the volume root; got '$Dir'")
    exit 2
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $base = "${letter}:\$relDir"
    try {
        $target = Join-Path $base 'target'
        [System.IO.Directory]::CreateDirectory($target) | Out-Null
        for ($i = 0; $i -lt $countInt; $i++) {
            $link = Join-Path $base ('j{0:D3}' -f $i)
            New-Item -ItemType Junction -Path $link -Target $target | Out-Null
        }
        "made $countInt junctions under $base" |
            Out-File "$Diag\write-junctions-result.txt" -Encoding UTF8
    } catch {
        $_.Exception.Message | Out-File "$Diag\write-junctions-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
