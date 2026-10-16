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
#
# The state the mocks share is global: New-WindowsFixture runs each phase as
# a nested pipeline, where $script: would name the global scope, not this
# script's.

$ErrorActionPreference = 'Stop'

$script:fails = 0
function Ok($what) { Write-Host "ok   $what" }
function Fail($what) { Write-Host "FAIL $what"; $script:fails++ }
function Check($cond, $what) { if ($cond) { Ok $what } else { Fail $what } }

$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ("wf-lifecycle-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $sandbox | Out-Null
$env:RUNNER_TEMP = Join-Path $sandbox 'runner-temp'
New-Item -ItemType Directory -Path $env:RUNNER_TEMP | Out-Null

$global:calls = [System.Collections.Generic.List[string]]::new()
$global:failSync = $false

function Get-VhdPathFor {
    param([string]$ImagePath)
    [System.IO.Path]::ChangeExtension($ImagePath, '.vhd')
}
function Initialize-VhdFromImg {
    param([string]$ImagePath, [string]$Diag)
    $vhd = [System.IO.Path]::ChangeExtension($ImagePath, '.vhd')
    Set-Content -LiteralPath $vhd -Value 'owned vhd'
    $global:attached = $true
    $global:calls.Add("initialize:$vhd")
    @{ Vhd = $vhd; Disk = 7 }
}
function Mount-VhdAndGetLetter {
    param([string]$Vhd)
    $global:calls.Add("mount:$Vhd")
    'Q'
}
function Format-Volume {
    [CmdletBinding(SupportsShouldProcess = $true)]
    param($DriveLetter, $FileSystem, $AllocationUnitSize, $NewFileSystemLabel, [switch]$Force)
    $global:calls.Add("format:${DriveLetter}:${FileSystem}:${AllocationUnitSize}:$NewFileSystemLabel")
}
function Sync-VhdToImg {
    param([string]$Vhd, [string]$ImagePath)
    $global:calls.Add("sync:$Vhd")
    if ($global:failSync) { throw 'injected sync failure' }
}
function Get-DiskImage {
    [CmdletBinding()]
    param([string]$ImagePath)
    $global:calls.Add("get-disk-image:$ImagePath")
    [pscustomobject]@{ ImagePath = $ImagePath; Attached = $global:attached }
}
function Dismount-DiskImage {
    [CmdletBinding()]
    param([Parameter(ValueFromPipelineByPropertyName = $true)] [string]$ImagePath)
    process { $global:calls.Add("dismount:$ImagePath") }
}

# Loading the helper defines the function and touches nothing.
$before = @(Get-ChildItem -LiteralPath $sandbox -Recurse -Force | ForEach-Object FullName)
. "$PSScriptRoot/../../test-disks/windows-fixture.ps1"
$after = @(Get-ChildItem -LiteralPath $sandbox -Recurse -Force | ForEach-Object FullName)
Check ($null -ne (Get-Command New-WindowsFixture -CommandType Function -ErrorAction SilentlyContinue)) 'loading the helper defines New-WindowsFixture'
Check ($global:calls.Count -eq 0) "loading the helper calls no disk command (calls: $($global:calls -join ', '))"
Check ($null -eq (Compare-Object $before $after)) 'loading the helper creates no file'

# The helper detaches through diskpart; record the detach instead. Defined
# after loading the helper, which defines the real one.
function Dismount-ScratchVhd {
    param([string]$Vhd, [int]$TimeoutSeconds)
    $global:calls.Add("dismount:$Vhd")
    $global:attached = $false
}

function Invoke-Case {
    param([string]$Name, [scriptblock]$Populate, [bool]$FailSync)
    $dir = Join-Path $sandbox $Name
    New-Item -ItemType Directory -Path $dir | Out-Null
    $image = Join-Path $dir 'fixture.img'
    $owned = [System.IO.Path]::ChangeExtension($image, '.vhd')
    $sentinel = Join-Path $dir 'unrelated.vhd'
    Set-Content -LiteralPath $sentinel -Value 'not ours'
    $global:calls.Clear()
    $global:attached = $false
    $global:failSync = $FailSync
    $global:populatedRoot = $null
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
        Calls     = @($global:calls)
        Dismounts = @($global:calls | Where-Object { $_ -like 'dismount:*' })
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
    $global:populatedRoot = $root
}
Check ($null -eq $ok.Error) "success: no error reaches the caller ($($ok.Error))"
Check ($global:populatedRoot -eq 'Q:\') "success: Populate receives the mounted root ($global:populatedRoot)"
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
