# Windows' security descriptors are the ACL oracle, independently of either
# NTFS reader. Dot-sourcing only defines the capture function. The caller
# owns the mount, and supplies the SHA-256 of the untouched raw image, not
# its mounted copy. Get-Acl reads each descriptor through Windows, and
# Windows' own SDDL parser (RawSecurityDescriptor) splits its DACL into ACEs,
# so no locale-dependent account name is recorded or parsed.
function Get-SlateWindowsAcl {
    param(
        [Parameter(Mandatory=$true)] [string]$Root,
        [Parameter(Mandatory=$true)] [string]$Case,
        [Parameter(Mandatory=$true)] [string]$ImageSha256
    )
    if ($ImageSha256 -cnotmatch '^[0-9a-f]{64}$') { throw "$Case has an invalid image SHA-256" }
    # The same walk as the allocation manifest, so the same root.
    if ($Root -cnotmatch '^[A-Za-z]:\\$') { throw "$Root is not a volume root such as 'E:\'" }
    $rootItem = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    $base = $rootItem.FullName.TrimEnd('\', '/')
    # The same root names the read manifest excludes, for the same reason.
    $excluded = @('$AttrDef', '$BadClus', '$Bitmap', '$Boot', '$Extend', '$LogFile',
        '$MFT', '$MFTMirr', '$Secure', '$UpCase', '$Volume', '$Recycle.Bin', 'System Volume Information')
    $sid = [Security.Principal.SecurityIdentifier]
    $protectedFlag = [Security.AccessControl.ControlFlags]::DiscretionaryAclProtected
    $entries = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
    $pending = [Collections.Generic.Stack[string]]::new()
    $pending.Push($rootItem.FullName)
    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        try {
            $children = @(Get-ChildItem -LiteralPath $directory -Force -ErrorAction Stop)
        } catch { throw "Windows cannot enumerate ${directory}: $($_.Exception.Message)" }
        foreach ($item in $children) {
            $relative = '/' + $item.FullName.Substring($base.Length).TrimStart('\', '/').Replace('\', '/')
            if ($directory -eq $rootItem.FullName -and $excluded -contains $item.Name) { continue }
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Windows ACL manifest does not follow reparse point $relative"
            }
            try {
                $acl = Get-Acl -LiteralPath $item.FullName -ErrorAction Stop
                $sddl = $acl.GetSecurityDescriptorSddlForm('Owner, Group, Access')
                $raw = [Security.AccessControl.RawSecurityDescriptor]::new($sddl)
                if ($null -eq $raw.Owner -or $null -eq $raw.Group) { throw 'the descriptor has no owner or no group' }
                # Assigned, not returned from an if, so one ACE stays an array.
                $aces = $null
                if ($null -ne $raw.DiscretionaryAcl) {
                    $aces = @(foreach ($ace in $raw.DiscretionaryAcl) {
                        $known = $ace -is [Security.AccessControl.KnownAce]
                        [ordered]@{
                            type = $ace.AceType.ToString()
                            flags = $ace.AceFlags.ToString()
                            inherited = $ace.IsInherited
                            sid = if ($known) { $ace.SecurityIdentifier.Value } else { $null }
                            access_mask = if ($known) { [BitConverter]::ToUInt32([BitConverter]::GetBytes($ace.AccessMask), 0) } else { $null }
                        }
                    })
                }
                $entry = [ordered]@{
                    path = $relative
                    kind = if ($item.PSIsContainer) { 'directory' } else { 'file' }
                    owner = $acl.GetOwner($sid).Value
                    group = $acl.GetGroup($sid).Value
                    sddl = $sddl
                    dacl_sddl = $acl.GetSecurityDescriptorSddlForm('Access')
                    dacl_protected = (($raw.ControlFlags -band $protectedFlag) -ne 0)
                    aces = $aces
                }
            } catch { throw "Windows cannot read the security descriptor of ${relative}: $($_.Exception.Message)" }
            $entries.Add($relative, $entry)
            if ($item.PSIsContainer) { $pending.Push($item.FullName) }
        }
    }
    [string[]]$paths = @($entries.Keys)
    [Array]::Sort($paths, [StringComparer]::Ordinal)
    [ordered]@{
        schema = 1
        case = $Case
        image_sha256 = $ImageSha256
        oracle = 'Windows Get-Acl (GetNamedSecurityInfo) and RawSecurityDescriptor (ConvertStringSecurityDescriptorToSecurityDescriptor)'
        capabilities = [ordered]@{
            excluded_root_names = $excluded
            exclusion_reason = 'NTFS metadata and Windows mount lifecycle entries'
            sections = 'owner, group and DACL; the SACL needs SeSecurityPrivilege and is outside this manifest'
            sids = 'SIDs only; account names depend on the machine that reads them'
            aces = "every DACL ACE in order; a null aces is a null DACL; sid and access_mask are null for an ACE Windows cannot attribute to one SID"
            unreadable = 'a descriptor Windows refuses to return fails by path; never omitted'
            reparse_points = 'fail by path; never followed or silently omitted'
            root_entry = "implicit directory; the root's descriptor is outside this manifest"
        }
        entries = @($paths | ForEach-Object { $entries[$_] })
    }
}
