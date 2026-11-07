# The Slate tier's Windows controls are shrunk to exactly 256 MiB before they
# are captured (#836), by the populate block that
# test-disks/build-slate-oracle-cases.ps1 hands New-WindowsFixture.
#
# That block used to call Resize-Partition unconditionally. When the scratch
# partition is already 256 MiB, Windows refuses with "Size Not Supported ...
# The partition is already the requested size", and the whole Slate run
# stopped at its first case (#1284).
#
# This runs the block itself, taken from the script's syntax tree, with
# Get-Partition and Resize-Partition replaced by stand-ins that behave as
# Windows does: a resize to the current size is refused with Windows' own
# message. No disk is touched, so it runs on any host with pwsh.
$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot '../../test-disks/build-slate-oracle-cases.ps1'
$ast = [System.Management.Automation.Language.Parser]::ParseFile($script, [ref]$null, [ref]$null)
$call = $ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.CommandAst] -and
        $node.GetCommandName() -eq 'New-WindowsFixture'
}, $true)
if (-not $call) { throw "no New-WindowsFixture call in $script" }
$populate = $null
for ($i = 0; $i -lt $call.CommandElements.Count - 1; $i++) {
    $element = $call.CommandElements[$i]
    if ($element -is [System.Management.Automation.Language.CommandParameterAst] -and
        $element.ParameterName -eq 'Populate') {
        $populate = $call.CommandElements[$i + 1]
    }
}
if ($populate -isnot [System.Management.Automation.Language.ScriptBlockExpressionAst]) {
    throw "the New-WindowsFixture call in $script has no -Populate script block"
}
$block = $populate.ScriptBlock.GetScriptBlock()

# The stand-ins. One partition, on drive E, of $script:partitionSize bytes.
$script:partitionSize = 0
$script:resizes = @()
function Get-Partition {
    [CmdletBinding()]
    param([string]$DriveLetter)
    if ($DriveLetter -ne 'E') { throw "Get-Partition: no partition on drive $DriveLetter" }
    [pscustomobject]@{ DriveLetter = 'E'; Size = [uint64]$script:partitionSize }
}
function Resize-Partition {
    [CmdletBinding()]
    param([string]$DriveLetter, [uint64]$Size)
    if ($DriveLetter -ne 'E') { throw "Resize-Partition: no partition on drive $DriveLetter" }
    $script:resizes += $Size
    if ($Size -eq $script:partitionSize) {
        throw "Size Not Supported`n`nExtended information:`nThe partition is already the requested size."
    }
    $script:partitionSize = $Size
}

$checks = 0
$failures = @()
function Test-Control([string]$Name, [uint64]$Start, [scriptblock]$Expect) {
    $script:partitionSize = $Start
    $script:resizes = @()
    $thrown = $null
    try { & $block 'E:\' } catch { $thrown = $_.Exception.Message }
    $verdict = & $Expect $thrown
    if ($verdict) {
        $script:failures += "${Name}: $verdict"
    } else {
        $script:checks++
    }
}

# With slack, as New-WindowsFixture formats it: shrunk to 256 MiB, once.
Test-Control 'a partition with slack' (304MB) {
    param($thrown)
    if ($thrown) { return "threw: $thrown" }
    if ($script:partitionSize -ne 256MB) { return "ended at $($script:partitionSize) bytes, not $(256MB)" }
    if ($script:resizes.Count -ne 1) { return "resized $($script:resizes.Count) times, not once" }
}

# Already 256 MiB: nothing to do, and no refusal from Windows.
Test-Control 'a partition already 256 MiB' (256MB) {
    param($thrown)
    if ($thrown) { return "threw: $thrown" }
    if ($script:partitionSize -ne 256MB) { return "ended at $($script:partitionSize) bytes" }
}

# Smaller than 256 MiB: the control cannot hold the volume the comparison
# expects, so the run stops, naming both sizes.
Test-Control 'a partition smaller than 256 MiB' (200MB) {
    param($thrown)
    if (-not $thrown) { return "did not stop (ended at $($script:partitionSize) bytes)" }
    if ($thrown -notmatch "$(200MB)" -or $thrown -notmatch "$(256MB)") {
        return "stopped without naming both sizes: $thrown"
    }
    if ($script:resizes.Count -ne 0) { return "resized a partition that was too small" }
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Output "  FAIL  $_" }
    Write-Output "slate-control-size: $checks passed, $($failures.Count) failed"
    exit 1
}
Write-Output "slate-control-size: $checks checks passed"
