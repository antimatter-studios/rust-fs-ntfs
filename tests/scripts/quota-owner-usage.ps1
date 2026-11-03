# tests/scripts/quota-owner-usage.ps1 -- an owner's quota usage, read through
# the real Get-OwnerUsage from what Windows prints, with fsutil and WMI replaced
# by mocks and no VM.
#
#   pwsh -NoProfile -File tests/scripts/quota-owner-usage.ps1
#
# CI runs it in the ubuntu-latest "quota owner usage test" step.
#
# The fsutil text below is verbatim from quota-enforce.txt in the artifact
# ntfs-quota-enforce-measurement of run 41558177459. That report said
# usage_before=0 and usage_after=0 beside this text, which charges LOCAL SERVICE
# 1024 and then 66560 bytes (#867).

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../scripts/fs-windows-test-harness/_quota.ps1"

$script:fails = 0
function Check($got, $want, $what) {
    if ("$got" -ceq "$want") {
        Write-Host "ok   $what"
    } else {
        Write-Host "FAIL $what`: got '$got', want '$want'"
        $script:fails++
    }
}

$LimitSet = @'
FileSystemControlFlags = 0x00000002
    Quotas are tracked and enforced on this volume
    Logging for quota events is not enabled
    The quota values are up to date

Default Quota Threshold = 0xffffffffffffffff
Default Quota Limit     = 0xffffffffffffffff

SID Name        = BUILTIN\Administrators (Alias)
Change time     = Tuesday, November 3, 2026   1:42:03 AM
Quota Used      = 70656
Quota Threshold = 18446744073709551615
Quota Limit     = 18446744073709551615

SID Name        = NT AUTHORITY\NETWORK SERVICE (WellKnownGroup)
Change time     = Tuesday, November 3, 2026   1:42:29 AM
Quota Used      = 1024
Quota Threshold = 18446744073709551615
Quota Limit     = 18446744073709551615

SID Name        = NT AUTHORITY\SYSTEM (WellKnownGroup)
Change time     = Tuesday, November 3, 2026   1:42:29 AM
Quota Used      = 4197376
Quota Threshold = 18446744073709551615
Quota Limit     = 18446744073709551615

SID Name        = NT AUTHORITY\LOCAL SERVICE (WellKnownGroup)
Change time     = Tuesday, November 3, 2026   1:42:30 AM
Quota Used      = 1024
Quota Threshold = 65536
Quota Limit     = 65536
'@

$AfterWrite = $LimitSet -replace '(?m)^(SID Name\s*=\s*NT AUTHORITY\\LOCAL SERVICE .*\r?\n.*\r?\nQuota Used\s*=\s*)1024', '${1}66560'

$script:Query = $LimitSet
$script:Rows = @()
function fsutil.exe {
    $global:LASTEXITCODE = 0
    $script:Query -split "`r?`n"
}
function Get-CimInstance {
    [CmdletBinding()] param([string]$ClassName)
    $script:Rows
}

$LocalService = 'NT AUTHORITY\LOCAL SERVICE'

# WMI lists nothing for the volume.
Check (Get-OwnerUsage -Letter X -Owner $LocalService) 1024 'the owner fsutil names with its SID type'
Check (Get-OwnerUsage -Letter X -Owner 'NT AUTHORITY\SYSTEM') 4197376 'each owner has its own usage, not the next one''s'
Check (Get-OwnerUsage -Letter X -Owner 'BUILTIN\Administrators') 70656 'an owner of another SID type'

$script:Query = $AfterWrite
Check (Get-OwnerUsage -Letter X -Owner $LocalService) 66560 'the usage after the write'

# WMI lists other owners but not this one.
$script:Rows = @([pscustomobject]@{
    QuotaVolume   = [pscustomobject]@{ DeviceID = 'X:' }
    User          = [pscustomobject]@{ Domain = 'BUILTIN'; Name = 'Administrators' }
    DiskSpaceUsed = 70656
})
Check (Get-OwnerUsage -Letter X -Owner $LocalService) 66560 'an owner WMI does not list'

# An owner with no entry is charged nothing, and a name is matched whole.
Check (Get-OwnerUsage -Letter X -Owner 'NT AUTHORITY\ANONYMOUS LOGON') 0 'an owner with no entry'
Check (Get-OwnerUsage -Letter X -Owner 'NT AUTHORITY\LOCAL') 0 'the start of another owner''s name'

if ($script:fails -gt 0) {
    Write-Host "quota-owner-usage: $($script:fails) check(s) failed"
    exit 1
}
Write-Host 'quota-owner-usage: all checks passed'
