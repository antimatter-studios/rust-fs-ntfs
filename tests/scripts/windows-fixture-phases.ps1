# tests/scripts/windows-fixture-phases.ps1 -- New-WindowsFixture, the real
# function from test-disks/windows-fixture.ps1, is driven through success,
# a stalled phase and a failing native command, with the storage cmdlets
# replaced by recording mocks.
#
#   pwsh -NoProfile -File tests/scripts/windows-fixture-phases.ps1
#
# The test parses the helper and evaluates only its top-level function
# definitions: the code under test is the helper's own text, not a copy of
# it. (tests/scripts/windows-fixture-lifecycle.ps1 holds the ownership
# contract: only this fixture's VHD is detached and removed.)
#
# Each scenario runs in a child pwsh that this harness kills after a hard
# limit, so a lifecycle that never ends fails here in seconds, naming the
# phase, instead of holding a Windows runner until the job timeout.
#
# The contract held here:
#   * success leaves the final image at full size, the VHD detached and
#     the scratch VHD removed, for downstream steps to consume;
#   * a phase that stalls past FIXTURE_PHASE_TIMEOUT_SECONDS fails on its
#     own, naming that phase, detaches the VHD and removes this run's
#     partial image and scratch VHD;
#   * a native command that exits nonzero fails the fixture in the phase
#     that ran it, with the same cleanup;
#   * a stalled detach is bounded and reported as cleanup failure, then a
#     bounded recovery detaches only this run's VHD and removes its output.

param([string]$Scenario = '', [string]$Work = '')

$ErrorActionPreference = 'Stop'

if ($Scenario) {
    $env:RUNNER_TEMP = $Work
    $env:FIXTURE_PHASE_TIMEOUT_SECONDS = '5'
    $env:FIXTURE_CLEANUP_TIMEOUT_SECONDS = '5'
    $builder = [IO.Path]::Combine($PSScriptRoot, '..', '..', 'test-disks',
        'windows-fixture.ps1')
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $builder, [ref]$null, [ref]$null)
    $defs = @($ast.FindAll({
        param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst]
    }, $false))
    if (-not ($defs | Where-Object Name -eq 'New-WindowsFixture')) {
        throw "the helper defines no New-WindowsFixture: $builder"
    }
    foreach ($def in $defs) { . ([scriptblock]::Create($def.Extent.Text)) }

    $script:VhdPath = [IO.Path]::Combine($Work, 'scenario.vhd')
    $script:Attached = $false
    $script:DetachCalls = 0
    if ($Scenario -eq 'cleanup-stall') {
        [IO.File]::WriteAllBytes([IO.Path]::Combine($Work, 'unrelated.vhd'), [byte[]](9, 8, 7))
    }

    function Initialize-VhdFromImg {
        param($ImagePath, $Diag)
        [IO.File]::WriteAllBytes($script:VhdPath, [byte[]](1, 2, 3))
        $script:Attached = $true
        Write-Host 'mock init'
        [pscustomobject]@{ Vhd = $script:VhdPath }
    }
    function Mount-VhdAndGetLetter { param($Vhd) 'Q' }
    function Format-Volume {
        param($DriveLetter, $FileSystem, $AllocationUnitSize,
              $NewFileSystemLabel, [switch]$Force, $Confirm)
        if ($Scenario -eq 'stall') {
            Write-Host 'mock format stalls'
            while ($true) { Start-Sleep -Seconds 1 }
        }
    }
    function Sync-VhdToImg { param($Vhd, $ImagePath) Write-Host 'mock sync' }
    function Get-DiskImage {
        [CmdletBinding()] param([string]$ImagePath)
        if (Test-Path $ImagePath) {
            [pscustomobject]@{ ImagePath = $ImagePath; Attached = $script:Attached }
        }
    }
    function Dismount-ScratchVhd {
        param([string]$Vhd, [int]$TimeoutSeconds)
        $script:DetachCalls++
        if ($Scenario -eq 'cleanup-stall' -and $script:DetachCalls -eq 1) {
            Write-Host 'mock cleanup detach stalls'
            while ($true) { Start-Sleep -Seconds 1 }
        }
        $script:Attached = $false
        Write-Host 'mock detach'
    }

    $pwsh = (Get-Process -Id $PID).Path
    $populate = switch ($Scenario) {
        'native' { { param($root) & $pwsh -NoProfile -Command 'exit 3' } }
        default  { { param($root) Write-Host "mock populate $root" } }
    }

    $image = [IO.Path]::Combine($Work, 'fixture.img')
    $code = 0
    try {
        New-WindowsFixture -ImagePath $image -Label 'LIFECYCLE' -Populate $populate
        Write-Host 'scenario-result returned'
    } catch {
        Write-Host "scenario-result threw $($_.Exception.Message)"
        $code = 1
    }
    Write-Host ("mock-state attached={0} vhd-present={1} image-present={2}" -f `
        $script:Attached, (Test-Path $script:VhdPath), (Test-Path $image))
    exit $code
}

$script:fails = 0
function Ok($what) { Write-Host "ok   $what" }
function Fail($what) { Write-Host "FAIL $what"; $script:fails++ }
function Check($cond, $what) { if ($cond) { Ok $what } else { Fail $what } }

$limitSeconds = 60
$pwsh = (Get-Process -Id $PID).Path

function Invoke-Scenario([string]$Name) {
    $work = [IO.Path]::Combine([IO.Path]::GetTempPath(), "fixture-lifecycle-$PID-$Name")
    [IO.Directory]::CreateDirectory($work) | Out-Null
    $psi = [Diagnostics.ProcessStartInfo]::new($pwsh)
    foreach ($a in '-NoProfile', '-File', $PSCommandPath, '-Scenario', $Name, '-Work', $work) {
        $psi.ArgumentList.Add($a)
    }
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $p = [Diagnostics.Process]::Start($psi)
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    $finished = $p.WaitForExit($limitSeconds * 1000)
    if (-not $finished) { $p.Kill($true); $p.WaitForExit() }
    $text = $out.Result + $err.Result
    Write-Host "--- scenario $Name"
    Write-Host $text
    [pscustomobject]@{
        Finished = $finished
        Exit     = $(if ($finished) { $p.ExitCode } else { $null })
        Out      = $text
        Image    = [IO.Path]::Combine($work, 'fixture.img')
        Work     = $work
    }
}

$ok = Invoke-Scenario 'success'
Check $ok.Finished "success: the lifecycle ends within $limitSeconds s"
Check ($ok.Exit -eq 0) "success: the fixture is built (exit $($ok.Exit))"
Check ($ok.Out -match 'fixture-phase \S+ fixture\.img complete') 'success: the complete phase is reported'
Check ($ok.Out -match 'mock detach') 'success: the VHD is detached'
Check ($ok.Out -match 'mock-state attached=False vhd-present=False image-present=True') `
    'success: the scratch VHD is gone and the final image is kept for its consumers'
Check ((Test-Path $ok.Image) -and (Get-Item $ok.Image).Length -eq 256MB) `
    'success: the final image is retained at 256 MiB'

$stall = Invoke-Scenario 'stall'
Check $stall.Finished "stall: a phase stalled past FIXTURE_PHASE_TIMEOUT_SECONDS fails on its own within $limitSeconds s instead of hanging"
Check ($stall.Finished -and $stall.Exit -ne 0) "stall: the fixture fails (exit $($stall.Exit))"
Check ($stall.Out -match 'failed phase=format') 'stall: the failure names the stalled phase, format'
Check ($stall.Out -match 'mock-state attached=False vhd-present=False image-present=False') `
    'stall: the VHD is detached and this run''s partial image and scratch VHD are removed'

$native = Invoke-Scenario 'native'
Check $native.Finished "native: the lifecycle ends within $limitSeconds s"
Check ($native.Finished -and $native.Exit -ne 0) "native: a native command exiting 3 fails the fixture (exit $($native.Exit))"
Check ($native.Out -match 'failed phase=populate') 'native: the failure names the phase that ran it, populate'
Check ($native.Out -match 'mock-state attached=False vhd-present=False image-present=False') `
    'native: the VHD is detached and this run''s partial image and scratch VHD are removed'

# Exact-head Windows red: run 38853480159, job 116821062200. Cleanup of
# ntfs-ea.img took 180 seconds; ntfs-format-64k.img's cleanup was still
# running when the 15-minute job limit cancelled the builder. All earlier
# lifecycle phases had finished, so bounding format/populate alone did not
# cover the storage cmdlets in the finally block.
$cleanup = Invoke-Scenario 'cleanup-stall'
Check ($cleanup.Out -match 'mock cleanup detach stalls') 'cleanup-stall: the mock reaches the stalled detach'
Check $cleanup.Finished "cleanup-stall: cleanup ends on its own within $limitSeconds s instead of hanging"
Check ($cleanup.Finished -and $cleanup.Exit -ne 0) "cleanup-stall: the fixture fails (exit $($cleanup.Exit))"
Check ($cleanup.Out -match 'failed phase=cleanup') 'cleanup-stall: the failure names cleanup'
Check ($cleanup.Out -match 'mock-state attached=False vhd-present=False image-present=False') `
    'cleanup-stall: recovery detaches and removes only this run''s failed image and scratch VHD'
Check ((Test-Path ([IO.Path]::Combine($cleanup.Work, 'unrelated.vhd'))) -and `
    ([Convert]::ToHexString([IO.File]::ReadAllBytes([IO.Path]::Combine($cleanup.Work, 'unrelated.vhd'))) -eq '090807')) `
    'cleanup-stall: an unrelated VHD is preserved byte for byte'

foreach ($w in $ok.Work, $stall.Work, $native.Work, $cleanup.Work) {
    Remove-Item -LiteralPath $w -Recurse -Force -ErrorAction SilentlyContinue
}

if ($script:fails -gt 0) {
    Write-Host "windows-fixture-phases: $($script:fails) check(s) failed"
    exit 1
}
Write-Host 'windows-fixture-phases: all checks passed'
