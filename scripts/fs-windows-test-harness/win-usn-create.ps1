# win-usn-create.ps1 -- give a mounted NTFS volume a change journal ($UsnJrnl)
# and some changes for it to record, then note which journal Windows reports.
#
# Runs `fsutil usn createjournal`, writes `\journaled.txt` and creates
# `\journaled-dir`, and keeps `fsutil usn queryjournal`'s answer in
# usn-create.txt and the journal's id alone in usn-journal-id.txt, for a
# later win-usn-query to compare with (#479).
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for usn-create.txt and usn-journal-id.txt.
#
# Exit code:
#   0 on success
#   1 when fsutil fails or reports no journal id

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
Remove-Item -LiteralPath "$Diag\usn-journal-id.txt" -Force -ErrorAction SilentlyContinue
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
    $volume = "${letter}:"

    $o = (& fsutil.exe usn createjournal m=8388608 a=1048576 $volume 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("win-usn-create: fsutil usn createjournal failed: $o")
        exit 1
    }
    [System.IO.File]::WriteAllText("$volume\journaled.txt", 'written while Windows journaled')
    New-Item -ItemType Directory -Path "$volume\journaled-dir" -Force | Out-Null

    $answer = (& fsutil.exe usn queryjournal $volume 2>&1 | Out-String).Trim()
    $answer | Out-File "$Diag\usn-create.txt" -Encoding UTF8
    if ($LASTEXITCODE -ne 0 -or $answer -notmatch 'Usn Journal ID\s*:\s*(0x[0-9a-fA-F]+)') {
        [Console]::Error.WriteLine("win-usn-create: fsutil usn queryjournal reports no journal: $answer")
        exit 1
    }
    $Matches[1] | Out-File "$Diag\usn-journal-id.txt" -Encoding ASCII
    Write-Host "win-usn-create: Windows journals this volume as $($Matches[1])"
    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
