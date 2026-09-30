# Captures NTFS volumes that Windows was writing to at the moment they were
# copied, so that their $LogFile holds work Windows had logged and not yet
# checkpointed -- and then lets Windows recover a separate copy of each, and
# records what it recovered (#366, for #137).
#
# HOW A "CRASH" IS MADE WITHOUT POWERING ANYTHING OFF. The test volume lives
# in a fixed VHD, a file on C:. While a workload writes to the volume, a VSS
# shadow copy of C: is taken: a point-in-time image of every block of C:,
# the VHD file's among them. VSS flushes C:, not the volume INSIDE the VHD,
# so the VHD in the shadow holds exactly what that inner volume had written
# to its disk at that instant -- what a power cut at that instant would
# have left, and nothing the inner file system still held in memory. The
# ClientAccessible context runs no VSS writers.
#
# FOR EACH SNAPSHOT k, into $Out:
#   pre-k.vhd          the VHD as the shadow holds it, never attached again
#   pre-k.sha256       its SHA-256, taken before anything else touches it
#   post-k.vhd         a copy of pre-k.vhd that Windows attached, recovered
#                      and detached again
#   post-k.manifest.tsv  every file Windows sees after recovery: path, size,
#                      SHA-256 (Windows' own Get-FileHash)
#   post-k.chkdsk.txt  read-only chkdsk of the recovered copy
# and once:
#   markers.log        one line per workload operation: index, UTC time
#   snapshots.tsv      k, UTC time, the marker count when the shadow was taken
#   layout.json        the partition's byte offset in the VHD
#   ntfs-events.txt    the System log's Ntfs events from the whole run
#
# Deciding which pre-images hold pending log records is done off the runner,
# from the bytes, by tools that are not this script.
#
# Run elevated on Windows (GitHub's windows-latest is). Windows PowerShell 5.1
# or PowerShell 7.
param(
    [int]$Candidates = 6,
    [int]$SizeMB = 128,
    [int]$WorkloadSeconds = 40,
    [string]$Out = 'logfile-oracle'
)

$ErrorActionPreference = 'Stop'
$root = 'C:\oracle366'
Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $root -Force | Out-Null
$Out = (New-Item -ItemType Directory $Out -Force).FullName
$vhd = Join-Path $root 'disk.vhd'
$markers = Join-Path $Out 'markers.log'
$started = Get-Date

function Invoke-Diskpart([string]$Script) {
    $file = Join-Path $root 'diskpart.txt'
    Set-Content -Path $file -Value $Script -Encoding ascii
    $log = & diskpart.exe /s $file
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed ($LASTEXITCODE): $log" }
}

function Get-VhdVolumeLetter([string]$Path) {
    $disk = Get-DiskImage -ImagePath $Path | Get-Disk
    if ($disk.IsOffline) { Set-Disk -Number $disk.Number -IsOffline $false }
    if ($disk.IsReadOnly) { Set-Disk -Number $disk.Number -IsReadOnly $false }
    $part = Get-Partition -DiskNumber $disk.Number | Where-Object Type -ne 'Reserved' | Select-Object -First 1
    if (-not $part.DriveLetter) {
        $part | Add-PartitionAccessPath -AssignDriveLetter
        $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber $part.PartitionNumber
    }
    return $part
}

# ---- the volume ---------------------------------------------------------
Invoke-Diskpart "create vdisk file=`"$vhd`" maximum=$SizeMB type=fixed"
Mount-DiskImage -ImagePath $vhd | Out-Null
$disk = Get-DiskImage -ImagePath $vhd | Get-Disk
Initialize-Disk -Number $disk.Number -PartitionStyle MBR
$part = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
Format-Volume -Partition $part -FileSystem NTFS -AllocationUnitSize 4096 `
    -NewFileSystemLabel 'ORACLE366' -Force -Confirm:$false | Out-Null
$part = Get-Partition -DiskNumber $disk.Number -PartitionNumber $part.PartitionNumber
$vol = "$($part.DriveLetter):\"
@{ partition_offset = $part.Offset; partition_size = $part.Size; vhd_bytes = (Get-Item $vhd).Length } |
    ConvertTo-Json | Set-Content (Join-Path $Out 'layout.json')
Write-Host "volume $vol at byte $($part.Offset) of $vhd"

# ---- the workload, in its own process -----------------------------------
# Metadata-heavy on purpose: every create, rename and delete is a logged
# NTFS transaction, and small files keep the lazy writer busy.
$workload = Join-Path $root 'workload.ps1'
@'
param($Vol, $Markers, $Seconds)
$rng = New-Object System.Random 366
$end = (Get-Date).AddSeconds($Seconds)
$i = 0
while ((Get-Date) -lt $end) {
    $dir = Join-Path $Vol ('d{0:D3}' -f ($i % 64))
    [System.IO.Directory]::CreateDirectory($dir) | Out-Null
    $file = Join-Path $dir ('f{0:D7}.bin' -f $i)
    $buf = New-Object byte[] ($rng.Next(1, 32768))
    $rng.NextBytes($buf)
    [System.IO.File]::WriteAllBytes($file, $buf)
    if ($i % 3 -eq 0) { [System.IO.File]::Move($file, "$file.renamed") }
    if ($i -ge 400 -and $i % 2 -eq 0) {
        $old = $i - 400
        $victim = Join-Path $Vol ('d{0:D3}\f{1:D7}.bin' -f ($old % 64), $old)
        foreach ($p in @($victim, "$victim.renamed")) {
            if ([System.IO.File]::Exists($p)) { [System.IO.File]::Delete($p) }
        }
    }
    [System.IO.File]::AppendAllText($Markers, "$i $([DateTime]::UtcNow.ToString('o'))`r`n")
    $i++
}
'@ | Set-Content -Path $workload -Encoding utf8

$shell = (Get-Process -Id $PID).Path
$proc = Start-Process -FilePath $shell -PassThru -NoNewWindow -ArgumentList @(
    '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $workload,
    '-Vol', $vol, '-Markers', $markers, '-Seconds', $WorkloadSeconds)

# ---- the snapshots, while it runs ---------------------------------------
Start-Sleep -Seconds 4
$snapshots = @()
$gap = [Math]::Max(1, [int](($WorkloadSeconds - 8) / $Candidates))
for ($k = 1; $k -le $Candidates; $k++) {
    $created = Invoke-CimMethod -ClassName Win32_ShadowCopy -MethodName Create `
        -Arguments @{ Volume = 'C:\'; Context = 'ClientAccessible' }
    if ($created.ReturnValue -ne 0) { throw "shadow copy $k failed: $($created.ReturnValue)" }
    $at = [DateTime]::UtcNow.ToString('o')
    $count = if (Test-Path $markers) { (Get-Content $markers).Count } else { 0 }
    $shadow = Get-CimInstance Win32_ShadowCopy | Where-Object ID -eq $created.ShadowID
    $link = Join-Path $root "snap$k"
    cmd /c mklink /d "$link" "$($shadow.DeviceObject)\" | Out-Null
    try {
        Copy-Item (Join-Path $link 'oracle366\disk.vhd') (Join-Path $Out "pre-$k.vhd")
    } finally {
        cmd /c rmdir "$link" | Out-Null
        $shadow | Remove-CimInstance
    }
    (Get-FileHash (Join-Path $Out "pre-$k.vhd") -Algorithm SHA256).Hash.ToLower() |
        Set-Content (Join-Path $Out "pre-$k.sha256")
    $snapshots += "$k`t$at`t$count"
    Write-Host "snapshot $k at $at after $count operations"
    Start-Sleep -Seconds $gap
}
$snapshots | Set-Content (Join-Path $Out 'snapshots.tsv')

$proc.WaitForExit()
Dismount-DiskImage -ImagePath $vhd | Out-Null

# ---- Windows recovers a copy of each ------------------------------------
for ($k = 1; $k -le $Candidates; $k++) {
    $post = Join-Path $Out "post-$k.vhd"
    Copy-Item (Join-Path $Out "pre-$k.vhd") $post
    Mount-DiskImage -ImagePath $post | Out-Null
    try {
        $p = Get-VhdVolumeLetter $post
        $letter = "$($p.DriveLetter):"
        # Mounting is what makes NTFS run its restart pass; the first access
        # below forces the mount.
        $rows = Get-ChildItem "$letter\" -Recurse -File -Force -ErrorAction SilentlyContinue |
            Sort-Object FullName | ForEach-Object {
                $rel = $_.FullName.Substring(3) -replace '\\', '/'
                "$rel`t$($_.Length)`t$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())"
            }
        $rows | Set-Content (Join-Path $Out "post-$k.manifest.tsv")
        & chkdsk.exe $letter 2>&1 | Set-Content (Join-Path $Out "post-$k.chkdsk.txt")
        & fsutil.exe dirty query $letter 2>&1 | Add-Content (Join-Path $Out "post-$k.chkdsk.txt")
    } finally {
        Dismount-DiskImage -ImagePath $post | Out-Null
    }
    Write-Host "post $k recovered and detached"
}

Get-WinEvent -FilterHashtable @{ LogName = 'System'; ProviderName = 'Ntfs'; StartTime = $started } `
    -ErrorAction SilentlyContinue |
    Format-List TimeCreated, Id, LevelDisplayName, Message |
    Out-String -Width 200 | Set-Content (Join-Path $Out 'ntfs-events.txt')

Write-Host "done: $Candidates candidates in $Out"
