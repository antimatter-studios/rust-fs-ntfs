# Windows oracle for `fs.ntfs mv` of a file that has a DOS 8.3 name (#739).
#
# Our own tools give a file one name, in the POSIX or WIN32_AND_DOS
# namespace, so the CLI's rename scenario never meets what Windows writes
# for a long name when short-name generation is on: a WIN32 name and a
# separate DOS 8.3 name on the same record. Here Windows formats a fixed
# VHD, turns short-name generation on for that volume, and writes two files
# with long names; `dir /x` must show each one's generated short name
# before the image leaves Windows. `fs.ntfs mv` renames one to another long
# name and the other to a name that fits 8.3. Windows then mounts the
# image again: chkdsk must find it clean, the root must list each new name
# exactly and neither old one, and each file must read back with the bytes
# Windows wrote. The short name Windows reports afterwards is recorded
# beside the names `fs.ntfs stat` reports, before and after the rename.
#
# Usage: mv-short-name-windows-oracle.ps1 -Cli <rust-fs-ntfs.exe> -OutDir <dir>
param(
    [Parameter(Mandatory = $true)][string]$Cli,
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$Cli = (Resolve-Path $Cli).Path
$img = Join-Path $OutDir 'mv-short-name.vhd'

Add-Type -Namespace MvShortName -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern uint GetShortPathNameW(string lpszLongPath, System.Text.StringBuilder lpszShortPath, uint cchBuffer);
'@

# Each case: the long name Windows writes, the name `fs.ntfs mv` gives it.
$cases = @(
    [ordered]@{ case = 'long-to-long'; old = 'Quarterly Report Final.txt'; new = 'Revised Quarterly Figures.txt'; content = 'quarterly figures' },
    [ordered]@{ case = 'long-to-8.3'; old = 'Annual Budget Summary.txt'; new = 'budget.txt'; content = 'annual budget' }
)

# The leaf of the short path Windows reports for $Path: the DOS name when the
# file has one, else the file's only name.
function Get-ShortName([string]$Path) {
    $buf = New-Object System.Text.StringBuilder 1024
    $n = [MvShortName.Native]::GetShortPathNameW($Path, $buf, [uint32]$buf.Capacity)
    if ($n -eq 0 -or $n -ge $buf.Capacity) {
        throw "GetShortPathNameW $Path failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    return Split-Path -Leaf $buf.ToString()
}

# `dir /x` of the root, the listing the issue grades the short names by.
function Get-DirX([string]$Letter) {
    $out = & cmd.exe /d /c "dir /x /a-d $Letter\*" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "dir /x $Letter\* failed ($LASTEXITCODE): $out" }
    return @($out | ForEach-Object { "$_" })
}

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

# `fs.ntfs stat`'s $FILE_NAMEs for $Name in the root, as name and namespace.
function Get-CliNames([uint64]$Offset, [string]$Name) {
    $out = & $Cli fs --json --offset $Offset $img stat "/$Name"
    if ($LASTEXITCODE -ne 0) { throw "fs.ntfs stat /$Name exited $LASTEXITCODE" }
    $stat = ($out -join "`n") | ConvertFrom-Json
    return @($stat.ntfs.file_names | ForEach-Object { [ordered]@{ name = $_.name; namespace = $_.namespace } })
}

# The system-wide 8dot3 mode, so it can be put back: 2 lets each volume
# choose, which is what `fsutil 8dot3name set X: 0` needs.
function Get-SystemShortNameMode {
    $q = & fsutil.exe 8dot3name query 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil 8dot3name query failed ($LASTEXITCODE): $q" }
    $m = [regex]::Match(($q -join "`n"), 'registry state is:\s*(\d)')
    if (-not $m.Success) { throw "fsutil 8dot3name query gave no registry state: $q" }
    return [int]$m.Groups[1].Value
}

$systemMode = $null
try {
    $systemMode = Get-SystemShortNameMode
    if ($systemMode -ne 0 -and $systemMode -ne 2) {
        $set = & fsutil.exe 8dot3name set 2 2>&1
        if ($LASTEXITCODE -ne 0) { throw "fsutil 8dot3name set 2 failed ($LASTEXITCODE): $set" }
    }

    $dp = Join-Path $OutDir 'diskpart.txt'
    Set-Content -Path $dp -Value "create vdisk file=`"$img`" maximum=64 type=fixed" -Encoding ascii
    $log = & diskpart.exe /s $dp
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed ($LASTEXITCODE): $log" }
    Remove-Item -LiteralPath $dp -Force
    Mount-DiskImage -ImagePath $img | Out-Null
    $disk = Get-DiskImage -ImagePath $img | Get-Disk
    Initialize-Disk -Number $disk.Number -PartitionStyle MBR
    $part = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
    Format-Volume -Partition $part -FileSystem NTFS -NewFileSystemLabel 'MVSHORT' -Force -Confirm:$false | Out-Null
    $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber $part.PartitionNumber
    $letter = "$($part.DriveLetter):"
    [uint64]$offset = $part.Offset

    $set = & fsutil.exe 8dot3name set $letter 0 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil 8dot3name set $letter 0 failed ($LASTEXITCODE): $set" }
    $volumeQuery = @(& fsutil.exe 8dot3name query $letter 2>&1 | ForEach-Object { "$_" })

    foreach ($c in $cases) {
        $path = Join-Path "$letter\" $c.old
        [IO.File]::WriteAllText($path, $c.content)
        $c.sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        $c.windows_short_before = Get-ShortName $path
    }
    $dirBefore = Get-DirX $letter
    foreach ($c in $cases) {
        # A generated short name: not the long name, and `dir /x` shows it
        # in the short-name column, before the long name.
        if ($c.windows_short_before -eq $c.old -or $c.windows_short_before -notmatch '~') {
            throw "Windows gave $($c.old) no generated short name ($($c.windows_short_before)), so the case proves nothing; fsutil 8dot3name query ${letter}: $($volumeQuery -join ' ')"
        }
        $pattern = "\s$([regex]::Escape($c.windows_short_before))\s+$([regex]::Escape($c.old))$"
        $row = @($dirBefore | Where-Object { $_ -match $pattern })
        if ($row.Count -ne 1) {
            throw "dir /x does not list $($c.old) with the short name $($c.windows_short_before):`n$($dirBefore -join "`n")"
        }
    }
    Dismount-DiskImage -ImagePath $img | Out-Null

    $failures = @()
    foreach ($c in $cases) {
        $c.cli_names_before = Get-CliNames $offset $c.old
        # The CLI's error, if any, is JSON on stderr, which the log keeps.
        & $Cli fs --offset $offset $img mv "/$($c.old)" "/$($c.new)" | Out-Null
        $c.mv_exit = $LASTEXITCODE
        if ($c.mv_exit -ne 0) {
            $failures += "$($c.case): fs.ntfs mv /$($c.old) /$($c.new) exited $($c.mv_exit)"
            continue
        }
        $c.cli_names_after = Get-CliNames $offset $c.new
    }

    Mount-DiskImage -ImagePath $img | Out-Null
    $part = Get-VhdPartition $img
    $letter = "$($part.DriveLetter):"
    $chkdsk = @(& chkdsk.exe $letter 2>&1 | ForEach-Object { "$_" })
    $chkdskExit = $LASTEXITCODE
    $chkdsk | Set-Content -Path (Join-Path $OutDir 'chkdsk.txt') -Encoding utf8
    if ($chkdskExit -ne 0) { $failures += "chkdsk $letter exited $chkdskExit; see chkdsk.txt" }

    $listed = @(Get-ChildItem -LiteralPath "$letter\" -Force -File | ForEach-Object { $_.Name })
    $dirAfter = Get-DirX $letter
    foreach ($c in $cases) {
        if ($c.mv_exit -ne 0) { continue }
        $exact = @($listed | Where-Object { $_ -ceq $c.new })
        if ($exact.Count -ne 1) { $failures += "$($c.case): Windows lists $($exact.Count) entries named exactly $($c.new)" }
        $old = @($listed | Where-Object { $_ -eq $c.old })
        if ($old.Count -ne 0) { $failures += "$($c.case): Windows still lists $($c.old)" }
        $path = Join-Path "$letter\" $c.new
        if (-not (Test-Path -LiteralPath $path)) {
            $failures += "$($c.case): Windows cannot open $($c.new)"
            continue
        }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if ($hash -ne $c.sha256) { $failures += "$($c.case): $($c.new) reads back $hash, Windows wrote $($c.sha256)" }
        $c.windows_short_after = Get-ShortName $path
    }
    Dismount-DiskImage -ImagePath $img | Out-Null

    [ordered]@{
        volume_8dot3 = $volumeQuery
        system_8dot3_mode = $systemMode
        cases = $cases
        dir_x_before = $dirBefore
        dir_x_after = $dirAfter
        chkdsk_exit = $chkdskExit
        failures = $failures
    } | ConvertTo-Json -Depth 6 |
        Set-Content -Path (Join-Path $OutDir 'mv-short-name-windows.json') -Encoding utf8
    foreach ($c in $cases) {
        $before = ($c.cli_names_before | ForEach-Object { "$($_.name) [$($_.namespace)]" }) -join ', '
        $after = ($c.cli_names_after | ForEach-Object { "$($_.name) [$($_.namespace)]" }) -join ', '
        Write-Host "MV $($c.case): Windows short $($c.windows_short_before) -> $($c.windows_short_after); fs.ntfs stat $before -> $after"
    }
    foreach ($f in $failures) { Write-Host "MV $f" }
    if ($failures.Count -gt 0) { throw "$($failures.Count) disagreement(s) with Windows" }
    Write-Host "mv-short-name-windows-oracle: both renames of a file with a DOS name are clean to chkdsk and read back as Windows wrote them"
} finally {
    # Only this run's own image: detach it if still attached, then delete it.
    $attached = $null
    try { $attached = Get-DiskImage -ImagePath $img -ErrorAction Stop } catch { }
    if ($null -ne $attached -and $attached.Attached) {
        Dismount-DiskImage -ImagePath $img | Out-Null
    }
    if (Test-Path -LiteralPath $img) { Remove-Item -LiteralPath $img -Force }
    if ($null -ne $systemMode -and $systemMode -ne 0 -and $systemMode -ne 2) {
        & fsutil.exe 8dot3name set $systemMode | Out-Null
    }
    if (Test-Path -LiteralPath $img) { throw "the oracle image $img was not deleted" }
}
