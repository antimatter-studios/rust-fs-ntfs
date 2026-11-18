param([string]$Scenario = '', [string]$Work = '')
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$builder = Join-Path $root 'test-disks/build-windows-4kn-fixture.ps1'
$ast = [System.Management.Automation.Language.Parser]::ParseFile($builder, [ref]$null, [ref]$null)

if ($Scenario) {
    . (Join-Path $root 'test-disks/windows-fixture.ps1')
    $definitions = @($ast.FindAll({ param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst]
    }, $false))
    if (-not ($definitions | Where-Object Name -eq 'Invoke-4KnFixturePhase')) {
        throw 'the 4Kn builder has no bounded, logged phase runner'
    }
    foreach ($definition in $definitions) { . ([scriptblock]::Create($definition.Extent.Text)) }
    $phaseTimeout = if ($Scenario -eq 'stall') { 1 } else { 10 }
    $options = @{ LogPath = (Join-Path $Work 'phases.log'); PhaseTimeoutSeconds = $phaseTimeout }
    switch ($Scenario) {
        'success' {
            $result = @(Invoke-4KnFixturePhase @options -PhaseName 'Format-Volume' -PhaseAction { 17; 'kept' })
            if (($result -join ',') -ne '17,kept') { throw 'phase output was changed or polluted by diagnostics' }
        }
        'failure' {
            Invoke-4KnFixturePhase @options -PhaseName 'Format-Volume' -PhaseAction { throw 'storage failed' }
        }
        'stall' {
            Invoke-4KnFixturePhase @options -PhaseName 'Format-Volume' -PhaseAction { Start-Sleep -Seconds 30 }
        }
        { $_ -in @('native', 'native-slow') } {
            $nativeCommand = if ($Scenario -eq 'native-slow') { 'Start-Sleep -Seconds 2; exit 7' } else { 'exit 7' }
            Invoke-4KnFixturePhase @options -PhaseName 'chkdsk.exe' -CheckNativeExit -PhaseAction {
                & (Get-Process -Id $PID).Path -NoProfile -Command $nativeCommand
            }
        }
        default { throw "unknown scenario $Scenario" }
    }
    exit 0
}

$script:checks = 0
function Check([bool]$Condition, [string]$Message) {
    $script:checks++
    if (-not $Condition) { throw "FAIL: $Message" }
}

$commands = @($ast.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.CommandAst]
}, $true))
foreach ($name in @('New-SectorGeometryDisk', 'Mount-DiskImage', 'Get-Disk', 'Initialize-Disk',
        'New-Partition', 'Format-Volume', 'fsutil', 'chkdsk.exe', 'Dismount-DiskImage', 'Get-DiskImage')) {
    $calls = @($commands | Where-Object { $_.GetCommandName() -eq $name })
    Check ($calls.Count -gt 0) "builder still exercises $name"
    foreach ($call in $calls) {
        $parent = $call.Parent
        while ($parent -and -not ($parent -is [System.Management.Automation.Language.CommandAst] -and
                $parent.GetCommandName() -eq 'Invoke-4KnFixturePhase')) { $parent = $parent.Parent }
        Check ($null -ne $parent) "$name runs inside a bounded, logged phase"
    }
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) ('4kn-phase-test-' + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $scratch
try {
    foreach ($scenarioName in @('success', 'failure', 'stall', 'native', 'native-slow')) {
        $workDir = Join-Path $scratch $scenarioName
        $null = New-Item -ItemType Directory -Path $workDir
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('-NoProfile', '-File', $PSCommandPath, '-Scenario', $scenarioName, '-Work', $workDir)) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $finished = $process.WaitForExit(30000)
        if (-not $finished) { $process.Kill($true); $null = $process.WaitForExit(5000) }
        $text = $stdout.Result + $stderr.Result
        Check $finished "$scenarioName terminates within thirty seconds"
        $expectedSuccess = $scenarioName -eq 'success'
        Check (($process.ExitCode -eq 0) -eq $expectedSuccess) "$scenarioName has the expected verdict: $text"
        $phase = if ($scenarioName -like 'native*') { 'chkdsk.exe' } else { 'Format-Volume' }
        $log = Get-Content -Raw -LiteralPath (Join-Path $workDir 'phases.log')
        Check ($log -match "phase=$([regex]::Escape($phase)) state=started") "$scenarioName retains its phase before execution"
        $state = if ($expectedSuccess) { 'finished' } else { 'failed' }
        Check ($log -match "phase=$([regex]::Escape($phase)) state=$state") "$scenarioName retains its final verdict"
        if ($scenarioName -eq 'stall') {
            Check ($text -match 'did not finish within 1 s') 'stalled phase names its deadline'
        }
        if ($scenarioName -like 'native*') {
            Check ($text -match 'exited 7') "$scenarioName failure retains its exit status: $text"
        }
        $process.Dispose()
    }
} finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force
}
Write-Host "windows-4kn-fixture-phases: all $script:checks checks passed"
