# Windows is the security descriptor oracle. Descriptors Windows writes on a
# volume Windows formats, and Windows' own account of them through Get-Acl,
# are the expected values; no NTFS reader from this crate or Slate is
# consulted. Run elevated on windows-latest in the Slate tier (#876).
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../scripts/fs-windows-test-harness/_lib.ps1"
. "$PSScriptRoot/../../test-disks/windows-fixture.ps1"
. "$PSScriptRoot/../../test-disks/capture-slate-windows-acl.ps1"
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$work = Join-Path $tempRoot ("slate-acl-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
$checks = 0
function Assert-Acl([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}
function Get-Entry($Manifest, [string]$Path) {
    $entry = @($Manifest.entries | Where-Object path -CEQ $Path)[0]
    if (-not $entry) { throw "no entry for $Path" }
    $entry
}
function Set-Sddl([string]$Path, [string]$Sddl) {
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetSecurityDescriptorSddlForm($Sddl, 'Access')
    Set-Acl -LiteralPath $Path -AclObject $acl
}
$digest = 'c' * 64
$administrators = 'S-1-5-32-544'
$users = 'S-1-5-32-545'
$guests = 'S-1-5-32-546'
$me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
# The populate block runs as an inline call, so it fills this table.
$captured = @{}
try {
    New-WindowsFixture -ImagePath (Join-Path $work 'acl.img') -Label 'SLATEACL' `
        -AllocationUnitSize 4096 -Populate {
            param($root)
            [IO.File]::WriteAllBytes("${root}default", [byte[]](1))
            $default = (Get-Acl -LiteralPath "${root}default").GetOwner([Security.Principal.SecurityIdentifier]).Value
            # Whichever of the two the token may own that Windows did not pick.
            $captured.owner = if ($default -ceq $administrators) { $me } else { $administrators }
            $captured.defaultOwner = $default
            [IO.File]::WriteAllBytes("${root}owned", [byte[]](2))
            $acl = Get-Acl -LiteralPath "${root}owned"
            $acl.SetOwner([Security.Principal.SecurityIdentifier]$captured.owner)
            Set-Acl -LiteralPath "${root}owned" -AclObject $acl
            # An inheritable explicit ACE on a directory, then children that inherit it.
            New-Item -ItemType Directory -Path "${root}dir" | Out-Null
            $acl = Get-Acl -LiteralPath "${root}dir"
            $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
                [Security.Principal.SecurityIdentifier]$users, 'ReadAndExecute',
                'ContainerInherit, ObjectInherit', 'None', 'Allow'))
            Set-Acl -LiteralPath "${root}dir" -AclObject $acl
            [IO.File]::WriteAllBytes("${root}dir\inherited", [byte[]](3))
            [IO.File]::WriteAllBytes("${root}dir\explicit", [byte[]](4))
            $acl = Get-Acl -LiteralPath "${root}dir\explicit"
            $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
                [Security.Principal.SecurityIdentifier]$guests, 'Write', 'Deny'))
            Set-Acl -LiteralPath "${root}dir\explicit" -AclObject $acl
            [IO.File]::WriteAllBytes("${root}protected", [byte[]](5))
            Set-Sddl "${root}protected" "D:P(A;;FA;;;BA)(A;;FA;;;$me)"
            $captured.manifest = Get-SlateWindowsAcl -Root $root -Case 'contract' -ImageSha256 $digest
            $captured.again = Get-SlateWindowsAcl -Root $root -Case 'contract' -ImageSha256 $digest
            # Get-Acl again, path by path, outside the capture function.
            $captured.reread = @{}
            foreach ($entry in $captured.manifest.entries) {
                $full = $root + $entry.path.TrimStart('/').Replace('/', '\')
                $captured.reread[$entry.path] = (Get-Acl -LiteralPath $full).GetSecurityDescriptorSddlForm('Owner, Group, Access')
            }
            $captured.rejectedSha = $false
            try { Get-SlateWindowsAcl -Root $root -Case 'contract' -ImageSha256 'wrong' | Out-Null }
            catch { $captured.rejectedSha = $true }
            $captured.rejectedRoot = $false
            try { Get-SlateWindowsAcl -Root "${root}dir" -Case 'contract' -ImageSha256 $digest | Out-Null }
            catch { $captured.rejectedRoot = $true }
            New-Item -ItemType Junction -Path "${root}link" -Target "${root}dir" | Out-Null
            $captured.reparseFailure = ''
            try { Get-SlateWindowsAcl -Root $root -Case 'contract' -ImageSha256 $digest | Out-Null }
            catch { $captured.reparseFailure = $_.Exception.Message }
            # Removes the junction, not the directory it points at.
            [IO.Directory]::Delete("${root}link", $false)
            # An OWNER RIGHTS ACE replaces the owner's implicit READ_CONTROL,
            # and everyone is denied it: Windows cannot return the descriptor.
            # It is written last, because nothing can read it back.
            [IO.File]::WriteAllBytes("${root}dir\locked", [byte[]](6))
            Set-Sddl "${root}dir\locked" 'D:P(D;;RC;;;WD)(A;;0x1;;;OW)'
            $captured.unreadableFailure = ''
            try { Get-SlateWindowsAcl -Root $root -Case 'contract' -ImageSha256 $digest | Out-Null }
            catch { $captured.unreadableFailure = $_.Exception.Message }
        }
    $manifest = $captured.manifest
    Assert-Acl ($manifest.schema -eq 1 -and $manifest.case -eq 'contract' -and
        $manifest.image_sha256 -ceq $digest) 'the ACL manifest is bound to the case and original image SHA-256'
    $paths = @($manifest.entries | ForEach-Object { $_.path })
    Assert-Acl (($paths -join ',') -ceq '/default,/dir,/dir/explicit,/dir/inherited,/owned,/protected') "every user file and directory has an entry: $($paths -join ',')"
    Assert-Acl (@($manifest.entries | Where-Object {
        $_.owner -cnotmatch '^S-1-[0-9-]+$' -or $_.group -cnotmatch '^S-1-[0-9-]+$' }).Count -eq 0) 'every entry records its owner and group as SIDs'
    $owned = Get-Entry $manifest '/owned'
    Assert-Acl ($owned.owner -ceq $captured.owner -and $owned.owner -cne $captured.defaultOwner -and
        (Get-Entry $manifest '/default').owner -ceq $captured.defaultOwner) 'an explicit owner is recorded, distinct from the default one'
    Assert-Acl (@($manifest.entries | Where-Object { $captured.reread[$_.path] -cne $_.sddl }).Count -eq 0) 'every SDDL is the one Get-Acl reads back for its path'
    Assert-Acl (@($manifest.entries | Where-Object {
        [Security.AccessControl.RawSecurityDescriptor]::new($_.sddl).GetSddlForm('Owner, Group, Access') -cne $_.sddl
    }).Count -eq 0) "every SDDL round-trips through Windows' descriptor parser"
    Assert-Acl (@($manifest.entries | Where-Object { -not $_.sddl.EndsWith($_.dacl_sddl) -or
        -not $_.dacl_sddl.StartsWith('D:') }).Count -eq 0) "each entry's DACL SDDL is the DACL of its descriptor"
    $directory = Get-Entry $manifest '/dir'
    Assert-Acl ($directory.kind -eq 'directory' -and @($directory.aces | Where-Object {
        $_.sid -ceq $users -and -not $_.inherited -and $_.flags -match 'ObjectInherit' -and
        $_.flags -match 'ContainerInherit' }).Count -eq 1) 'a directory records its explicit inheritable ACE and its flags'
    $inherited = Get-Entry $manifest '/dir/inherited'
    Assert-Acl (-not $inherited.dacl_protected -and $inherited.aces.Count -gt 0 -and
        @($inherited.aces | Where-Object { -not $_.inherited }).Count -eq 0 -and
        @($inherited.aces | Where-Object { $_.sid -ceq $users -and $_.type -eq 'AccessAllowed' }).Count -eq 1) 'an inherited DACL holds only inherited ACEs, including the parent ACE'
    $explicit = Get-Entry $manifest '/dir/explicit'
    $explicitAces = @($explicit.aces | Where-Object { -not $_.inherited })
    Assert-Acl ($explicitAces.Count -eq 1 -and $explicitAces[0].sid -ceq $guests -and
        $explicitAces[0].type -eq 'AccessDenied' -and
        @($explicit.aces | Where-Object { $_.inherited -and $_.sid -ceq $users }).Count -eq 1) 'an explicit ACE is distinguished from the inherited ones beside it'
    $protected = Get-Entry $manifest '/protected'
    Assert-Acl ($protected.dacl_protected -and $protected.dacl_sddl.StartsWith('D:P') -and
        @($protected.aces | Where-Object { $_.inherited }).Count -eq 0 -and
        @($protected.aces | Where-Object { $_.sid -ceq $administrators }).Count -eq 1) 'a protected DACL is flagged and holds no inherited ACEs'
    Assert-Acl (($manifest | ConvertTo-Json -Depth 10 -Compress) -ceq
        ($captured.again | ConvertTo-Json -Depth 10 -Compress)) 'repeated capture is deterministic'
    Assert-Acl $captured.rejectedSha 'an invalid image binding fails'
    Assert-Acl $captured.rejectedRoot 'a root that is not a volume root fails'
    Assert-Acl ($captured.unreadableFailure -match '/dir/locked') "an unreadable descriptor fails by path: '$($captured.unreadableFailure)'"
    Assert-Acl ($captured.reparseFailure -match '/link') "a reparse point fails by path: '$($captured.reparseFailure)'"
    if ($checks -lt 16) { throw "only $checks ACL contracts executed, floor 16" }
    Write-Host "slate-windows-acl.ps1: all checks passed ($checks; floor 16)"
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction Stop
}
