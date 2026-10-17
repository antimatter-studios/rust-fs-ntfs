# _quota.ps1 -- what Windows says about a volume's quota accounting, for
# win-quota-track.ps1 and win-quota-verify.ps1 (#510). Dot-sourced.

# FileSystemControlFlags, as `fsutil quota query` prints them: the scan
# that charges every file to its owner has not finished while either is set.
$FILE_VC_QUOTA_MASK = 0x3
$FILE_VC_QUOTAS_INCOMPLETE = 0x100
$FILE_VC_QUOTAS_REBUILDING = 0x200

# `fsutil quota query X:`, as text. Throws when fsutil fails.
function Get-QuotaQuery {
    param([Parameter(Mandatory=$true)] [string]$Letter)
    $out = (& fsutil.exe quota query "${Letter}:" 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "fsutil quota query ${Letter}: failed ($LASTEXITCODE): $out"
    }
    return $out
}

# The volume's FileSystemControlFlags, read from `fsutil quota query`.
function Get-QuotaControlFlags {
    param([Parameter(Mandatory=$true)] [string]$Letter)
    $out = Get-QuotaQuery -Letter $Letter
    if ($out -notmatch 'FileSystemControlFlags\s*=\s*0x([0-9a-fA-F]+)') {
        throw "fsutil quota query ${Letter}: printed no FileSystemControlFlags: $out"
    }
    return [Convert]::ToInt64($Matches[1], 16)
}

# Wait until the scan that charges every file to its owner has finished.
# Returns the flags then. Throws when tracking is off, or after $Seconds.
function Wait-QuotaScan {
    param(
        [Parameter(Mandatory=$true)] [string]$Letter,
        [int]$Seconds = 300
    )
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ($true) {
        $flags = Get-QuotaControlFlags -Letter $Letter
        if (($flags -band $FILE_VC_QUOTA_MASK) -eq 0) {
            throw ("quota on ${Letter}: tracking is off (FileSystemControlFlags 0x{0:x})" -f $flags)
        }
        $pending = $FILE_VC_QUOTAS_INCOMPLETE -bor $FILE_VC_QUOTAS_REBUILDING
        if (($flags -band $pending) -eq 0) { return $flags }
        if ((Get-Date) -gt $deadline) {
            throw ("quota on ${Letter}: Windows' scan had not finished after $Seconds s " +
                   "(FileSystemControlFlags 0x{0:x})" -f $flags)
        }
        Start-Sleep -Seconds 1
    }
}

# Turn quota tracking on, and fail loudly when fsutil does not.
function Enable-QuotaTracking {
    param([Parameter(Mandatory=$true)] [string]$Letter)
    $out = & fsutil.exe quota track "${Letter}:" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil quota track ${Letter}: failed ($LASTEXITCODE): $out" }
}

# Each owner's usage on the volume, in bytes, keyed by account name, as
# Windows reports it: from WMI's Win32_DiskQuota, or, when that lists
# nothing for the volume, from the "SID Name" / "Quota Used" pairs of
# `fsutil quota query`. Throws when neither yields an owner.
function Get-QuotaUsage {
    param([Parameter(Mandatory=$true)] [string]$Letter)
    $usage = [ordered]@{}
    try {
        $rows = @(Get-CimInstance -ClassName Win32_DiskQuota -EA Stop |
            Where-Object { $_.QuotaVolume.DeviceID -eq "${Letter}:" })
        foreach ($r in $rows) {
            $usage["$($r.User.Domain)\$($r.User.Name)"] = [UInt64]$r.DiskSpaceUsed
        }
    } catch {
        Write-Warning "Win32_DiskQuota on ${Letter}: $_"
    }
    if ($usage.Count -eq 0) {
        $text = Get-QuotaQuery -Letter $Letter
        $pairs = [regex]::Matches($text, '(?ms)SID Name\s*=\s*([^\r\n]+).*?Quota Used\s*=\s*(\d+)')
        foreach ($m in $pairs) {
            $usage[$m.Groups[1].Value.Trim()] = [UInt64]$m.Groups[2].Value
        }
    }
    if ($usage.Count -eq 0) {
        throw "quota on ${Letter}: Windows reports no owner's usage"
    }
    return $usage
}

# A usage table as "owner<TAB>bytes" lines, sorted by owner.
function Format-QuotaUsage {
    param([Parameter(Mandatory=$true)] $Usage)
    return ($Usage.Keys | Sort-Object | ForEach-Object { "$_`t$($Usage[$_])" }) -join "`n"
}
