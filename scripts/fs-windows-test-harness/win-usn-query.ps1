# win-usn-query.ps1 -- does WINDOWS still report the change journal
# win-usn-create gave the volume?
#
# A change journal is a promise that every change since a consumer's last USN
# is in it. A writer that changed the volume without recording the change in
# it must not leave the journal reporting the same id, or a consumer reading
# on from its last USN misses the change and cannot tell (#479). Windows
# answers through ntfs.sys, not through our reading of $UsnJrnl.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Expect     `Retired`: Windows reports no journal, or one with a different
#               id. `Kept`: Windows reports the journal win-usn-create noted.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory holding win-usn-create's usn-journal-id.txt; this
#               writes usn-query.txt, fsutil's own words.
#
# Exit code:
#   0 when Windows' answer is -Expect
#   1 when it is not, or fsutil's answer is not understood
#   2 for a bad -Expect, or no usn-journal-id.txt to compare with

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Expect,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

if ($Expect -ne 'Retired' -and $Expect -ne 'Kept') {
    [Console]::Error.WriteLine("win-usn-query: -Expect must be Retired or Kept; got '$Expect'")
    exit 2
}
$idFile = "$Diag\usn-journal-id.txt"
if (-not (Test-Path -LiteralPath $idFile)) {
    [Console]::Error.WriteLine("win-usn-query: no $idFile; a win-usn-create step must come first")
    exit 2
}
$before = (Get-Content -LiteralPath $idFile -Raw).Trim()

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

# fsutil's answer, its error included. Windows PowerShell turns a native
# command's redirected stderr into an error, which 'Stop' would throw; "not
# active" is an answer here, not a failure.
function Get-JournalAnswer([string]$Volume) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        (& fsutil.exe usn queryjournal $Volume 2>&1 | Out-String).Trim()
    } finally {
        $ErrorActionPreference = $saved
    }
}

$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
    $volume = "${letter}:"

    # A deletion under way answers "being deleted" until it completes.
    $answer = Get-JournalAnswer $volume
    for ($i = 0; $i -lt 30 -and $answer -match 'being deleted'; $i++) {
        Start-Sleep -Seconds 2
        $answer = Get-JournalAnswer $volume
    }
    $answer | Out-File "$Diag\usn-query.txt" -Encoding UTF8

    if ($answer -match 'Usn Journal ID\s*:\s*(0x[0-9a-fA-F]+)') {
        $now = $Matches[1]
        $got = if ($now -eq $before) { 'Kept' } else { 'Retired' }
        $said = "journal $now, which was $before"
    } elseif ($answer -match 'not active') {
        $got = 'Retired'
        $said = "no journal, which was $before"
    } else {
        [Console]::Error.WriteLine("win-usn-query: fsutil's answer is not understood: $answer")
        exit 1
    }
    if ($got -ne $Expect) {
        [Console]::Error.WriteLine("win-usn-query: Windows reports $said; expected $Expect")
        exit 1
    }
    Write-Host "win-usn-query: Windows reports $said, as expected ($Expect)"
    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
