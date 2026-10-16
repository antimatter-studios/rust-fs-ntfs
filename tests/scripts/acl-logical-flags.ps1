# The one stored-to-logical transformation the ACL oracle may apply when it
# grades `fs acl show` against Get-Acl. The CLI reports each ACE's flags as
# stored (src/cli/ntfs/acl.rs: "reported as stored ... raw type and flags";
# nothing is evaluated or canonicalised), and promises no logical view. Get-Acl
# has been observed reporting INHERITED_ACE (0x10) on ACEs that both
# GetFileSecurityW and the on-disk $SDS store without it. That bit is therefore
# applied only where this run's own raw GetFileSecurityW ACE, paired by index,
# is the same ACE as Get-Acl's (type, mask and SID agree) and its flags differ
# from Get-Acl's by 0x10 alone. Nothing here reads the CLI's output.
#
# Dot-sourced by acl-show-windows-oracle.ps1 and by the controls in
# acl-logical-flags-controls.ps1, so both exercise this exact predicate.

$INHERITED_ACE = 0x10

# Returns 0x10 when the raw native ACE $Native and the Get-Acl ACE $Logical are
# the same ACE differing only by INHERITED_ACE, otherwise 0.
function Get-AclOracleLogicalBit($Native, $Logical) {
    if ($null -eq $Native -or $null -eq $Logical) { return 0 }
    if ($Native.type -ne $Logical.type) { return 0 }
    if ("$($Native.mask)" -ne "$($Logical.mask)") { return 0 }
    if ($Native.sid -ne $Logical.sid) { return 0 }
    if (([int]$Logical.flags -band $INHERITED_ACE) -eq 0) { return 0 }
    if ([int]$Native.flags -ne ([int]$Logical.flags -band (-bnot $INHERITED_ACE))) { return 0 }
    return $INHERITED_ACE
}

# Applies $Bit to an ACE's stored flags and to the flags byte of its hex
# bytes, returning both. With $Bit 0 the stored values come back unchanged.
function Get-AclOracleLogicalAce([int]$Flags, [string]$Bytes, [int]$Bit) {
    $logicalFlags = $Flags -bor $Bit
    $logicalBytes = $Bytes
    if ($Bit -ne 0 -and $Bytes.Length -ge 4) {
        $logicalBytes = $Bytes.Substring(0, 2) + $logicalFlags.ToString('x2') + $Bytes.Substring(4)
    }
    return [pscustomobject]@{ flags = $logicalFlags; bytes = $logicalBytes }
}
