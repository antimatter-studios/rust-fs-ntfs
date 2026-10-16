# tests/scripts/windows-fixture-lifecycle.ps1 -- New-WindowsFixture's disk
# lifecycle, driven through success, a Populate failure and a Sync failure.
#
#   pwsh -NoProfile -File tests/scripts/windows-fixture-lifecycle.ps1
#
# Runs in the filename-fields-capture job before the real capture. The
# harness and Windows storage commands are replaced by recording functions,
# which shadow the cmdlets of the same name, so no disk is attached and no
# volume is formatted. Each case asserts that only the VHD the fixture owns
# is dismounted and removed, that an unrelated VHD beside it survives, and
# that an injected failure still reaches the caller.

$ErrorActionPreference = 'Stop'

$script:fails = 0
function Ok($what) { Write-Host "ok   $what" }
function Fail($what) { Write-Host "FAIL $what"; $script:fails++ }
function Check($cond, $what) { if ($cond) { Ok $what } else { Fail $what } }

$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ("wf-lifecycle-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $sandbox | Out-Null
$env:RUNNER_TEMP = Join-Path $sandbox 'runner-temp'
New-Item -ItemType Directory -Path $env:RUNNER_TEMP | Out-Null

$script:calls = [System.Collections.Generic.List[string]]::new()
$script:failSync = $false

function Get-VhdPathFor {
    param([string]$ImagePath)
    [System.IO.Path]::ChangeExtension($ImagePath, '.vhd')
}
function Initialize-VhdFromImg {
    param([string]$ImagePath, [string]$Diag)
    $vhd = [System.IO.Path]::ChangeExtension($ImagePath, '.vhd')
    Set-Content -LiteralPath $vhd -Value 'owned vhd'
    $script:calls.Add("initialize:$vhd")
    @{ Vhd = $vhd; Disk = 7 }
}
function Mount-VhdAndGetLetter {
    param([string]$Vhd)
    $script:calls.Add("mount:$Vhd")
    'Q'
}
function Format-Volume {
    [CmdletBinding(SupportsShouldProcess = $true)]
    param($DriveLetter, $FileSystem, $AllocationUnitSize, $NewFileSystemLabel, [switch]$Force)
    $script:calls.Add("format:${DriveLetter}:${FileSystem}:${AllocationUnitSize}:$NewFileSystemLabel")
}
function Sync-VhdToImg {
    param([string]$Vhd, [string]$ImagePath)
    $script:calls.Add("sync:$Vhd")
    if ($script:failSync) { throw 'injected sync failure' }
}
function Get-DiskImage {
    [CmdletBinding()]
    param([string]$ImagePath)
    $script:calls.Add("get-disk-image:$ImagePath")
    [pscustomobject]@{ ImagePath = $ImagePath; Attached = $true }
}
function Dismount-DiskImage {
    [CmdletBinding()]
    param([Parameter(ValueFromPipelineByPropertyName = $true)] [string]$ImagePath)
    process { $script:calls.Add("dismount:$ImagePath") }
}

# Loading the helper defines the function and touches nothing.
$before = @(Get-ChildItem -LiteralPath $sandbox -Recurse -Force | ForEach-Object FullName)
. "$PSScriptRoot/../../test-disks/windows-fixture.ps1"
$after = @(Get-ChildItem -LiteralPath $sandbox -Recurse -Force | ForEach-Object FullName)
Check ($null -ne (Get-Command New-WindowsFixture -CommandType Function -ErrorAction SilentlyContinue)) 'loading the helper defines New-WindowsFixture'
Check ($script:calls.Count -eq 0) "loading the helper calls no disk command (calls: $($script:calls -join ', '))"
Check ($null -eq (Compare-Object $before $after)) 'loading the helper creates no file'

function Invoke-Case {
    param([string]$Name, [scriptblock]$Populate, [bool]$FailSync)
    $dir = Join-Path $sandbox $Name
    New-Item -ItemType Directory -Path $dir | Out-Null
    $image = Join-Path $dir 'fixture.img'
    $owned = [System.IO.Path]::ChangeExtension($image, '.vhd')
    $sentinel = Join-Path $dir 'unrelated.vhd'
    Set-Content -LiteralPath $sentinel -Value 'not ours'
    $script:calls.Clear()
    $script:failSync = $FailSync
    $script:populatedRoot = $null
    $err = $null
    try {
        New-WindowsFixture -ImagePath $image -Label 'LIFECYCLE' -Populate $Populate
    } catch {
        $err = $_.Exception.Message
    }
    [pscustomobject]@{
        Error     = $err
        Owned     = $owned
        Image     = $image
        Sentinel  = $sentinel
        Calls     = @($script:calls)
        Dismounts = @($script:calls | Where-Object { $_ -like 'dismount:*' })
    }
}

function Check-Cleanup($case, $name) {
    Check (($case.Dismounts.Count -eq 1) -and ($case.Dismounts[0] -eq "dismount:$($case.Owned)")) `
        "${name}: exactly the owned VHD is dismounted (dismounts: $($case.Dismounts -join ', '))"
    Check (-not (Test-Path -LiteralPath $case.Owned)) "${name}: the owned VHD is removed"
    Check ((Test-Path -LiteralPath $case.Sentinel) -and ((Get-Content -LiteralPath $case.Sentinel) -eq 'not ours')) `
        "${name}: the unrelated VHD beside it survives untouched"
}

$ok = Invoke-Case -Name 'success' -FailSync $false -Populate {
    param($root)
    $script:populatedRoot = $root
}
Check ($null -eq $ok.Error) "success: no error reaches the caller ($($ok.Error))"
Check ($script:populatedRoot -eq 'Q:\') "success: Populate receives the mounted root ($script:populatedRoot)"
Check ($ok.Calls -contains 'format:Q:NTFS:4096:LIFECYCLE') 'success: the mounted letter is formatted NTFS with the default cluster size'
Check (@($ok.Calls | Where-Object { $_ -eq "sync:$($ok.Owned)" }).Count -eq 1) 'success: the owned VHD is synced back once'
Check ((Test-Path -LiteralPath $ok.Image) -and ((Get-Item -LiteralPath $ok.Image).Length -eq 256MB)) 'success: the raw image is 256 MiB'
Check ([array]::IndexOf($ok.Calls, "sync:$($ok.Owned)") -lt [array]::IndexOf($ok.Calls, "dismount:$($ok.Owned)")) 'success: sync runs before the dismount'
Check-Cleanup $ok 'success'

$populate = Invoke-Case -Name 'populate-failure' -FailSync $false -Populate {
    param($root)
    throw 'injected populate failure'
}
Check ($populate.Error -eq 'injected populate failure') "populate failure: the injected error reaches the caller ($($populate.Error))"
Check (-not ($populate.Calls | Where-Object { $_ -like 'sync:*' })) 'populate failure: nothing is synced back'
Check-Cleanup $populate 'populate failure'

$sync = Invoke-Case -Name 'sync-failure' -FailSync $true -Populate { param($root) }
Check ($sync.Error -eq 'injected sync failure') "sync failure: the injected error reaches the caller ($($sync.Error))"
Check-Cleanup $sync 'sync failure'

Remove-Item -LiteralPath $sandbox -Recurse -Force

if ($script:fails -gt 0) {
    Write-Host "windows-fixture-lifecycle: $script:fails check(s) failed"
    exit 1
}
Write-Host 'windows-fixture-lifecycle: all checks passed'
