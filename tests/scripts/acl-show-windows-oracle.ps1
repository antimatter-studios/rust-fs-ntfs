# Windows oracle for `fs acl show`: Windows formats a fixed VHD, writes files
# with known ACLs, and reads them back through its own security APIs. The
# expected manifest comes only from Windows (Get-Acl and the .NET
# RawSecurityDescriptor over GetSecurityDescriptorBinaryForm); the CLI is then
# run against the detached image and its JSON is compared field by field.
#
# Usage: acl-show-windows-oracle.ps1 -Cli <rust-fs-ntfs.exe> -OutDir <dir>
param(
    [Parameter(Mandatory = $true)][string]$Cli,
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$img = Join-Path (Resolve-Path $OutDir) 'acl-oracle.vhd'
$letter = 'R'

# GetFileSecurityW returns the Windows logical descriptor, which must be
# captured separately from the raw stored descriptor. Get-Acl/Set-Acl use
# CommonSecurityDescriptor, which replaces a NULL DACL with an Everyone ACE, so
# the NULL and empty cases are written and read back through these instead.
Add-Type -Namespace AclOracle -Name Native -MemberDefinition @'
[DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool SetFileSecurityW(string lpFileName, uint SecurityInformation, byte[] pSecurityDescriptor);
[DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool GetFileSecurityW(string lpFileName, uint RequestedInformation, byte[] pSecurityDescriptor, uint nLength, out uint lpnLengthNeeded);
'@
$OWNER_GROUP_DACL = [uint32]0x7
$DACL_ONLY = [uint32]0x4
Add-Type -Path (Join-Path $PSScriptRoot 'acl-stored-windows-capture.cs')
. (Join-Path $PSScriptRoot 'acl-logical-flags.ps1')

function Hex([byte[]]$bytes) {
    return (($bytes | ForEach-Object { $_.ToString('x2') }) -join '')
}

function Ace-Identity($ace) {
    # The mask and SID that pair a Get-Acl ACE with its raw native ACE; both
    # stay null for ACE types that carry neither.
    $identity = [ordered]@{ mask = $null; sid = $null }
    if ($ace -is [System.Security.AccessControl.KnownAce]) {
        $identity.mask = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$ace.AccessMask), 0)
        $identity.sid = $ace.SecurityIdentifier.Value
    }
    return $identity
}

function Descriptor-Manifest([byte[]]$bytes) {
    # The independent Windows decoder grades the descriptor read from $SDS.
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor($bytes, 0)
    $aces = @()
    foreach ($ace in $sd.DiscretionaryAcl) {
        $buf = New-Object byte[] $ace.BinaryLength
        $ace.GetBinaryForm($buf, 0)
        $identity = Ace-Identity $ace
        $aces += [ordered]@{ index = $aces.Count; type = [int]$ace.AceType
            flags = [int]$ace.AceFlags; mask = $identity.mask; sid = $identity.sid
            bytes = (Hex $buf) }
    }
    $present = ($sd.ControlFlags -band
        [System.Security.AccessControl.ControlFlags]::DiscretionaryAclPresent) -ne 0
    $state = if (-not $present) { 'absent' }
        elseif ($null -eq $sd.DiscretionaryAcl) { 'null' }
        elseif ($aces.Count -eq 0) { 'empty' } else { 'present' }
    return [ordered]@{ control = [int]$sd.ControlFlags; owner = $sd.Owner.Value
        group = $sd.Group.Value; bytes = (Hex $bytes)
        dacl = [ordered]@{ state = $state; aces = $aces } }
}

function Read-LogicalBytes([string]$file) {
    $need = [uint32]0
    [void][AclOracle.Native]::GetFileSecurityW($file, $OWNER_GROUP_DACL, $null, 0, [ref]$need)
    $buf = New-Object byte[] $need
    if (-not [AclOracle.Native]::GetFileSecurityW($file, $OWNER_GROUP_DACL, $buf, $need, [ref]$need)) {
        throw "GetFileSecurityW $file failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    return ,$buf
}

function Read-RawDescriptor([string]$file) {
    $bytes = Read-LogicalBytes $file
    return New-Object System.Security.AccessControl.RawSecurityDescriptor($bytes, 0)
}

function Write-Dacl([string]$file, [string]$intent) {
    $flags = [System.Security.AccessControl.ControlFlags]::DiscretionaryAclPresent -bor
        [System.Security.AccessControl.ControlFlags]::DiscretionaryAclProtected
    $dacl = if ($intent -eq 'empty') { New-Object System.Security.AccessControl.RawAcl(2, 0) } else { $null }
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor($flags, $null, $null, $null, $dacl)
    $bytes = New-Object byte[] $sd.BinaryLength
    $sd.GetBinaryForm($bytes, 0)
    if (-not [AclOracle.Native]::SetFileSecurityW($file, $DACL_ONLY, $bytes)) {
        throw "SetFileSecurityW $file ($intent) failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
}

function Invoke-Diskpart([string[]]$lines) {
    $script = Join-Path $env:RUNNER_TEMP ("diskpart-" + [guid]::NewGuid() + '.txt')
    Set-Content -Path $script -Value $lines -Encoding ascii
    $out = diskpart /s $script
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed ($LASTEXITCODE): $out" }
}

try {
Invoke-Diskpart @(
    "create vdisk file=`"$img`" maximum=64 type=fixed",
    "select vdisk file=`"$img`"",
    'attach vdisk',
    'convert mbr',
    'create partition primary',
    'format fs=ntfs quick label=ACLORACLE',
    "assign letter=$letter"
)
$offset = (Get-Partition -DriveLetter $letter).Offset

# Each case: a file and the SDDL Windows applies to it, or $null to keep the
# descriptor Windows itself gives a new file (inherited ACEs from the root).
$cases = [ordered]@{
    '/explicit.txt'  = 'O:BAG:SYD:PAI(D;;FW;;;BG)(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;BU)'
    '/inherited.txt' = $null
}
# Zero-ACE cases: the DACL state each one asks for, accepted only once Windows
# reads it back. 'null' is DACL-present with no ACL; 'empty' is a protected
# ACL with no ACEs.
$zeroAce = [ordered]@{
    '/null-dacl.txt'  = 'null'
    '/empty-dacl.txt' = 'empty'
}
$expected = [ordered]@{}
$captures = [ordered]@{}
$evidence = [ordered]@{}
foreach ($path in @($cases.Keys) + @($zeroAce.Keys)) {
    $file = "${letter}:" + ($path -replace '/', '\')
    Set-Content -Path $file -Value 'acl oracle' -Encoding ascii
    if ($zeroAce.Contains($path)) {
        Write-Dacl $file $zeroAce[$path]
        $raw = Read-RawDescriptor $file
        $sddl = $raw.GetSddlForm([System.Security.AccessControl.AccessControlSections]::All)
    } else {
        if ($null -ne $cases[$path]) {
            $acl = Get-Acl -LiteralPath $file
            $acl.SetSecurityDescriptorSddlForm($cases[$path])
            Set-Acl -LiteralPath $file -AclObject $acl
        }
        $read = Get-Acl -LiteralPath $file
        $raw = New-Object System.Security.AccessControl.RawSecurityDescriptor(
            $read.GetSecurityDescriptorBinaryForm(), 0)
        $sddl = $read.GetSecurityDescriptorSddlForm('All')
    }
    $aces = @()
    $index = 0
    foreach ($ace in $raw.DiscretionaryAcl) {
        $buf = New-Object byte[] $ace.BinaryLength
        $ace.GetBinaryForm($buf, 0)
        $identity = Ace-Identity $ace
        $aces += [ordered]@{
            index = $index
            type  = [int]$ace.AceType
            flags = [int]$ace.AceFlags
            mask  = $identity.mask
            sid   = $identity.sid
            bytes = (($buf | ForEach-Object { $_.ToString('x2') }) -join '')
        }
        $index++
    }
    $daclPresent = ($raw.ControlFlags -band
        [System.Security.AccessControl.ControlFlags]::DiscretionaryAclPresent) -ne 0
    $state = if (-not $daclPresent) { 'absent' }
        elseif ($null -eq $raw.DiscretionaryAcl) { 'null' }
        elseif ($aces.Count -eq 0) { 'empty' } else { 'present' }
    $want = if ($zeroAce.Contains($path)) { $zeroAce[$path] } else { 'present' }
    if ($state -ne $want) { throw "${path}: Windows read back DACL state '$state', the case needs '$want'" }
    $expected[$path] = [ordered]@{
        sddl  = $sddl
        controlFlags = [int]$raw.ControlFlags
        owner = $raw.Owner.Value
        group = $raw.Group.Value
        dacl  = [ordered]@{ state = $state; aces = $aces }
    }
    $captures[$path] = [AclOracle.StoredCapture]::Capture($file)
    $logicalBytes = Read-LogicalBytes $file
    $evidence[$path] = [ordered]@{
        fileReference = $captures[$path].FileReference.ToString('x16')
        securityId = $captures[$path].SecurityId
        fileRecord = (Hex $captures[$path].FileRecord)
        secureRecord = (Hex $captures[$path].SecureRecord)
        nativeLogical = (Descriptor-Manifest $logicalBytes)
    }
}
$evidence | ConvertTo-Json -Depth 12 |
    Set-Content -Path (Join-Path $OutDir 'acl-show-stored-vs-logical.json') -Encoding utf8

Invoke-Diskpart @("select vdisk file=`"$img`"", 'detach vdisk')
$expected | ConvertTo-Json -Depth 8 |
    Set-Content -Path (Join-Path $OutDir 'acl-show-windows-expected.json') -Encoding utf8
$stored = [ordered]@{}
foreach ($path in $captures.Keys) {
    $captures[$path].ReadDetached($img, $offset)
    $stored[$path] = Descriptor-Manifest $captures[$path].Descriptor
    $evidence[$path].sdsOffset = $captures[$path].SdsOffset
    $evidence[$path].stored = $stored[$path]
}
$evidence | ConvertTo-Json -Depth 12 |
    Set-Content -Path (Join-Path $OutDir 'acl-show-stored-vs-logical.json') -Encoding utf8

function Find-Report($json) {
    foreach ($candidate in @($json, $json.data, $json.result, $json.output)) {
        if ($null -ne $candidate -and $candidate.PSObject.Properties.Name -contains 'dacl') {
            return $candidate
        }
    }
    throw "CLI JSON carries no dacl: $($json | ConvertTo-Json -Depth 8 -Compress)"
}

$failures = 0
function Check($label, $want, $got) {
    if ("$want" -ne "$got") {
        Write-Host "FAIL ${label}: Windows '$want', CLI '$got'"
        $script:failures++
    }
}
$compared = 0
$imageBefore = (Get-FileHash -LiteralPath $img -Algorithm SHA256).Hash
foreach ($path in $expected.Keys) {
    $out = & $Cli fs --json --offset $offset $img acl show $path
    if ($LASTEXITCODE -ne 0) { throw "CLI acl show $path exited $LASTEXITCODE" }
    $out | Set-Content -Path (Join-Path $OutDir ("cli" + ($path -replace '[/\\.]', '-') + '.json')) -Encoding utf8
    $got = Find-Report ($out | Out-String | ConvertFrom-Json)
    $want = $expected[$path]
    Check "$path owner" $want.owner $got.owner
    Check "$path group" $want.group $got.group
    Check "$path dacl.state" $want.dacl.state $got.dacl.state
    # A missing property means no ACE array, not a one-element null array.
    $gotAces = if ($null -eq $got.dacl.aces) { @() } else { @($got.dacl.aces) }
    Check "$path ace count" $want.dacl.aces.Count $gotAces.Count
    # Get-Acl reports INHERITED_ACE on ACEs that the raw descriptor, from both
    # GetFileSecurityW and the on-disk $SDS, stores without it. The CLI reports
    # the stored descriptor, so against Get-Acl that one bit is applied as a
    # logical transformation, and only where this run's own native capture of
    # the same ACE demonstrates it: same type, mask and SID, flags differing by
    # 0x10 alone. Every other field and bit is still compared exactly, and the
    # CLI must match the raw native ACE byte for byte.
    $nativeAces = @($evidence[$path].nativeLogical.dacl.aces)
    for ($i = 0; $i -lt $want.dacl.aces.Count; $i++) {
        $w = $want.dacl.aces[$i]
        $n = if ($i -lt $nativeAces.Count) { $nativeAces[$i] } else { $null }
        $logicalBit = Get-AclOracleLogicalBit $n $w
        foreach ($field in 'index', 'type') {
            Check "$path ace[$i].$field" $w[$field] $gotAces[$i].$field
        }
        $gotLogical = Get-AclOracleLogicalAce ([int]$gotAces[$i].flags) "$($gotAces[$i].bytes)" $logicalBit
        $label = if ($logicalBit -ne 0) { 'flags (Get-Acl INHERITED_ACE applied)' } else { 'flags' }
        Check "$path ace[$i].$label" $w.flags $gotLogical.flags
        Check "$path ace[$i].bytes" $w.bytes $gotLogical.bytes
        if ($null -ne $n) {
            foreach ($field in 'flags', 'bytes') {
                Check "$path native.ace[$i].$field" $n[$field] $gotAces[$i].$field
            }
        } else {
            Check "$path native.ace[$i]" 'present' 'missing'
        }
    }
    $disk = $stored[$path]
    foreach ($field in 'control', 'owner', 'group', 'bytes') {
        Check "$path stored.$field" $disk[$field] $got.$field
    }
    Check "$path stored.dacl.state" $disk.dacl.state $got.dacl.state
    Check "$path stored.ace count" $disk.dacl.aces.Count $gotAces.Count
    for ($i = 0; $i -lt $disk.dacl.aces.Count; $i++) {
        foreach ($field in 'index', 'type', 'flags', 'bytes') {
            Check "$path stored.ace[$i].$field" $disk.dacl.aces[$i][$field] $gotAces[$i].$field
        }
    }
    if ($want.dacl.aces.Count -eq 0 -and -not $zeroAce.Contains($path)) {
        throw "${path}: Windows reported no ACEs; the case proves nothing"
    }
    if ($zeroAce.Contains($path) -and $want.dacl.state -ne $zeroAce[$path]) {
        throw "${path}: zero ACEs without the Windows-confirmed '$($zeroAce[$path])' state"
    }
    $compared++
}
$imageAfter = (Get-FileHash -LiteralPath $img -Algorithm SHA256).Hash
if ($imageAfter -ne $imageBefore) { throw "the image changed while the CLI read it" }
$expectedStates = 'present', 'present', 'null', 'empty'
$seenStates = @($expected.Values | ForEach-Object { $_.dacl.state })
if ($compared -ne 4 -or "$seenStates" -ne "$expectedStates") {
    throw "compared $compared cases with states '$seenStates'; need four: '$expectedStates'"
}
if ($failures -gt 0) { throw "$failures field(s) differ from Windows" }
Write-Host "acl-show-windows-oracle: $compared cases match Windows"
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
