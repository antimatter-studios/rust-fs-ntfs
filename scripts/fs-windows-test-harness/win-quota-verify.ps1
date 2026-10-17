# scripts/fs-windows-test-harness/win-quota-verify.ps1 -- does each owner's
# quota usage, as the volume holds it, match what Windows itself charges
# when it counts every file again?
#
# The oracle for quota accounting kept by writes made elsewhere (#510).
# Mounts the .img (re-using a prior op's .vhd if `keep_image=true` was set
# on the prior step) and reads each owner's usage, which comes straight
# from $Extend\$Quota:$Q. Then it turns tracking off and on again, which
# has NTFS scan the volume and charge every file to its owner from
# scratch, waits for that scan, and reads the usage again. A write that
# changed a file's allocation without moving its owner's usage shows as a
# difference between the two.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for quota-before.txt, quota-after.txt and
#               quota-query-before.txt / quota-query-after.txt.
#
# Exit code:
#   0 when every owner's usage is what Windows' own count gives
#   1 when one differs, tracking is off, or fsutil fails

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

    # What the volume holds: no scan may be running, or it is Windows'
    # count, not the volume's, that is read.
    Wait-QuotaScan -Letter $letter | Out-Null
    Get-QuotaQuery -Letter $letter | Out-File "$Diag\quota-query-before.txt" -Encoding UTF8
    $before = Get-QuotaUsage -Letter $letter
    Format-QuotaUsage -Usage $before | Out-File "$Diag\quota-before.txt" -Encoding UTF8

    # Windows' own count.
    $out = & fsutil.exe quota disable "${letter}:" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil quota disable ${letter}: failed ($LASTEXITCODE): $out" }
    Enable-QuotaTracking -Letter $letter
    Wait-QuotaScan -Letter $letter | Out-Null
    Get-QuotaQuery -Letter $letter | Out-File "$Diag\quota-query-after.txt" -Encoding UTF8
    $after = Get-QuotaUsage -Letter $letter
    Format-QuotaUsage -Usage $after | Out-File "$Diag\quota-after.txt" -Encoding UTF8

    $owners = @($before.Keys) + @($after.Keys) | Sort-Object -Unique
    $diffs = @()
    foreach ($o in $owners) {
        $b = if ($before.Contains($o)) { $before[$o] } else { 0 }
        $a = if ($after.Contains($o)) { $after[$o] } else { 0 }
        if ($b -ne $a) { $diffs += "$o`: the volume held $b bytes, Windows counts $a" }
    }
    if (@($after.Values | Where-Object { $_ -gt 0 }).Count -eq 0) {
        throw "Windows' count charges no owner anything: there was nothing to compare"
    }
    if ($diffs.Count -gt 0) {
        [Console]::Error.WriteLine("win-quota-verify: an owner's usage is not what Windows counts:")
        foreach ($d in $diffs) { [Console]::Error.WriteLine("  $d") }
        exit 1
    }
    Write-Host "win-quota-verify: every owner's usage is what Windows counts ($($owners.Count) owners)"
    exit 0
} catch {
    [Console]::Error.WriteLine("win-quota-verify: $($_.Exception.Message)")
    exit 1
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
