# tests/scripts/sync-vhd-order.ps1 -- Sync-VhdToImg lets Windows detach the
# VHD, then copies the partition out of the fixed wrapper file, driven
# through the real function with the storage cmdlets replaced by recording
# mocks.
#
#   pwsh -NoProfile -File tests/scripts/sync-vhd-order.ps1
#
# CI runs it in the ubuntu-latest "Sync-VhdToImg order test" step.
#
# The wrapper file holds the volume's bytes as Windows leaves them: 0xD1
# while the disk is attached, as if NTFS could still be writing, and 0xC1
# once the detach has flushed it. The copied .img is therefore the record
# of what the copy saw, beside the call log. A forced `fsutil volume
# dismount` or an offline/online cycle is recorded too, and refused: the
# first left the next detach hung in the storage service (full runs
# 38874733868, 38897625186, 38908178473).
#
# Each mutant below is the real function with one change, and each must be
# caught: a guard that cannot fail is no guard.

$ErrorActionPreference = 'Stop'

$script:fails = 0
function Ok($what) { Write-Host "ok   $what" }
function Fail($what) { Write-Host "FAIL $what"; $script:fails++ }
function Check($cond, $what) { if ($cond) { Ok $what } else { Fail $what } }

# _lib.ps1 prepends a Windows-style cargo path to PATH; keep this host's.
$savedPath = $env:PATH
. "$PSScriptRoot/../../scripts/fs-windows-test-harness/_lib.ps1"
$env:PATH = $savedPath

$script:DiskNumber = 7
$script:Offset = 4096
$script:ImgSize = 8192
$script:Work = [IO.Path]::Combine([IO.Path]::GetTempPath(), "sync-vhd-order-$PID")
$script:Img = [IO.Path]::Combine($script:Work, 'scenario.img')
$script:Vhd = [IO.Path]::Combine($script:Work, 'scenario.vhd')
[IO.Directory]::CreateDirectory($script:Work) | Out-Null

$script:Log = [System.Collections.Generic.List[string]]::new()
$script:Attached = $true
$script:DetachFails = $false

function Get-DiskImage {
    [CmdletBinding()] param([string]$ImagePath)
    [pscustomobject]@{ ImagePath = $ImagePath; Attached = $script:Attached; Number = $script:DiskNumber }
}
function Get-Disk {
    [CmdletBinding()] param([int]$Number)
    [pscustomobject]@{ Number = $Number; PhysicalSectorSize = 512 }
}
function Get-Partition {
    [CmdletBinding()] param([int]$DiskNumber)
    [pscustomobject]@{ Type = 'Reserved'; Size = 1MB; Offset = 0 }
    [pscustomobject]@{ Type = 'Basic'; Size = $script:ImgSize; Offset = $script:Offset }
}
function Get-Volume {
    [CmdletBinding()] param($Partition)
    [pscustomobject]@{ DriveLetter = 'E' }
}
function fsutil {
    $script:Log.Add("fsutil $($args -join ' ')")
    $global:LASTEXITCODE = 0
}
function Set-Disk {
    [CmdletBinding()] param([int]$Number, [bool]$IsOffline)
    $script:Log.Add("Set-Disk $Number offline=$IsOffline")
}
function Dismount-DiskImage {
    [CmdletBinding()] param([Parameter(ValueFromPipelineByPropertyName)] [string]$ImagePath)
    process {
        $script:Log.Add('Dismount-DiskImage')
        if ($script:DetachFails) {
            # As the storage module reports one: a non-terminating error.
            Write-Error 'Dismount-DiskImage: the device is busy'
            return
        }
        Write-Wrapper 0xC1
        $script:Attached = $false
    }
}
# A fixed VHD: the disk's bytes from offset 0, then a 512-byte footer.
function Write-Wrapper([byte]$fill) {
    $bytes = [byte[]]::new($script:Offset + $script:ImgSize + 512)
    for ($i = $script:Offset; $i -lt $script:Offset + $script:ImgSize; $i++) { $bytes[$i] = $fill }
    [IO.File]::WriteAllBytes($script:Vhd, $bytes)
}

function Reset-Scenario([bool]$detachFails) {
    $script:Log.Clear()
    $script:Attached = $true
    $script:DetachFails = $detachFails
    [IO.File]::WriteAllBytes($script:Img, [byte[]](,0xAA * $script:ImgSize))
    Write-Wrapper 0xD1
}
function Img-Fill {
    $u = @([IO.File]::ReadAllBytes($script:Img) | Select-Object -Unique)
    if ($u.Count -eq 1) { '{0:X2}' -f $u[0] } else { 'mixed' }
}
function Observe([bool]$detachFails) {
    Reset-Scenario $detachFails
    $err = $null
    try { Sync-VhdToImg -Vhd $script:Vhd -ImagePath $script:Img } catch { $err = "$_" }
    [pscustomobject]@{ Err = $err; Order = ($script:Log -join ' | '); Fill = (Img-Fill) }
}

$Detach = 'Dismount-DiskImage'
function Success-Holds($o) { ($null -eq $o.Err) -and ($o.Order -eq $Detach) -and ($o.Fill -eq 'C1') }
function Failure-Holds($o) {
    ($o.Err -like '*device is busy*') -and ($o.Order -eq $Detach) -and ($o.Fill -eq 'AA')
}

# The real function, Windows detaching the VHD.
$o = Observe $false
Check ($null -eq $o.Err) "a successful detach lets the sync finish (error: $($o.Err))"
Check ($o.Order -eq $Detach) "the sync detaches the VHD and nothing else: no fsutil dismount, no offline/online cycle (saw: $($o.Order))"
Check ($o.Fill -eq 'C1') "the .img holds the bytes the wrapper had after the detach (fill $($o.Fill))"
Check (-not $script:Attached) 'the sync returns with the VHD detached'

# The real function, the detach refused.
$o = Observe $true
Check ($o.Err -like '*device is busy*') "a failed detach fails the sync, naming the error (error: $($o.Err))"
Check ($o.Fill -eq 'AA') "a failed detach leaves the .img untouched (fill $($o.Fill))"

# A VHD that is not attached has no partition to read.
Reset-Scenario $false
$script:Attached = $false
$err = $null
try { Sync-VhdToImg -Vhd $script:Vhd -ImagePath $script:Img } catch { $err = "$_" }
Check ($err -like '*VHD not attached*') "a detached VHD fails the sync by name (error: $err)"

# The scenario cleanup still runs when the sync fails: the warning is
# reported, the detach is retried, and both scratch files are removed.
Reset-Scenario $true
$warn = (Dismount-VhdAndCleanup -Vhd $script:Vhd -ImagePath $script:Img 3>&1 2>$null | ForEach-Object { "$_" }) -join "`n"
Check ($warn -like '*Sync-VhdToImg failed*device is busy*') "cleanup reports the failed sync as a warning (saw: $($warn.Trim()))"
Check (($script:Log -join ' | ') -eq "$Detach | $Detach") "cleanup retries the detach after a failed sync (saw: $($script:Log -join ' | '))"
Check (-not [IO.File]::Exists($script:Vhd)) 'cleanup removes the scratch .vhd after a failed sync'
Check (-not [IO.File]::Exists($script:Img)) 'cleanup removes the scenario .img after a failed sync'

# A successful sync leaves cleanup nothing to detach.
Reset-Scenario $false
Dismount-VhdAndCleanup -Vhd $script:Vhd -ImagePath $script:Img | Out-Null
Check (($script:Log -join ' | ') -eq $Detach) "cleanup after a successful sync detaches nothing more (saw: $($script:Log -join ' | '))"

# Mutation controls: each mutant must break the property the real function
# was just shown to hold.
$real = ${function:Sync-VhdToImg}.ToString()
function Use-Mutant($name, $from, $to) {
    if (-not $real.Contains($from)) {
        Fail "mutant '$name' no longer matches Sync-VhdToImg; update the control"
        return $false
    }
    Set-Item -Path 'function:script:Sync-VhdToImg' -Value ([scriptblock]::Create($real.Replace($from, $to)))
    return $true
}

$DetachLine = 'Dismount-DiskImage -ImagePath $Vhd -ErrorAction Stop | Out-Null'
if (Use-Mutant 'no detach' $DetachLine '') {
    $o = Observe $false
    Check (-not (Success-Holds $o)) "control: a sync that reads without detaching is caught (saw: $($o.Order); fill $($o.Fill))"
}
$SrcOpen = '$src = [System.IO.File]::Open($Vhd'
if (Use-Mutant 'forced dismount' $SrcOpen ("& fsutil volume dismount E: | Out-Null`n    " + $SrcOpen)) {
    $o = Observe $false
    Check (-not (Success-Holds $o)) "control: a sync that also forces a dismount is caught (saw: $($o.Order); fill $($o.Fill))"
}
if (Use-Mutant 'failure ignored' $DetachLine 'Dismount-DiskImage -ImagePath $Vhd -ErrorAction SilentlyContinue | Out-Null') {
    $o = Observe $true
    Check (-not (Failure-Holds $o)) "control: a sync that reads past a failed detach is caught (saw: $($o.Order); fill $($o.Fill))"
}
Set-Item -Path 'function:script:Sync-VhdToImg' -Value ([scriptblock]::Create($real))

[IO.Directory]::Delete($script:Work, $true)

if ($script:fails -gt 0) {
    Write-Host "sync-vhd-order: $($script:fails) check(s) failed"
    exit 1
}
Write-Host 'sync-vhd-order: all checks passed'
