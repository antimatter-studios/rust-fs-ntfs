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

function Ace-Access($ace) {
    # Allow and deny are decoded against $rightsMap; every other type stays null.
    $access = [ordered]@{ mask = $null; rights = $null; unknown_mask = $null; sid = $null }
    if ([int]$ace.AceType -le 1 -and $ace -is [System.Security.AccessControl.KnownAce]) {
        $mask = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$ace.AccessMask), 0)
        $known = [uint32]0
        foreach ($v in $rightsMap.Values) { $known = $known -bor $v }
        $access.mask = $mask
        $access.rights = @($rightsMap.Keys | Where-Object { ($mask -band $rightsMap[$_]) -ne 0 })
        $access.unknown_mask = $mask -band (-bnot $known) -band [uint32]::MaxValue
        $access.sid = $ace.SecurityIdentifier.Value
    }
    return $access
}

function Descriptor-Manifest([byte[]]$bytes) {
    # The independent Windows decoder grades the descriptor read from $SDS.
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor($bytes, 0)
    $aces = @()
    foreach ($ace in $sd.DiscretionaryAcl) {
        $buf = New-Object byte[] $ace.BinaryLength
        $ace.GetBinaryForm($buf, 0)
        $access = Ace-Access $ace
        # INHERITED_ACE (0x10) is kept apart so an inherited-flag disagreement
        # between the native and stored descriptors is named, not buried in flags.
        $aces += [ordered]@{ index = $aces.Count; type = [int]$ace.AceType
            flags = [int]$ace.AceFlags; inherited = (([int]$ace.AceFlags -band 0x10) -ne 0)
            mask = $access.mask; rights = $access.rights
            unknown_mask = $access.unknown_mask; sid = $access.sid; bytes = (Hex $buf) }
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

function Write-ZeroMaskDacl([string]$file, $spec) {
    # Each ACE is built as a CommonAce with its literal mask, so a zero mask is
    # handed to SetFileSecurityW as written rather than through an SDDL parser.
    $acl = New-Object System.Security.AccessControl.RawAcl(2, $spec.Count)
    foreach ($s in $spec) {
        $q = if ($s.type -eq 0) { [System.Security.AccessControl.AceQualifier]::AccessAllowed }
            else { [System.Security.AccessControl.AceQualifier]::AccessDenied }
        $ace = New-Object System.Security.AccessControl.CommonAce(
            [System.Security.AccessControl.AceFlags]::None, $q, [int]$s.mask,
            (New-Object System.Security.Principal.SecurityIdentifier($s.sid)), $false, $null)
        $acl.InsertAce($acl.Count, $ace)
    }
    $flags = [System.Security.AccessControl.ControlFlags]::DiscretionaryAclPresent -bor
        [System.Security.AccessControl.ControlFlags]::DiscretionaryAclProtected
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor($flags, $null, $null, $null, $acl)
    $bytes = New-Object byte[] $sd.BinaryLength
    $sd.GetBinaryForm($bytes, 0)
    if (-not [AclOracle.Native]::SetFileSecurityW($file, $DACL_ONLY, $bytes)) {
        throw "SetFileSecurityW $file (zero-mask) failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
}

function Write-RawSddl([string]$file, [string]$sddl) {
    # RawSecurityDescriptor keeps the ACE order the SDDL gives; Set-Acl would
    # pass it through CommonSecurityDescriptor first.
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor($sddl)
    $bytes = New-Object byte[] $sd.BinaryLength
    $sd.GetBinaryForm($bytes, 0)
    if (-not [AclOracle.Native]::SetFileSecurityW($file, $DACL_ONLY, $bytes)) {
        throw "SetFileSecurityW $file ($sddl) failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
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
    # Generic bits, and bits no file right names, so the oracle grades both the
    # names and the unknown remainder the CLI must keep.
    '/generic.txt'   = 'O:BAG:SYD:PAI(A;;GR;;;WD)(D;;0x0000FE00;;;BG)(A;;FA;;;SY)'
}
# Order cases: a DACL written through SetFileSecurityW from a raw descriptor,
# which Windows stores in the order given. Allow precedes deny here, which is
# not Windows canonical order, and every ACE differs in type, mask and SID, so
# a CLI that sorts, reverses, drops or repeats an ACE disagrees by index.
$orderCases = [ordered]@{
    '/order.txt' = 'D:P(A;;0x1200a9;;;BU)(D;;FW;;;BG)(A;;FA;;;SY)(D;;0x00010000;;;WD)'
}
# Zero-mask cases: a present DACL holding an allow and a deny ACE whose access
# mask is 0, beside full-access grants that keep the file usable. The two zero
# ACEs have distinct types and SIDs, so a CLI that drops either one, rewrites
# its mask or reports the DACL as empty is named by index.
$zeroMaskCases = [ordered]@{
    '/zero-mask.txt' = @(
        [ordered]@{ type = 0; mask = 0; sid = 'S-1-5-32-545' }
        [ordered]@{ type = 1; mask = 0; sid = 'S-1-5-32-546' }
        [ordered]@{ type = 0; mask = 0x1F01FF; sid = 'S-1-5-18' }
        [ordered]@{ type = 0; mask = 0x1F01FF; sid = 'S-1-5-32-544' }
    )
}
# Zero-ACE cases: the DACL state each one asks for, accepted only once Windows
# reads it back. 'null' is DACL-present with no ACL; 'empty' is a protected
# ACL with no ACEs.
$zeroAce = [ordered]@{
    '/null-dacl.txt'  = 'null'
    '/empty-dacl.txt' = 'empty'
}
# Each name the CLI may print, bound to the value Windows itself defines for
# it: the FileSystemRights enum for the file and standard rights, and the
# documented generic and special bits that enum does not carry.
$fsr = [System.Security.AccessControl.FileSystemRights]
$rightsMap = [ordered]@{
    FILE_READ_DATA         = [uint32]$fsr::ReadData
    FILE_WRITE_DATA        = [uint32]$fsr::WriteData
    FILE_APPEND_DATA       = [uint32]$fsr::AppendData
    FILE_READ_EA           = [uint32]$fsr::ReadExtendedAttributes
    FILE_WRITE_EA          = [uint32]$fsr::WriteExtendedAttributes
    FILE_EXECUTE           = [uint32]$fsr::ExecuteFile
    FILE_DELETE_CHILD      = [uint32]$fsr::DeleteSubdirectoriesAndFiles
    FILE_READ_ATTRIBUTES   = [uint32]$fsr::ReadAttributes
    FILE_WRITE_ATTRIBUTES  = [uint32]$fsr::WriteAttributes
    DELETE                 = [uint32]$fsr::Delete
    READ_CONTROL           = [uint32]$fsr::ReadPermissions
    WRITE_DAC              = [uint32]$fsr::ChangePermissions
    WRITE_OWNER            = [uint32]$fsr::TakeOwnership
    SYNCHRONIZE            = [uint32]$fsr::Synchronize
    ACCESS_SYSTEM_SECURITY = [uint32]0x01000000
    MAXIMUM_ALLOWED        = [uint32]0x02000000
    GENERIC_ALL            = [uint32]0x10000000
    GENERIC_EXECUTE        = [uint32]0x20000000
    GENERIC_WRITE          = [uint32]0x40000000
    # A hex literal with the top bit set parses as a negative Int32, which
    # [uint32] refuses, so this one bit is written as its decimal value.
    GENERIC_READ           = [uint32]2147483648
}
$rightsCompared = 0

$expected = [ordered]@{}
$captures = [ordered]@{}
$evidence = [ordered]@{}
foreach ($path in @($cases.Keys) + @($orderCases.Keys) + @($zeroMaskCases.Keys) + @($zeroAce.Keys)) {
    $file = "${letter}:" + ($path -replace '/', '\')
    Set-Content -Path $file -Value 'acl oracle' -Encoding ascii
    if ($orderCases.Contains($path)) {
        Write-RawSddl $file $orderCases[$path]
        $raw = Read-RawDescriptor $file
        $sddl = $raw.GetSddlForm([System.Security.AccessControl.AccessControlSections]::All)
    } elseif ($zeroMaskCases.Contains($path)) {
        Write-ZeroMaskDacl $file $zeroMaskCases[$path]
        $raw = Read-RawDescriptor $file
        $sddl = $raw.GetSddlForm([System.Security.AccessControl.AccessControlSections]::All)
    } elseif ($zeroAce.Contains($path)) {
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
        $access = Ace-Access $ace
        $aces += [ordered]@{
            index = $index
            type  = [int]$ace.AceType
            flags = [int]$ace.AceFlags
            mask  = $access.mask
            rights = $access.rights
            unknown_mask = $access.unknown_mask
            sid   = $access.sid
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
# The native descriptor Windows returns and the on-disk MFT/$SDS descriptor are
# compared for the same file identity: control, raw bytes and each ACE's flags
# and INHERITED_ACE bit. Missing evidence or a disagreement fails by case.
$evidenceFailures = 0
$evidenceCases = @()
foreach ($path in $captures.Keys) {
    $item = $evidence[$path]
    $diffs = @()
    if ($null -eq $item -or $null -eq $item.nativeLogical) { $diffs += 'native descriptor missing' }
    if ($null -eq $captures[$path].Descriptor -or $captures[$path].Descriptor.Length -eq 0) {
        $diffs += 'stored descriptor missing'
    }
    if ($captures[$path].FileReference -eq 0) { $diffs += 'file reference missing' }
    if ($diffs.Count -eq 0) {
        $native = $item.nativeLogical
        $disk = $item.stored
        # GetFileSecurityW was asked for owner, group and DACL only, so the SACL
        # control bits and the self-relative offsets it serialises differ from
        # the stored descriptor without any disagreement. The comparison is of
        # like parts: the DACL control bits, the owner and group SIDs, and each
        # ACE's own bytes. Whole-descriptor byte identity is recorded, not graded.
        $daclControl = 0x0004 -bor 0x0008 -bor 0x0400 -bor 0x1000
        if (($native.control -band $daclControl) -ne ($disk.control -band $daclControl)) {
            $diffs += "dacl control: native '$($native.control -band $daclControl)', stored '$($disk.control -band $daclControl)'"
        }
        foreach ($field in 'owner', 'group') {
            if ("$($native[$field])" -ne "$($disk[$field])") {
                $diffs += "${field}: native '$($native[$field])', stored '$($disk[$field])'"
            }
        }
        if ($native.dacl.state -ne $disk.dacl.state) {
            $diffs += "dacl.state: native '$($native.dacl.state)', stored '$($disk.dacl.state)'"
        }
        if ($native.dacl.aces.Count -ne $disk.dacl.aces.Count) {
            $diffs += "ace count: native $($native.dacl.aces.Count), stored $($disk.dacl.aces.Count)"
        } else {
            for ($i = 0; $i -lt $native.dacl.aces.Count; $i++) {
                foreach ($field in 'flags', 'inherited', 'bytes') {
                    if ("$($native.dacl.aces[$i][$field])" -ne "$($disk.dacl.aces[$i][$field])") {
                        $diffs += "ace[$i].${field}: native '$($native.dacl.aces[$i][$field])', stored '$($disk.dacl.aces[$i][$field])'"
                    }
                }
            }
        }
    }
    $item.nativeVsStored = [ordered]@{
        fileReference = $item.fileReference; securityId = $item.securityId
        descriptorBytesIdentical = ("$($item.nativeLogical.bytes)" -eq "$($item.stored.bytes)")
        agree = ($diffs.Count -eq 0); differences = $diffs }
    foreach ($d in $diffs) { Write-Host "EVIDENCE ${path} native-vs-stored ${d}" }
    if ($diffs.Count -gt 0) { $evidenceFailures += $diffs.Count; $evidenceCases += $path }
}
$evidence | ConvertTo-Json -Depth 12 |
    Set-Content -Path (Join-Path $OutDir 'acl-show-stored-vs-logical.json') -Encoding utf8

# The ACE order is graded as one sequence of type, mask and SID keys, so a
# reordered, dropped or repeated ACE is named as an order failure, not only as
# per-index field differences.
function Get-AceOrderKeys($aces) {
    return @(@($aces) | ForEach-Object { "$($_.type)/$($_.mask)/$($_.sid)" })
}
function Get-AceOrderDiff([string[]]$want, [string[]]$got) {
    $diffs = @()
    if ($want.Count -ne $got.Count) { $diffs += "order count: Windows $($want.Count), got $($got.Count)" }
    $n = [Math]::Max($want.Count, $got.Count)
    for ($i = 0; $i -lt $n; $i++) {
        $w = if ($i -lt $want.Count) { $want[$i] } else { '<none>' }
        $g = if ($i -lt $got.Count) { $got[$i] } else { '<none>' }
        if ($w -ne $g) { $diffs += "order[$i]: Windows '$w', got '$g'" }
    }
    return ,$diffs
}
$orderEvidence = [ordered]@{}
foreach ($path in $orderCases.Keys) {
    $native = Get-AceOrderKeys $evidence[$path].nativeLogical.dacl.aces
    $storedOrder = Get-AceOrderKeys $stored[$path].dacl.aces
    # The case proves nothing unless Windows kept an allow ahead of a deny, so a
    # CLI that put the ACEs in canonical order would be caught.
    $types = @($evidence[$path].nativeLogical.dacl.aces | ForEach-Object { [int]$_.type })
    $firstDeny = [Array]::IndexOf($types, 1)
    if ($native.Count -lt 4 -or $firstDeny -le 0 -or $types[0] -ne 0) {
        throw "${path}: Windows stored ACE types '$($types -join ',')'; the case needs an allow ahead of a deny"
    }
    if (@($native | Select-Object -Unique).Count -ne $native.Count) {
        throw "${path}: Windows stored ACEs that share type, mask and SID; order cannot be graded"
    }
    $storedDiff = Get-AceOrderDiff $native $storedOrder
    if ($storedDiff.Count -gt 0) { throw "${path}: native and stored ACE order differ: $($storedDiff -join '; ')" }
    # Negative controls: each mutation of the native order goes through the
    # same grading the CLI output does, and each must be refused.
    $canonical = @(@($native | Where-Object { $_ -like '1/*' }) + @($native | Where-Object { $_ -notlike '1/*' }))
    $controls = [ordered]@{
        reversed   = @($native[($native.Count - 1)..0])
        canonical  = $canonical
        dropped    = @($native[0..($native.Count - 2)])
        duplicated = @(@($native[0]) + @($native[0..($native.Count - 2)]))
        swapped    = @(@($native[1], $native[0]) + @($native[2..($native.Count - 1)]))
    }
    foreach ($name in $controls.Keys) {
        if ((Get-AceOrderDiff $native $controls[$name]).Count -eq 0) {
            throw "${path}: the order grade accepted the '$name' control"
        }
    }
    $orderEvidence[$path] = [ordered]@{ native = $native; stored = $storedOrder
        controls = @($controls.Keys) }
}
$orderEvidence | ConvertTo-Json -Depth 6 |
    Set-Content -Path (Join-Path $OutDir 'acl-show-order-evidence.json') -Encoding utf8

# Zero-mask grade: the DACL must stay present with every ACE, and each ACE
# Windows stored with mask 0 must come back at its index with the same type,
# SID, mask 0, no decoded rights and unknown mask 0.
function Get-ZeroMaskKey($ace) {
    if ($null -eq $ace) { return '<none>' }
    return "$($ace.type)/$($ace.mask)/$($ace.sid)/rights=[$(@($ace.rights) -join ',')]/unknown=$($ace.unknown_mask)"
}
function Get-ZeroMaskDiff($wantAces, [string]$gotState, $gotAces) {
    $want = @($wantAces); $got = @($gotAces)
    $diffs = @()
    if ($gotState -ne 'present') { $diffs += "dacl.state: Windows 'present', got '$gotState'" }
    if ($want.Count -ne $got.Count) { $diffs += "ace count: Windows $($want.Count), got $($got.Count)" }
    for ($i = 0; $i -lt $want.Count; $i++) {
        if ($null -eq $want[$i].mask -or [uint64]$want[$i].mask -ne 0) { continue }
        $g = if ($i -lt $got.Count) { $got[$i] } else { $null }
        if ((Get-ZeroMaskKey $want[$i]) -ne (Get-ZeroMaskKey $g)) {
            $diffs += "zero-mask ace[$i]: Windows '$(Get-ZeroMaskKey $want[$i])', got '$(Get-ZeroMaskKey $g)'"
        }
    }
    return ,$diffs
}
$zeroMaskEvidence = [ordered]@{}
foreach ($path in $zeroMaskCases.Keys) {
    $native = @($evidence[$path].nativeLogical.dacl.aces)
    $nativeRaw = "sddl '$($expected[$path].sddl)', bytes $($evidence[$path].nativeLogical.bytes)"
    # Windows must itself have kept both zero-mask ACEs; if it normalised them
    # away the case fails here with the raw native descriptor as evidence.
    $zero = @($native | Where-Object { $null -ne $_.mask -and [uint64]$_.mask -eq 0 })
    $zeroKeys = @($zero | ForEach-Object { "$($_.index)/$($_.type)/$($_.sid)" })
    $wantKeys = @('0/0/S-1-5-32-545', '1/1/S-1-5-32-546')
    if ($native.Count -ne 4 -or "$zeroKeys" -ne "$wantKeys") {
        throw "${path}: Windows did not retain the zero-mask ACEs ('$zeroKeys', $($native.Count) ACEs); native $nativeRaw"
    }
    foreach ($z in $zero) {
        if (@($z.rights).Count -ne 0 -or [uint64]$z.unknown_mask -ne 0) {
            throw "${path}: zero-mask ACE $($z.index) decoded rights '$(@($z.rights) -join ',')' unknown $($z.unknown_mask); native $nativeRaw"
        }
    }
    if ("$($expected[$path].dacl.state)" -ne 'present') {
        throw "${path}: Windows read back DACL state '$($expected[$path].dacl.state)'; native $nativeRaw"
    }
    $storedDiff = Get-ZeroMaskDiff $native "$($stored[$path].dacl.state)" $stored[$path].dacl.aces
    if ($storedDiff.Count -gt 0) { throw "${path}: native and stored zero-mask ACEs differ: $($storedDiff -join '; ')" }
    # Negative controls through the same grade the CLI output takes; each must
    # be refused for its own reason.
    $copy = { param($a) $c = [ordered]@{}; foreach ($k in $a.Keys) { $c[$k] = $a[$k] }; $c }
    $maskOne = @($native | ForEach-Object { & $copy $_ })
    $maskOne[0].mask = 1; $maskOne[0].rights = @('FILE_READ_DATA')
    $controls = [ordered]@{
        dropZero  = @{ state = 'present'; aces = @($native[1..($native.Count - 1)]); reason = 'ace count:' }
        maskOne   = @{ state = 'present'; aces = $maskOne; reason = 'zero-mask ace[0]:' }
        asEmpty   = @{ state = 'empty'; aces = @(); reason = 'dacl.state:' }
    }
    foreach ($name in $controls.Keys) {
        $d = Get-ZeroMaskDiff $native $controls[$name].state $controls[$name].aces
        if (@($d | Where-Object { $_.StartsWith($controls[$name].reason) }).Count -eq 0) {
            throw "${path}: the zero-mask grade did not refuse the '$name' control for '$($controls[$name].reason)': '$($d -join '; ')'"
        }
    }
    $zeroMaskEvidence[$path] = [ordered]@{ native = @($native | ForEach-Object { Get-ZeroMaskKey $_ })
        stored = @(@($stored[$path].dacl.aces) | ForEach-Object { Get-ZeroMaskKey $_ })
        nativeSddl = $expected[$path].sddl; nativeBytes = $evidence[$path].nativeLogical.bytes
        controls = @($controls.Keys) }
}

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
        foreach ($field in 'index', 'type', 'mask', 'unknown_mask', 'sid') {
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
        $wantRights = if ($null -eq $want.dacl.aces[$i].rights) { '<null>' } else { @($want.dacl.aces[$i].rights) -join ',' }
        $gotRights = if ($null -eq $gotAces[$i].rights) { '<null>' } else { @($gotAces[$i].rights) -join ',' }
        Check "$path ace[$i].rights" $wantRights $gotRights
        if ($null -ne $want.dacl.aces[$i].mask -and $want.dacl.aces[$i].mask -ne 0) { $rightsCompared++ }
    }
    if ($orderCases.Contains($path)) {
        $orderDiff = Get-AceOrderDiff $orderEvidence[$path].native (Get-AceOrderKeys $gotAces)
        foreach ($d in $orderDiff) { Write-Host "FAIL $path $d" }
        $failures += $orderDiff.Count
        $orderEvidence[$path].cli = Get-AceOrderKeys $gotAces
    }
    if ($zeroMaskCases.Contains($path)) {
        $zeroDiff = Get-ZeroMaskDiff $evidence[$path].nativeLogical.dacl.aces "$($got.dacl.state)" $gotAces
        foreach ($d in $zeroDiff) { Write-Host "FAIL $path $d" }
        $failures += $zeroDiff.Count
        $zeroMaskEvidence[$path].cli = @($gotAces | ForEach-Object { Get-ZeroMaskKey $_ })
    }
    $disk = $stored[$path]
    foreach ($field in 'control', 'owner', 'group', 'bytes') {
        Check "$path stored.$field" $disk[$field] $got.$field
    }
    Check "$path stored.dacl.state" $disk.dacl.state $got.dacl.state
    Check "$path stored.ace count" $disk.dacl.aces.Count $gotAces.Count
    for ($i = 0; $i -lt $disk.dacl.aces.Count; $i++) {
        foreach ($field in 'index', 'type', 'flags', 'mask', 'unknown_mask', 'sid', 'bytes') {
            Check "$path stored.ace[$i].$field" $disk.dacl.aces[$i][$field] $gotAces[$i].$field
        }
        $diskRights = if ($null -eq $disk.dacl.aces[$i].rights) { '<null>' } else { @($disk.dacl.aces[$i].rights) -join ',' }
        $gotRights = if ($null -eq $gotAces[$i].rights) { '<null>' } else { @($gotAces[$i].rights) -join ',' }
        Check "$path stored.ace[$i].rights" $diskRights $gotRights
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
$orderEvidence | ConvertTo-Json -Depth 6 |
    Set-Content -Path (Join-Path $OutDir 'acl-show-order-evidence.json') -Encoding utf8
$zeroMaskEvidence | ConvertTo-Json -Depth 6 |
    Set-Content -Path (Join-Path $OutDir 'acl-show-zero-mask-evidence.json') -Encoding utf8
$expectedStates = 'present', 'present', 'present', 'present', 'present', 'null', 'empty'
$seenStates = @($expected.Values | ForEach-Object { $_.dacl.state })
if ($compared -ne 7 -or "$seenStates" -ne "$expectedStates") {
    throw "compared $compared cases with states '$seenStates'; need seven: '$expectedStates'"
}
if (@($zeroMaskEvidence.Values | Where-Object { $null -ne $_.cli }).Count -ne $zeroMaskCases.Count) {
    throw "the zero-mask ACEs of $($zeroMaskCases.Count) case(s) were not graded against the CLI"
}
if (@($orderEvidence.Values | Where-Object { $null -ne $_.cli }).Count -ne $orderCases.Count) {
    throw "the ACE order of $($orderCases.Count) order case(s) was not graded against the CLI"
}
if ($rightsCompared -eq 0) { throw "no allow or deny ACE with a nonzero mask was compared" }
if ($failures -gt 0) { throw "$failures field(s) differ from Windows" }
if ($evidenceFailures -gt 0) {
    throw "$evidenceFailures native-versus-stored disagreement(s) in: $($evidenceCases -join ', ')"
}
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
