# scripts/fs-windows-test-harness/win-write-streams.ps1 -- write one file
# carrying many named streams to a mounted NTFS volume, so that Windows
# itself spreads the file's attributes over extension records behind an
# $ATTRIBUTE_LIST.
#
# Each stream is -SizeBytes long, which past about 700 bytes is too big to
# stay resident, so every stream is a non-resident $DATA attribute whose
# header (with its name and mapping pairs) takes some 80 bytes of an MFT
# record. Forty of them do not fit in one 1 KiB record: Windows moves
# attributes to extension records and names them in an $ATTRIBUTE_LIST,
# the shape a host-side unlink then has to free (#565).
#
# Stream i is named `s<i>` and holds the bytes of "stream <i> " repeated
# to -SizeBytes, so a stream read back in the wrong place is visible.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Path       File path inside the volume, e.g. "/streams.txt".
#   -Count      Positive integer; how many named streams to write.
#   -SizeBytes  Bytes per stream (> 0).
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for write-streams-result.txt + write-streams-error.txt.
#
# Exit code:
#   0 on success
#   1 on write failure (logged to write-streams-error.txt)
#   2 for arg errors

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Path,
    [Parameter(Mandatory=$true)] [string]$Count,
    [Parameter(Mandatory=$true)] [string]$SizeBytes,
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
if (-not [int]::TryParse($Count, [ref]$countInt) -or $countInt -lt 1) {
    [Console]::Error.WriteLine("invalid -Count: '$Count' (expected a positive integer)")
    exit 2
}
$sizeInt = 0
if (-not [int]::TryParse($SizeBytes, [ref]$sizeInt) -or $sizeInt -lt 1) {
    [Console]::Error.WriteLine("invalid -SizeBytes: '$SizeBytes' (expected a positive integer)")
    exit 2
}

$relPath = $Path -replace '^[/\\]+', ''
if ($relPath -eq '') {
    [Console]::Error.WriteLine("win-write-streams: -Path must name a file under the volume root; got '$Path'")
    exit 2
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $target = "${letter}:\$relPath"
    try {
        [System.IO.File]::WriteAllBytes($target, [System.Text.Encoding]::UTF8.GetBytes("the unnamed stream"))
        for ($i = 1; $i -le $countInt; $i++) {
            $unit = [System.Text.Encoding]::UTF8.GetBytes("stream $i ")
            $bytes = New-Object byte[] $sizeInt
            for ($b = 0; $b -lt $sizeInt; $b++) { $bytes[$b] = $unit[$b % $unit.Length] }
            Set-Content -LiteralPath $target -Stream "s$i" -Value $bytes -Encoding Byte
        }
        $streams = @(Get-Item -LiteralPath $target -Stream * | Where-Object { $_.Stream -ne ':$DATA' })
        if ($streams.Count -ne $countInt) {
            throw "wrote $countInt streams to $target but Windows lists $($streams.Count)"
        }
        "wrote $countInt streams of $sizeInt bytes to $target" |
            Out-File "$Diag\write-streams-result.txt" -Encoding UTF8
    } catch {
        $_.Exception.Message | Out-File "$Diag\write-streams-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
