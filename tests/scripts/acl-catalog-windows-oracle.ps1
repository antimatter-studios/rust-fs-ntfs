# Windows oracle for `fs acl catalog` (#606): Windows formats a fixed VHD and
# gives each of $Count files a DACL of its own, so $Secure:$SDS holds more
# descriptors than one 256 KiB region, and Windows has to start a second
# primary region at 512 KiB. Each file's descriptor is read back through
# Windows' own GetFileSecurityW. The CLI's catalog of the detached image must
# hold every one of them, as Windows reads it, in exactly one primary copy;
# every copy must verify, every primary's mirror must be the same, and at
# least one primary must sit in the second region, or the case proved nothing.
#
# Usage: acl-catalog-windows-oracle.ps1 -Cli <rust-fs-ntfs.exe> -OutDir <dir> [-Count N]
param(
    [Parameter(Mandatory = $true)][string]$Cli,
    [Parameter(Mandatory = $true)][string]$OutDir,
    [int]$Count = 2400
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$img = Join-Path (Resolve-Path $OutDir) 'acl-catalog.vhd'
$letter = 'S'
# One $SDS region; a primary region is followed by its mirror.
$REGION = 262144

Add-Type -Namespace AclCatalog -Name Native -MemberDefinition @'
[DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool SetFileSecurityW(string lpFileName, uint SecurityInformation, byte[] pSecurityDescriptor);
[DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool GetFileSecurityW(string lpFileName, uint RequestedInformation, byte[] pSecurityDescriptor, uint nLength, out uint lpnLengthNeeded);
'@
$DACL_ONLY = [uint32]0x4
$OWNER_GROUP_DACL = [uint32]0x7

function Hex([byte[]]$bytes) {
    return [BitConverter]::ToString($bytes).Replace('-', '').ToLowerInvariant()
}

function Invoke-Diskpart([string[]]$lines) {
    $script = Join-Path $env:RUNNER_TEMP ("diskpart-" + [guid]::NewGuid() + '.txt')
    Set-Content -Path $script -Value $lines -Encoding ascii
    $out = diskpart /s $script
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed ($LASTEXITCODE): $out" }
}

# The trustee that makes file $i's descriptor its own: no two files share a
# descriptor, so Windows cannot store one for several.
function Trustee([int]$i) { return "S-1-5-21-1000-2000-3000-$(100000 + $i)" }

try {
Invoke-Diskpart @(
    "create vdisk file=`"$img`" maximum=128 type=fixed",
    "select vdisk file=`"$img`"",
    'attach vdisk',
    'convert mbr',
    'create partition primary',
    'format fs=ntfs quick label=ACLCATALOG',
    "assign letter=$letter"
)
$offset = (Get-Partition -DriveLetter $letter).Offset
$dir = "${letter}:\many"
New-Item -ItemType Directory -Path $dir | Out-Null

# Windows' own reading of each file's ACE for its trustee, by trustee.
$windows = @{}
for ($i = 0; $i -lt $Count; $i++) {
    $file = Join-Path $dir ('f{0:d5}.txt' -f $i)
    [IO.File]::WriteAllText($file, 'catalog')
    $sid = Trustee $i
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor(
        "D:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;$sid)")
    $bytes = New-Object byte[] $sd.BinaryLength
    $sd.GetBinaryForm($bytes, 0)
    if (-not [AclCatalog.Native]::SetFileSecurityW($file, $DACL_ONLY, $bytes)) {
        throw "SetFileSecurityW $file failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    $need = [uint32]0
    [void][AclCatalog.Native]::GetFileSecurityW($file, $OWNER_GROUP_DACL, $null, 0, [ref]$need)
    $buf = New-Object byte[] $need
    if (-not [AclCatalog.Native]::GetFileSecurityW($file, $OWNER_GROUP_DACL, $buf, $need, [ref]$need)) {
        throw "GetFileSecurityW $file failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    $raw = New-Object System.Security.AccessControl.RawSecurityDescriptor($buf, 0)
    $own = @($raw.DiscretionaryAcl | Where-Object { $_.SecurityIdentifier.Value -eq $sid })
    if ($own.Count -ne 1) { throw "${file}: Windows read back $($own.Count) ACEs for $sid" }
    $ace = New-Object byte[] $own[0].BinaryLength
    $own[0].GetBinaryForm($ace, 0)
    $windows[$sid] = Hex $ace
}
Invoke-Diskpart @("select vdisk file=`"$img`"", 'detach vdisk')
$imageBefore = (Get-FileHash -LiteralPath $img -Algorithm SHA256).Hash

$lines = & $Cli fs --offset $offset $img acl catalog --jsonl
if ($LASTEXITCODE -ne 0) { throw "CLI acl catalog exited $LASTEXITCODE" }
$copies = @($lines | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json })
$primaries = @($copies | Where-Object { $_.copy -eq 'primary' })
$mirrors = @($copies | Where-Object { $_.copy -eq 'mirror' })

$failures = @()
foreach ($c in $copies) {
    if (-not $c.hash_verified) {
        $failures += "copy at $($c.physical): stored hash $($c.stored_hash), computed $($c.computed_hash)"
    }
    if (-not $c.offset_verified) {
        $failures += "copy at $($c.physical): header offset $($c.logical)"
    }
    if ($null -ne $c.descriptor.error) {
        $failures += "copy at $($c.physical): $($c.descriptor.error)"
    }
}
foreach ($p in $primaries) {
    if ($p.mirror -ne 'same') { $failures += "primary at $($p.physical): mirror $($p.mirror)" }
}
if ($mirrors.Count -ne $primaries.Count) {
    $failures += "$($primaries.Count) primaries but $($mirrors.Count) mirrors"
}
# The case is a multi-region catalog only if Windows started a second
# primary region; without one it proves nothing about region rules.
$second = @($primaries | Where-Object { [uint64]$_.physical -ge 2 * $REGION })
$last = ($primaries | Measure-Object -Property physical -Maximum).Maximum
if ($second.Count -eq 0) {
    throw "no primary copy at or past 512 KiB (the last at $last); $Count descriptors did not fill a region"
}
# Every descriptor Windows wrote is in exactly one primary, as Windows reads it.
$found = @{}
foreach ($p in $primaries) {
    foreach ($ace in @($p.descriptor.dacl.aces)) {
        if ($null -eq $ace -or $null -eq $ace.sid -or -not $windows.ContainsKey($ace.sid)) { continue }
        if ($found.ContainsKey($ace.sid)) {
            $failures += "$($ace.sid): in the primaries at $($found[$ace.sid]) and $($p.physical)"
        }
        $found[$ace.sid] = $p.physical
        if ($ace.bytes -ne $windows[$ace.sid]) {
            $failures += "$($ace.sid): catalog ACE $($ace.bytes), Windows $($windows[$ace.sid])"
        }
    }
}
foreach ($sid in $windows.Keys) {
    if (-not $found.ContainsKey($sid)) { $failures += "${sid}: Windows stored it; no primary holds it" }
}
$imageAfter = (Get-FileHash -LiteralPath $img -Algorithm SHA256).Hash
if ($imageAfter -ne $imageBefore) { $failures += 'the image changed while the CLI read it' }

[ordered]@{
    count = $Count
    copies = $copies.Count
    primaries = $primaries.Count
    second_region_primaries = $second.Count
    last_primary = $last
    failures = $failures
} | ConvertTo-Json -Depth 4 |
    Set-Content -Path (Join-Path $OutDir 'acl-catalog-windows.json') -Encoding utf8
foreach ($f in $failures | Select-Object -First 40) { Write-Host "CATALOG $f" }
if ($failures.Count -gt 0) { throw "$($failures.Count) catalog disagreement(s) with Windows" }
Write-Host "acl-catalog-windows-oracle: $($windows.Count) descriptors, $($second.Count) primaries past 512 KiB, all as Windows stored them"
} finally {
    # Only this run's own image: detach it if still attached, then delete it.
    $attached = $null
    try { $attached = Get-DiskImage -ImagePath $img -ErrorAction Stop } catch { }
    if ($null -ne $attached -and $attached.Attached) {
        Dismount-DiskImage -ImagePath $img | Out-Null
    }
    if (Test-Path -LiteralPath $img) { Remove-Item -LiteralPath $img -Force }
    if (Test-Path -LiteralPath $img) { throw "the oracle image $img was not deleted" }
}
