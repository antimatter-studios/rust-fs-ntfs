# Windows oracle for `fs.ntfs mv` of a file into another directory (#600).
#
# Windows formats a fixed VHD with short-name generation off, so every
# file it writes has one name, and builds four directories: /src, /dst,
# /dst/sub and /big, which holds enough files that its index lives in INDX
# blocks. It writes the files below, one of them with a second hard link
# in /dst, and records each one's SHA-256. The image is detached, and
# `fs.ntfs mv` moves each file into another directory, some under a new
# name. Windows then mounts the image again:
#
# - read-only chkdsk must exit 0;
# - each new name must be listed, exactly so, in its new directory, and the
#   old name must be gone from the old one;
# - each file must hash as Windows wrote it;
# - the hard-linked file must still have two names, the moved one and the
#   one left in /dst, as `fsutil hardlink list` reports them.
#
# Usage: mv-across-directories-windows-oracle.ps1 -Cli <rust-fs-ntfs.exe> -OutDir <dir>
param(
    [Parameter(Mandatory = $true)][string]$Cli,
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$Cli = (Resolve-Path $Cli).Path
$img = Join-Path $OutDir 'mv-across-directories.vhd'

# Each case: the file Windows writes and the path `fs.ntfs mv` gives it.
$cases = @(
    [ordered]@{ case = 'same-name'; old = '\src\report.txt'; new = '\dst\report.txt'; content = 'a report' },
    [ordered]@{ case = 'new-name-deeper'; old = '\src\Quarterly Report Final.txt'; new = '\dst\sub\figures.txt'; content = 'quarterly figures' },
    [ordered]@{ case = 'out-of-indx'; old = '\big\file-060.txt'; new = '\from-big.txt'; content = $null },
    [ordered]@{ case = 'into-indx'; old = '\root-file.txt'; new = '\big\into-big.txt'; content = 'into a big directory' },
    [ordered]@{ case = 'hard-linked'; old = '\src\linked.txt'; new = '\big\linked.txt'; content = 'two names'; link = '\dst\linked-other.txt' },
    [ordered]@{ case = 'nonresident'; old = '\dst\large.bin'; new = '\src\large.bin'; content = ('0123456789abcdef' * 8192) }
)

# The volume of the attached VHD, online, writable and with a drive letter.
function Get-VhdPartition([string]$Path) {
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

# The names Windows lists in $Dir, files and directories.
function Get-Names([string]$Dir) {
    return @(Get-ChildItem -LiteralPath $Dir -Force | ForEach-Object { $_.Name })
}

# The CLI's form of a Windows path: forward slashes.
function ConvertTo-CliPath([string]$Path) { return $Path.Replace('\', '/') }

try {
    $dp = Join-Path $OutDir 'diskpart.txt'
    Set-Content -Path $dp -Value "create vdisk file=`"$img`" maximum=64 type=fixed" -Encoding ascii
    $log = & diskpart.exe /s $dp
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed ($LASTEXITCODE): $log" }
    Remove-Item -LiteralPath $dp -Force
    Mount-DiskImage -ImagePath $img | Out-Null
    $disk = Get-DiskImage -ImagePath $img | Get-Disk
    Initialize-Disk -Number $disk.Number -PartitionStyle MBR
    $part = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
    Format-Volume -Partition $part -FileSystem NTFS -NewFileSystemLabel 'MVACROSS' -Force -Confirm:$false | Out-Null
    $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber $part.PartitionNumber
    $letter = "$($part.DriveLetter):"
    [uint64]$offset = $part.Offset

    # One name per file: a separate DOS 8.3 name is a later part of #600.
    $set = & fsutil.exe 8dot3name set $letter 1 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil 8dot3name set $letter 1 failed ($LASTEXITCODE): $set" }

    foreach ($d in '\src', '\dst', '\dst\sub', '\big') {
        New-Item -ItemType Directory -Path "$letter$d" | Out-Null
    }
    foreach ($i in 0..119) {
        [IO.File]::WriteAllText("$letter\big\file-$('{0:d3}' -f $i).txt", "file $i")
    }
    foreach ($c in $cases) {
        $path = "$letter$($c.old)"
        if ($null -ne $c.content) { [IO.File]::WriteAllText($path, $c.content) }
        $c.sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if ($c.link) { New-Item -ItemType HardLink -Path "$letter$($c.link)" -Target $path | Out-Null }
    }
    Dismount-DiskImage -ImagePath $img | Out-Null

    $failures = @()
    foreach ($c in $cases) {
        $from = ConvertTo-CliPath $c.old
        $to = ConvertTo-CliPath $c.new
        # The CLI's error, if any, is JSON on stderr, which the log keeps.
        & $Cli fs --offset $offset $img mv $from $to | Out-Null
        $c.mv_exit = $LASTEXITCODE
        if ($c.mv_exit -ne 0) { $failures += "$($c.case): fs.ntfs mv $from $to exited $($c.mv_exit)" }
    }

    Mount-DiskImage -ImagePath $img | Out-Null
    $part = Get-VhdPartition $img
    $letter = "$($part.DriveLetter):"
    $chkdsk = @(& chkdsk.exe $letter 2>&1 | ForEach-Object { "$_" })
    $chkdskExit = $LASTEXITCODE
    $chkdsk | Set-Content -Path (Join-Path $OutDir 'chkdsk.txt') -Encoding utf8
    if ($chkdskExit -ne 0) { $failures += "chkdsk $letter exited $chkdskExit; see chkdsk.txt" }

    foreach ($c in $cases) {
        if ($c.mv_exit -ne 0) { continue }
        $newDir = Split-Path -Parent "$letter$($c.new)"
        $oldDir = Split-Path -Parent "$letter$($c.old)"
        $newName = Split-Path -Leaf $c.new
        $oldName = Split-Path -Leaf $c.old
        $exact = @(Get-Names $newDir | Where-Object { $_ -ceq $newName })
        if ($exact.Count -ne 1) { $failures += "$($c.case): Windows lists $($exact.Count) entries named exactly $newName in $newDir" }
        $left = @(Get-Names $oldDir | Where-Object { $_ -eq $oldName })
        if ($left.Count -ne 0) { $failures += "$($c.case): Windows still lists $oldName in $oldDir" }
        $path = "$letter$($c.new)"
        if (-not (Test-Path -LiteralPath $path)) {
            $failures += "$($c.case): Windows cannot open $($c.new)"
            continue
        }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if ($hash -ne $c.sha256) { $failures += "$($c.case): $($c.new) reads back $hash, Windows wrote $($c.sha256)" }
        if ($c.link) {
            $links = @(& fsutil.exe hardlink list $path 2>&1 | ForEach-Object { "$_" })
            $c.windows_links = $links
            $want = @($c.new, $c.link) | Sort-Object
            $got = @($links | ForEach-Object { $_.Trim() }) | Sort-Object
            if (($got -join '|') -ne ($want -join '|')) {
                $failures += "$($c.case): fsutil hardlink list gives $($got -join ', '), not $($want -join ', ')"
            }
        }
    }
    Dismount-DiskImage -ImagePath $img | Out-Null

    [ordered]@{
        cases = $cases
        chkdsk_exit = $chkdskExit
        failures = $failures
    } | ConvertTo-Json -Depth 6 |
        Set-Content -Path (Join-Path $OutDir 'mv-across-directories-windows.json') -Encoding utf8
    foreach ($f in $failures) { Write-Host "MV $f" }
    if ($failures.Count -gt 0) { throw "$($failures.Count) disagreement(s) with Windows" }
    Write-Host "mv-across-directories-windows-oracle: $($cases.Count) moves into other directories are clean to chkdsk and read back as Windows wrote them"
} finally {
    # Only this run's own image: detach it if still attached, then delete it.
    $attached = $null
    try { $attached = Get-DiskImage -ImagePath $img -ErrorAction Stop } catch { }
    if ($null -ne $attached -and $attached.Attached) {
        Dismount-DiskImage -ImagePath $img | Out-Null
    }
    if (Test-Path -LiteralPath $img) { Remove-Item -LiteralPath $img -Force }
    if (Test-Path -LiteralPath $img) { throw "the oracle image $img was not deleted" }
}
