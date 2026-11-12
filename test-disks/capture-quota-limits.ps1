# test-disks/capture-quota-limits.ps1 -- a volume Windows enforces quotas
# on, with a per-owner warning level and hard limit, and what Windows'
# own `fsutil quota query` says about it (#868, part of #679).
#
# Formats a disposable 256 MiB volume, turns quota tracking and
# enforcement on (`fsutil quota enforce`) and waits for Windows' scan.
# Creates /limited.bin, gives it to LOCAL SERVICE and writes 64 KiB to it,
# so that owner is charged something, then sets that owner's warning level
# to 1 MiB and its hard limit to 2 MiB (`fsutil quota modify`): two
# different values, so a reader that swaps them fails.
#
# Writes, for tests/quota_limits_windows_capture.rs:
#   test-disks/quota-limits.img         the volume, detached by Windows
#   test-disks/quota-limits.tsv         fsutil's values, one row each
#   test-disks/quota-limits.fsutil.txt  fsutil's output, as it printed it
#
# The rows of quota-limits.tsv, tab-separated:
#   control_flags      FileSystemControlFlags, hex
#   default_threshold  Default Quota Threshold
#   default_limit      Default Quota Limit
#   owner  <SID>  <Quota Used>  <Quota Threshold>  <Quota Limit>
#                      one per entry fsutil lists, the SID resolved from
#                      the account name fsutil prints
#
# Throws when fsutil fails, enforcement does not come on, or fsutil's
# output does not hold every value above.

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"
. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_quota.ps1"

$image = [System.IO.Path]::GetFullPath('test-disks/quota-limits.img')
$tsv = [System.IO.Path]::GetFullPath('test-disks/quota-limits.tsv')
$raw = [System.IO.Path]::GetFullPath('test-disks/quota-limits.fsutil.txt')
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$diag = Join-Path $tempRoot ("quota-limits-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $diag -Force | Out-Null

$threshold = [UInt64]1048576
$limit = [UInt64]2097152

# A value fsutil printed, in hex (0x...) or decimal.
function ConvertFrom-FsutilNumber {
    param([Parameter(Mandatory=$true)] [string]$Text)
    $t = $Text.Trim()
    if ($t -match '^0x([0-9a-fA-F]+)$') { return [Convert]::ToUInt64($Matches[1], 16) }
    if ($t -match '^\d+$') { return [UInt64]$t }
    throw "fsutil printed '$t' where a number belongs"
}

# The SID of an account name as fsutil prints it: `DOMAIN\Name (Type)`,
# or a SID string when Windows cannot name the account.
function Resolve-FsutilOwner {
    param([Parameter(Mandatory=$true)] [string]$Name)
    $n = ($Name.Trim() -replace '\s*\([^()]*\)$', '')
    if ($n -match '^S-1-[0-9-]+$') { return $n }
    $account = New-Object System.Security.Principal.NTAccount($n)
    return $account.Translate([System.Security.Principal.SecurityIdentifier]).Value
}

$stream = [System.IO.File]::Create($image)
try { $stream.SetLength(256MB) } finally { $stream.Close() }

$vhd = Get-VhdPathFor -ImagePath $image
try {
    $state = Initialize-VhdFromImg -ImagePath $image -Diag $diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
    Format-Volume -DriveLetter $letter -FileSystem NTFS -NewFileSystemLabel 'QUOTALIM' `
        -Force -Confirm:$false | Out-Null

    $out = & fsutil.exe quota enforce "${letter}:" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil quota enforce ${letter}: failed ($LASTEXITCODE): $out" }
    Wait-QuotaScan -Letter $letter | Out-Null

    # LOCAL SERVICE, by SID, so the name is right whatever the language.
    $sid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-19')
    $owner = $sid.Translate([System.Security.Principal.NTAccount]).Value
    $target = "${letter}:\limited.bin"
    New-Item -ItemType File -Path $target -Force | Out-Null
    $out = & icacls.exe $target /setowner $owner 2>&1
    if ($LASTEXITCODE -ne 0) { throw "icacls $target /setowner ${owner}: failed ($LASTEXITCODE): $out" }
    [System.IO.File]::WriteAllBytes($target, (New-Object byte[] 65536))

    $out = & fsutil.exe quota modify "${letter}:" $threshold $limit $owner 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "fsutil quota modify ${letter}: $threshold $limit ${owner}: failed ($LASTEXITCODE): $out"
    }

    $text = Get-QuotaQuery -Letter $letter
    Set-Content -LiteralPath $raw -Value $text
    Write-Host "fsutil quota query ${letter}: (disposable volume) printed:"
    $text -split "`r?`n" | ForEach-Object { Write-Host "  $_" }

    $rows = @("# fsutil quota query ${letter}: on a disposable volume, quotas enforced")
    if ($text -notmatch 'FileSystemControlFlags\s*=\s*(0x[0-9a-fA-F]+)') {
        throw "fsutil printed no FileSystemControlFlags"
    }
    $rows += "control_flags`t$($Matches[1])"
    if ($text -notmatch 'Default Quota Threshold\s*=\s*(\S+)') { throw "fsutil printed no Default Quota Threshold" }
    $rows += "default_threshold`t$(ConvertFrom-FsutilNumber $Matches[1])"
    if ($text -notmatch 'Default Quota Limit\s*=\s*(\S+)') { throw "fsutil printed no Default Quota Limit" }
    $rows += "default_limit`t$(ConvertFrom-FsutilNumber $Matches[1])"

    $entries = [regex]::Matches($text,
        '(?ms)SID Name\s*=\s*([^\r\n]+).*?Quota Used\s*=\s*(\S+).*?Quota Threshold\s*=\s*(\S+).*?Quota Limit\s*=\s*(\S+)')
    if ($entries.Count -eq 0) { throw "fsutil listed no owner's entry" }
    foreach ($m in $entries) {
        $ownerSid = Resolve-FsutilOwner -Name $m.Groups[1].Value
        $used = ConvertFrom-FsutilNumber $m.Groups[2].Value
        $thr = ConvertFrom-FsutilNumber $m.Groups[3].Value
        $lim = ConvertFrom-FsutilNumber $m.Groups[4].Value
        $rows += "owner`t$ownerSid`t$used`t$thr`t$lim"
    }
    Set-Content -LiteralPath $tsv -Value $rows
    Get-Content -LiteralPath $tsv

    # Windows' own detach, and the bytes it left copied back into the .img.
    # A sync that fails throws here: the .img would not be the volume
    # fsutil described.
    Sync-VhdToImg -Vhd $vhd -ImagePath $image
} finally {
    Dismount-VhdAndCleanup -Vhd $vhd -ImagePath $image -KeepImage $true -SyncBack $false
    Remove-Item -LiteralPath $vhd -Force -EA SilentlyContinue
}
