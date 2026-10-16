# New-WindowsFixture: formats a 256 MiB raw image as NTFS through Windows,
# hands the mounted root to a populate block, and writes the volume back to
# the raw image. Shared by build-windows-native-read-fixtures.ps1 and
# capture-filename-fields.ps1.
#
# Dot-sourcing this file only defines functions: it builds nothing, mounts
# nothing and changes no state, so either producer can load it safely. Each
# lifecycle phase of a fixture is bounded and reported by name, and cleanup
# only ever reaches the fixture's own image and scratch VHD.
#
# The populate block receives the volume root ("X:\") while the volume is
# still mounted. The caller must have dot-sourced
# scripts/fs-windows-test-harness/_lib.ps1 first, and for the CLIREAD label
# also test-disks/capture-python-directory-oracle.ps1.

# One line per lifecycle boundary, so a run cancelled by the job timeout
# still names the fixture and the phase it was in when it stopped.
function Write-FixturePhase {
    param([string]$Fixture, [string]$Phase)
    Write-Host ("fixture-phase {0} {1} {2}" -f `
        (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ss.fffZ'), $Fixture, $Phase)
}

# Runs one lifecycle phase as a nested pipeline in this runspace, so the
# phase sees the caller's variables and functions exactly as an inline
# call would, while a timer on another thread can stop it. Stopping the
# pipeline ends the PowerShell code and the native commands this pipeline
# started, and nothing else. A phase still running after
# $PhaseTimeoutSeconds fails naming itself; with -CheckNativeExit, a
# native command that left a nonzero $LASTEXITCODE fails it too.
function Invoke-FixturePhase {
    param(
        [Parameter(Mandatory=$true)] [string]$PhaseName,
        [Parameter(Mandatory=$true)] [scriptblock]$PhaseAction,
        [object[]]$PhaseArguments = @(),
        [int]$PhaseTimeoutSeconds = 600,
        [switch]$CheckNativeExit
    )

    if (-not ('FixturePhaseWatchdog' -as [type])) {
        Add-Type -TypeDefinition @'
    public static class FixturePhaseWatchdog {
        public static System.Threading.Timer Arm(
                System.Management.Automation.PowerShell pipeline, int milliseconds) {
            return new System.Threading.Timer(_ => {
                try { pipeline.BeginStop(null, null); } catch { }
            }, null, milliseconds, System.Threading.Timeout.Infinite);
        }
    }
'@
    }

    $output = $null
    $failure = $null
    $global:LASTEXITCODE = 0
    $nested = [PowerShell]::Create([System.Management.Automation.RunspaceMode]::CurrentRunspace)
    $null = $nested.AddScript('param($fixturePhaseAction, $fixturePhaseArguments) & $fixturePhaseAction @fixturePhaseArguments', $false)
    $null = $nested.AddArgument($PhaseAction).AddArgument($PhaseArguments)
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $timer = [FixturePhaseWatchdog]::Arm($nested, $PhaseTimeoutSeconds * 1000)
    try {
        $output = $nested.Invoke()
    } catch {
        $failure = $_.Exception
        if ($failure -is [System.Management.Automation.MethodInvocationException] -and $failure.InnerException) {
            $failure = $failure.InnerException
        }
    } finally {
        $timer.Dispose()
        $stopped = $nested.InvocationStateInfo.State -eq 'Stopped'
        $errors = @($nested.Streams.Error)
        $nested.Dispose()
    }

    if ($stopped -or $watch.Elapsed.TotalSeconds -ge $PhaseTimeoutSeconds) {
        throw "phase $PhaseName did not finish within $PhaseTimeoutSeconds s"
    }
    if ($failure) { throw $failure }
    if ($errors.Count -gt 0) { throw $errors[0].Exception }
    if ($CheckNativeExit -and $global:LASTEXITCODE -ne 0) {
        throw "phase $PhaseName ran a native command that exited $global:LASTEXITCODE"
    }
    $output
}

# Detach a scratch VHD with diskpart, run as its own process with its own
# deadline, its output kept in a file and its duration logged. The detach
# outlasted a 30 s cleanup deadline on a different fixture in each of runs
# 38874733868, 38897625186, 38904454140 (Dismount-DiskImage) and 38906122818
# (diskpart run inline), so a stall now reports what diskpart printed and
# how long it had run.
function Dismount-ScratchVhd {
    param([Parameter(Mandatory=$true)] [string]$Vhd, [int]$TimeoutSeconds = 120)
    $tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
    $stem = Join-Path $tempRoot ("detach-" + [guid]::NewGuid())
    Set-Content -LiteralPath "$stem.txt" -Encoding ascii -Value @(
        "select vdisk file=`"$Vhd`"", 'detach vdisk')
    try {
        $watch = [System.Diagnostics.Stopwatch]::StartNew()
        $p = Start-Process -FilePath "$env:SystemRoot\System32\diskpart.exe" `
            -ArgumentList '/s', "`"$stem.txt`"" -NoNewWindow -PassThru `
            -RedirectStandardOutput "$stem.out" -RedirectStandardError "$stem.err"
        if (-not $p.WaitForExit($TimeoutSeconds * 1000)) {
            $p.Kill()
            $p.WaitForExit()
            throw ("diskpart detach vdisk did not exit within $TimeoutSeconds s: " +
                (Get-Content -Raw -LiteralPath "$stem.out" -ErrorAction SilentlyContinue))
        }
        $p.WaitForExit()
        $out = Get-Content -Raw -LiteralPath "$stem.out" -ErrorAction SilentlyContinue
        Write-Host ("diskpart detach exited {0} after {1} ms" -f $p.ExitCode, [int]$watch.Elapsed.TotalMilliseconds)
        if ($p.ExitCode -ne 0) { throw "diskpart detach vdisk exited $($p.ExitCode): $out" }
    } finally {
        Remove-Item -LiteralPath "$stem.txt", "$stem.out", "$stem.err" -Force -ErrorAction SilentlyContinue
    }
}

function New-WindowsFixture {
    param(
        [Parameter(Mandatory=$true)] [string]$ImagePath,
        [Parameter(Mandatory=$true)] [string]$Label,
        [Parameter(Mandatory=$true)] [scriptblock]$Populate,
        [int]$AllocationUnitSize = 4096
    )

    $tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
    $image = [System.IO.Path]::GetFullPath($ImagePath)
    $fixture = [System.IO.Path]::GetFileName($image)
    $diag = Join-Path $tempRoot ([System.IO.Path]::GetFileNameWithoutExtension($image))
    New-Item -ItemType Directory -Path $diag -Force | Out-Null
    Remove-Item $image,([System.IO.Path]::ChangeExtension($image, '.vhd')) `
        -Force -ErrorAction SilentlyContinue

    $timeout = 600
    if ($env:FIXTURE_PHASE_TIMEOUT_SECONDS) { $timeout = [int]$env:FIXTURE_PHASE_TIMEOUT_SECONDS }
    # Long enough for a detach Windows is slow to finish (see
    # Dismount-ScratchVhd), still well inside the job's own timeout.
    $cleanupTimeout = 180
    if ($env:FIXTURE_CLEANUP_TIMEOUT_SECONDS) { $cleanupTimeout = [int]$env:FIXTURE_CLEANUP_TIMEOUT_SECONDS }

    # Until init reports the scratch VHD, it can only be at the path this
    # run cleared above, so cleanup never reaches another run's disk.
    $vhd = [System.IO.Path]::ChangeExtension($image, '.vhd')
    $phase = 'image'
    try {
        Write-FixturePhase $fixture $phase
        $stream = [System.IO.File]::Create($image)
        try { $stream.SetLength(256MB) } finally { $stream.Close() }

        $phase = 'init'
        Write-FixturePhase $fixture $phase
        $state = Invoke-FixturePhase $phase -PhaseTimeoutSeconds $timeout {
            Initialize-VhdFromImg -ImagePath $image -Diag $diag
        }
        if ($state.Vhd) { $vhd = $state.Vhd }
        $phase = 'mount'
        Write-FixturePhase $fixture $phase
        $letter = Invoke-FixturePhase $phase -PhaseTimeoutSeconds $timeout {
            Mount-VhdAndGetLetter -Vhd $vhd
        }
        $phase = 'format'
        Write-FixturePhase $fixture "$phase letter=$letter"
        Invoke-FixturePhase $phase -PhaseTimeoutSeconds $timeout {
            Format-Volume -DriveLetter $letter -FileSystem NTFS `
                -AllocationUnitSize $AllocationUnitSize -NewFileSystemLabel $Label `
                -Force -Confirm:$false | Out-Null
        }
        $phase = 'populate'
        Write-FixturePhase $fixture $phase
        Invoke-FixturePhase $phase -PhaseTimeoutSeconds $timeout -CheckNativeExit `
            -PhaseArguments "${letter}:\" $Populate
        if ($Label -eq 'CLIREAD') {
            # Windows' own account of the mounted volume: the directory
            # listings the Python client is graded against, and fsutil's
            # serial and geometry for python-volume-info-windows.
            $phase = 'clread-capture'
            Write-FixturePhase $fixture $phase
            $capture = Invoke-FixturePhase $phase -PhaseTimeoutSeconds $timeout {
                $ntfsinfo = & fsutil fsinfo ntfsinfo "${letter}:"
                if ($LASTEXITCODE -ne 0) { throw "fsutil fsinfo ntfsinfo ${letter}: failed" }
                @{ Oracle = (New-PythonDirectoryOracle -Root "${letter}:\"); Ntfsinfo = $ntfsinfo }
            }
        }
        $phase = 'sync'
        Write-FixturePhase $fixture $phase
        Invoke-FixturePhase $phase -PhaseTimeoutSeconds $timeout {
            Sync-VhdToImg -Vhd $vhd -ImagePath $image
        }
        if ($Label -eq 'CLIREAD') {
            $phase = 'clread-record'
            Write-FixturePhase $fixture $phase
            $digest = (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant()
            $head = (& git rev-parse HEAD).Trim()
            if ($LASTEXITCODE -ne 0 -or $head -notmatch '^[0-9a-f]{40}$') { throw 'cannot bind the CLIREAD records to HEAD' }
            $record = @("image sha256 : $digest", "git HEAD : $head") + $capture.Ntfsinfo
            Set-Content -LiteralPath ([System.IO.Path]::ChangeExtension($image, '.ntfsinfo.txt')) -Value $record
            $directoryOracle = $capture.Oracle
            $directoryOracle['image sha256'] = $digest
            $directoryOracle['git HEAD'] = $head
            $directoryOracle | ConvertTo-Json -Depth 8 |
                Set-Content -LiteralPath ([IO.Path]::ChangeExtension($image, '.directory.json')) -Encoding utf8
        }
        $phase = 'done'
    } catch {
        Write-FixturePhase $fixture "failed phase=$phase error=$($_.Exception.Message)"
        throw
    } finally {
        # Bound storage cmdlets and file removal too: format/populate bounds
        # cannot interrupt an unbounded finally block. A recovery attempt gets
        # its own deadline and can only reach this fixture's recorded paths.
        Write-FixturePhase $fixture "cleanup after=$phase"
        $cleanupAction = {
            param([bool]$KeepImage)
            Write-FixturePhase $fixture "cleanup step=begin keep-image=$KeepImage"
            # Failed output is never a fixture, even if a storage provider
            # later refuses to detach its separate scratch VHD.
            if (-not $KeepImage -and (Test-Path -LiteralPath $image)) {
                Write-FixturePhase $fixture 'cleanup step=remove-invalid-image'
                Remove-Item -LiteralPath $image -Force -ErrorAction Stop
            }
            if (-not $KeepImage -and $Label -eq 'CLIREAD') {
                foreach ($suffix in '.directory.json', '.ntfsinfo.txt', '.sha256.tsv') {
                    $partial = [IO.Path]::ChangeExtension($image, $suffix)
                    if (Test-Path -LiteralPath $partial) {
                        Remove-Item -LiteralPath $partial -Force -ErrorAction Stop
                    }
                }
            }
            $attached = 0
            if (Test-Path -LiteralPath $vhd) {
                Write-FixturePhase $fixture 'cleanup step=query-before-detach'
                if (@(Get-DiskImage -ImagePath $vhd -ErrorAction Stop |
                        Where-Object Attached).Count -gt 0) {
                    Write-FixturePhase $fixture 'cleanup step=diskpart-detach'
                    Dismount-ScratchVhd -Vhd $vhd -TimeoutSeconds ([Math]::Max(1, $cleanupTimeout - 15))
                }
                Write-FixturePhase $fixture 'cleanup step=query-after-detach'
                $attached = @(Get-DiskImage -ImagePath $vhd -ErrorAction Stop |
                    Where-Object Attached).Count
                if ($attached -ne 0) { throw "owned scratch VHD remains attached: $vhd" }
                Write-FixturePhase $fixture 'cleanup step=remove-detached-vhd'
                Remove-Item -LiteralPath $vhd -Force -ErrorAction Stop
            }
            Write-FixturePhase $fixture 'cleanup step=verify-owned-paths'
            if ((Test-Path -LiteralPath $vhd) -or
                (-not $KeepImage -and (Test-Path -LiteralPath $image))) {
                throw "owned fixture output remains after cleanup: $image / $vhd"
            }
            Write-FixturePhase $fixture ("cleaned attached=$attached vhd-present=$(Test-Path $vhd)" +
                " image-present=$(Test-Path $image)")
        }
        try {
            Invoke-FixturePhase 'cleanup' -PhaseTimeoutSeconds $cleanupTimeout `
                -PhaseArguments @($phase -eq 'done') $cleanupAction
        } catch {
            $cleanupFailure = $_.Exception
            Write-FixturePhase $fixture "failed phase=cleanup error=$($cleanupFailure.Message)"
            try {
                # A cleanup failure invalidates even a fully synced image.
                Invoke-FixturePhase 'cleanup-recovery' -PhaseTimeoutSeconds $cleanupTimeout `
                    -PhaseArguments @($false) $cleanupAction
            } catch {
                Write-FixturePhase $fixture ("failed phase=cleanup-recovery error=$($_.Exception.Message)" +
                    " owned-image=$image owned-vhd=$vhd")
            }
            throw $cleanupFailure
        }
    }

    if (-not (Test-Path $image) -or (Get-Item $image).Length -ne 256MB) {
        throw "fixture was not written at the expected size: $image"
    }
    Write-FixturePhase $fixture 'complete'
}
