# Positive and negative controls for the ACL oracle's stored-to-logical
# INHERITED_ACE transformation (acl-logical-flags.ps1), over ACEs captured by
# the Windows oracle in run 38779001503: raw GetFileSecurityW, on-disk $SDS,
# Get-Acl and `fs acl show`, kept verbatim in
# tests/fixtures/acl-oracle/run-38779001503-aces.json. No image or CLI is used.
#
# The transformation must accept the observed raw-flags-0 versus Get-Acl-16
# pairs, and must refuse a wrong mask, SID or type, any other flag bit, a
# different ACE, a reordered native list and a CLI byte that was changed.
#
# Usage: acl-logical-flags-controls.ps1
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'acl-logical-flags.ps1')
$fixture = Get-Content -Raw (Join-Path $PSScriptRoot '../fixtures/acl-oracle/run-38779001503-aces.json') | ConvertFrom-Json
$fails = 0
$checks = 0
function Expect([string]$Label, $Want, $Got) {
    $script:checks++
    if ("$Want" -ceq "$Got") { Write-Host "ok   $Label" }
    else { Write-Host "FAIL $Label`: want '$Want', got '$Got'"; $script:fails++ }
}
function Copy-Ace($Ace, [hashtable]$Change) {
    $copy = [pscustomobject]@{ index = $Ace.index; type = $Ace.type; flags = $Ace.flags
        mask = $Ace.mask; sid = $Ace.sid; bytes = $Ace.bytes }
    foreach ($k in $Change.Keys) { $copy.$k = $Change[$k] }
    return $copy
}
# The oracle's grade of one CLI ACE against Get-Acl, with the raw native ACE
# as the only authority for the transformation.
function Grade($Native, $Logical, $Cli) {
    $bit = Get-AclOracleLogicalBit $Native $Logical
    $l = Get-AclOracleLogicalAce ([int]$Cli.flags) "$($Cli.bytes)" $bit
    return ($l.flags -eq [int]$Logical.flags -and $l.bytes -ceq "$($Logical.bytes)" -and
        $null -ne $Native -and [int]$Cli.flags -eq [int]$Native.flags -and "$($Cli.bytes)" -ceq "$($Native.bytes)")
}

$inh = $fixture.cases.'/inherited.txt'
$exp = $fixture.cases.'/explicit.txt'
Expect 'fixture inherited ACE count' 3 @($inh.native).Count
Expect 'fixture explicit ACE count' 4 @($exp.native).Count

# The captured evidence itself: native and stored agree byte for byte, the CLI
# reports the stored bytes, and Get-Acl alone carries INHERITED_ACE.
for ($i = 0; $i -lt 3; $i++) {
    Expect "inherited[$i] native flags" 0 $inh.native[$i].flags
    Expect "inherited[$i] Get-Acl flags" 16 $inh.getAcl[$i].flags
    Expect "inherited[$i] stored bytes = native" $inh.native[$i].bytes $inh.stored[$i].bytes
    Expect "inherited[$i] cli bytes = stored" $inh.stored[$i].bytes $inh.cli[$i].bytes
}

# Positive: each observed pair takes exactly 0x10 and the CLI grades equal.
for ($i = 0; $i -lt 3; $i++) {
    Expect "inherited[$i] bit" 16 (Get-AclOracleLogicalBit $inh.native[$i] $inh.getAcl[$i])
    $l = Get-AclOracleLogicalAce ([int]$inh.cli[$i].flags) $inh.cli[$i].bytes 16
    Expect "inherited[$i] logical bytes = Get-Acl" $inh.getAcl[$i].bytes $l.bytes
    Expect "inherited[$i] grade" $true (Grade $inh.native[$i] $inh.getAcl[$i] $inh.cli[$i])
}
# Positive: no INHERITED_ACE in Get-Acl means no transformation.
for ($i = 0; $i -lt 4; $i++) {
    Expect "explicit[$i] bit" 0 (Get-AclOracleLogicalBit $exp.native[$i] $exp.getAcl[$i])
    Expect "explicit[$i] grade" $true (Grade $exp.native[$i] $exp.getAcl[$i] $exp.cli[$i])
}

# Negative: identity, type, mask and flags must agree for the bit to apply.
$n0 = $inh.native[0]; $w0 = $inh.getAcl[0]; $c0 = $inh.cli[0]
Expect 'wrong mask' 0 (Get-AclOracleLogicalBit (Copy-Ace $n0 @{ mask = 1179817 }) $w0)
Expect 'wrong SID' 0 (Get-AclOracleLogicalBit (Copy-Ace $n0 @{ sid = 'S-1-5-18' }) $w0)
Expect 'wrong type' 0 (Get-AclOracleLogicalBit (Copy-Ace $n0 @{ type = 1 }) $w0)
Expect 'native carries another flag bit' 0 (Get-AclOracleLogicalBit (Copy-Ace $n0 @{ flags = 0x01 }) $w0)
Expect 'Get-Acl carries another flag bit' 0 (Get-AclOracleLogicalBit $n0 (Copy-Ace $w0 @{ flags = 0x11 }))
Expect 'native already inherited' 0 (Get-AclOracleLogicalBit (Copy-Ace $n0 @{ flags = 0x10 }) $w0)
Expect 'missing native ACE' 0 (Get-AclOracleLogicalBit $null $w0)
Expect 'missing native ACE grade' $false (Grade $null $w0 $c0)
Expect 'different ACE' 0 (Get-AclOracleLogicalBit $inh.native[1] $w0)
$reversed = @($inh.native[2], $inh.native[1], $inh.native[0])
Expect 'mismatched order [0]' 0 (Get-AclOracleLogicalBit $reversed[0] $inh.getAcl[0])
Expect 'mismatched order [2]' 0 (Get-AclOracleLogicalBit $reversed[2] $inh.getAcl[2])
Expect 'mismatched order [0] grade' $false (Grade $reversed[0] $inh.getAcl[0] $inh.cli[0])

# Negative: the transformation never covers a changed CLI byte.
$maskByte = Copy-Ace $c0 @{ bytes = $c0.bytes.Substring(0, 8) + 'fe' + $c0.bytes.Substring(10) }
Expect 'changed CLI mask byte grade' $false (Grade $n0 $w0 $maskByte)
$flagByte = Copy-Ace $c0 @{ flags = 0x02; bytes = '0002' + $c0.bytes.Substring(4) }
Expect 'changed CLI flags byte grade' $false (Grade $n0 $w0 $flagByte)
$sidByte = Copy-Ace $c0 @{ bytes = $c0.bytes.Substring(0, $c0.bytes.Length - 2) + '21' }
Expect 'changed CLI SID byte grade' $false (Grade $n0 $w0 $sidByte)
$cliInherited = Copy-Ace $c0 @{ flags = 0x10; bytes = '0010' + $c0.bytes.Substring(4) }
Expect 'CLI reporting the logical bit grade' $false (Grade $n0 $w0 $cliInherited)

if ($checks -lt 47) { Write-Host "FAIL only $checks checks ran"; $fails++ }
if ($fails -gt 0) { Write-Host "acl-logical-flags-controls: $fails of $checks checks failed"; exit 1 }
Write-Host "acl-logical-flags-controls: all $checks checks passed"
