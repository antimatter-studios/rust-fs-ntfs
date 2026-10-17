# scripts/fs-windows-test-harness/win-quota-track.ps1 -- turn quota tracking
# on for a volume, as `fsutil quota track` does, and wait until Windows has
# charged every file on it to its owner.
#
# With tracking on, NTFS charges each file's allocation to its owner's
# entry in $Extend\$Quota:$Q, and records the owner and the charge in the
# file's $STANDARD_INFORMATION. Turning it on starts a scan of the volume
# in the background; this waits for the scan, so a follow-on op sees a
# volume whose accounting Windows finished (#510).
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for quota-track.txt (fsutil's own words).
#
# Exit code:
#   0 once tracking is on and the scan has finished
#   1 when fsutil fails or the scan does not finish

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"
. "$PSScriptRoot\_quota.ps1"

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    Enable-QuotaTracking -Letter $letter
    $flags = Wait-QuotaScan -Letter $letter
    Get-QuotaQuery -Letter $letter | Out-File "$Diag\quota-track.txt" -Encoding UTF8
    Write-Host ("win-quota-track: tracking on, scan finished (FileSystemControlFlags 0x{0:x})" -f $flags)
    exit 0
} catch {
    [Console]::Error.WriteLine("win-quota-track: $($_.Exception.Message)")
    exit 1
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
