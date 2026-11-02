# scripts/fs-windows-test-harness/win-quota-enforce.ps1 -- what Windows
# does with a write that would take a file's owner past an enforced quota
# hard limit (#867, part of #679).
#
# Turns quota tracking and enforcement on, as `fsutil quota enforce` does,
# and waits for Windows' scan. Creates the file at -Path, gives it to
# LOCAL SERVICE (the Administrators group, which owns what this session
# creates, takes no quota limit), sets that owner's warning level and hard
# limit to -HeadroomBytes above what it is charged, and then writes
# -WriteBytes to the file, past the limit.
#
# quota-enforce.txt in -Diag records the measurement: the error the write
# returned (HRESULT, Win32 code, message), how many bytes the write accepted
# before it, the file's length afterwards, the owner's usage before and
# after, and `fsutil quota query` before and after.
#
# Args:
#   -ImagePath      Path on the VM to the .img file.
#   -Path           File path inside the volume, e.g. "/limited.bin".
#   -HeadroomBytes  How far above the owner's usage the hard limit sits.
#   -WriteBytes     How many bytes to write; more than -HeadroomBytes.
#   -KeepImage      `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag           Directory for quota-enforce.txt.
#
# Exit code:
#   0 when Windows refused the write past the limit
#   1 when it did not, or fsutil, the scan or the setup failed

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Path,
    [Parameter(Mandatory=$true)] [string]$HeadroomBytes,
    [Parameter(Mandatory=$true)] [string]$WriteBytes,
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

$relPath = $Path -replace '^[/\\]+', ''
if ($relPath -eq '') {
    [Console]::Error.WriteLine("win-quota-enforce: -Path must name a file under the volume root; got '$Path'")
    exit 1
}
$headroom = [UInt64]$HeadroomBytes
$toWrite = [UInt64]$WriteBytes
if ($toWrite -le $headroom) {
    [Console]::Error.WriteLine("win-quota-enforce: -WriteBytes ($toWrite) must exceed -HeadroomBytes ($headroom)")
    exit 1
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath
$report = "$Diag\quota-enforce.txt"

# The owner's usage as Windows reports it; 0 when it has no entry yet.
function Get-OwnerUsage {
    param([string]$Letter, [string]$Owner)
    $usage = Get-QuotaUsage -Letter $Letter
    if ($usage.Contains($Owner)) { return [UInt64]$usage[$Owner] }
    return [UInt64]0
}

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $out = & fsutil.exe quota enforce "${letter}:" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil quota enforce ${letter}: failed ($LASTEXITCODE): $out" }
    $flags = Wait-QuotaScan -Letter $letter

    # LOCAL SERVICE, by SID, so the name is right whatever the language.
    $sid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-19')
    $owner = $sid.Translate([System.Security.Principal.NTAccount]).Value

    $target = "${letter}:\$relPath"
    New-Item -ItemType File -Path $target -Force | Out-Null
    $out = & icacls.exe $target /setowner $owner 2>&1
    if ($LASTEXITCODE -ne 0) { throw "icacls $target /setowner ${owner}: failed ($LASTEXITCODE): $out" }

    $usedBefore = Get-OwnerUsage -Letter $letter -Owner $owner
    $limit = $usedBefore + $headroom
    $out = & fsutil.exe quota modify "${letter}:" $limit $limit $owner 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil quota modify ${letter}: $limit $limit ${owner}: failed ($LASTEXITCODE): $out" }
    $queryBefore = Get-QuotaQuery -Letter $letter

    # Write-through, so a refusal surfaces at the write that met the limit
    # rather than at a lazy flush after the handle is gone.
    $accepted = [UInt64]0
    $failure = $null
    $chunk = New-Object byte[] 65536
    $fs = New-Object System.IO.FileStream($target, [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Write, [System.IO.FileShare]::None, 4096,
        [System.IO.FileOptions]::WriteThrough)
    try {
        while ($accepted -lt $toWrite) {
            $n = [int][Math]::Min([UInt64]$chunk.Length, $toWrite - $accepted)
            $fs.Write($chunk, 0, $n)
            $fs.Flush($true)
            $accepted += [UInt64]$n
        }
    } catch {
        $failure = $_.Exception
        while ($failure -is [System.Management.Automation.MethodInvocationException] -and
               $failure.InnerException) {
            $failure = $failure.InnerException
        }
    } finally {
        try { $fs.Dispose() } catch {
            if (-not $failure) { $failure = $_.Exception }
        }
    }

    $lengthAfter = (Get-Item -LiteralPath $target).Length
    $usedAfter = Get-OwnerUsage -Letter $letter -Owner $owner
    $queryAfter = Get-QuotaQuery -Letter $letter

    $lines = @(
        ("control_flags=0x{0:x}" -f $flags),
        "owner=$owner",
        "limit=$limit",
        "usage_before=$usedBefore",
        "write_bytes=$toWrite",
        "accepted_bytes=$accepted",
        "length_after=$lengthAfter",
        "usage_after=$usedAfter"
    )
    if ($failure) {
        $hr = $failure.HResult
        $lines += ("error_hresult=0x{0:x8}" -f $hr)
        $lines += ("error_win32={0}" -f ($hr -band 0xFFFF))
        $lines += "error_type=$($failure.GetType().FullName)"
        $lines += "error_message=$($failure.Message)"
    } else {
        $lines += "error_hresult=none"
    }
    $lines += '--- fsutil quota query, limit set, before the write ---'
    $lines += $queryBefore
    $lines += '--- fsutil quota query, after the write ---'
    $lines += $queryAfter
    $lines -join "`r`n" | Out-File $report -Encoding UTF8

    if (-not $failure) {
        throw ("Windows accepted all $toWrite bytes past a hard limit of $limit " +
               "($owner charged $usedBefore before, $usedAfter after); see $report")
    }
    Write-Host ("win-quota-enforce: Windows refused the write past the limit: HRESULT 0x{0:x8}, " +
                "$accepted of $toWrite bytes accepted, length $lengthAfter, usage $usedBefore -> $usedAfter" `
                -f $failure.HResult)
    exit 0
} catch {
    [Console]::Error.WriteLine("win-quota-enforce: $($_.Exception.Message)")
    exit 1
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
