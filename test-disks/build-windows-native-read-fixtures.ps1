# Builds the fixtures whose shapes only Windows can author:
#   * ntfs-attrlist.img: named $DATA streams overflow into extension records
#   * ntfs-compressed.img: classic NTFS/LZNT1-compressed $DATA
#   * ntfs-symlink.img: symlinks Windows created, plus
#     ntfs-symlink.targets.tsv, the target Windows itself reports for each
#   * ntfs-cli-read.img: files Windows wrote, plus ntfs-cli-read.sha256.tsv,
#     each file's path, size and the SHA-256 Windows itself computes
#   * ntfs-ea.img: extended attributes Windows set through NtSetEaFile, plus
#     ntfs-ea.names.tsv, each name and value as it was handed to Windows;
#     ea-summary-{0,1,2}.bin distinguish compact/query sizes and NEED_EA counts
#   * ntfs-format-64k.img: an empty volume Windows formatted with 64 KiB
#     clusters, whose $MFTMirr the formatter is held to
#   * ntfs-usnjrnl.img: a volume with an active change journal, plus
#     ntfs-usnjrnl.query.txt and ntfs-usnjrnl.records.csv, what fsutil
#     reports of the journal and of every record in it
#
# Run from the repository root on an elevated Windows host. rust-img-vhd must be
# on PATH; CI installs the pinned rust-img-vhd release before invoking this.

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }

function New-WindowsFixture {
    param(
        [Parameter(Mandatory=$true)] [string]$ImagePath,
        [Parameter(Mandatory=$true)] [string]$Label,
        [Parameter(Mandatory=$true)] [scriptblock]$Populate,
        [int]$AllocationUnitSize = 4096
    )

    $image = [System.IO.Path]::GetFullPath($ImagePath)
    $diag = Join-Path $tempRoot ([System.IO.Path]::GetFileNameWithoutExtension($image))
    New-Item -ItemType Directory -Path $diag -Force | Out-Null
    Remove-Item $image,([System.IO.Path]::ChangeExtension($image, '.vhd')) `
        -Force -ErrorAction SilentlyContinue

    $stream = [System.IO.File]::Create($image)
    try { $stream.SetLength(256MB) } finally { $stream.Close() }

    $state = Initialize-VhdFromImg -ImagePath $image -Diag $diag
    try {
        $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
        Format-Volume -DriveLetter $letter -FileSystem NTFS `
            -AllocationUnitSize $AllocationUnitSize -NewFileSystemLabel $Label `
            -Force -Confirm:$false | Out-Null
        & $Populate "${letter}:\"
        Sync-VhdToImg -Vhd $state.Vhd -ImagePath $image
    } finally {
        Get-DiskImage -ImagePath $state.Vhd -ErrorAction SilentlyContinue |
            Where-Object Attached |
            Dismount-DiskImage -ErrorAction SilentlyContinue | Out-Null
        Remove-Item $state.Vhd -Force -ErrorAction SilentlyContinue
    }

    if (-not (Test-Path $image) -or (Get-Item $image).Length -ne 256MB) {
        throw "fixture was not written at the expected size: $image"
    }
}

New-WindowsFixture -ImagePath 'test-disks/ntfs-attrlist.img' -Label 'ATTRLIST' -Populate {
    param($root)
    $file = Join-Path $root 'many.bin'
    [System.IO.File]::WriteAllBytes($file, [byte[]](0x42))

    # Each resident named stream consumes an attribute header, UTF-16 name,
    # and value in the file record. Enough streams force Windows to create an
    # $ATTRIBUTE_LIST and extension records; the Rust tests assert that shape.
    for ($i = 0; $i -lt 96; $i++) {
        $name = 'stream-{0:D3}' -f $i
        $bytes = [System.Text.Encoding]::UTF8.GetBytes("payload-$name")
        [System.IO.File]::WriteAllBytes("${file}:$name", $bytes)
    }
}

New-WindowsFixture -ImagePath 'test-disks/ntfs-compressed.img' -Label 'COMPRESSED' -Populate {
    param($root)
    $file = Join-Path $root 'comp.txt'
    $bytes = New-Object byte[] 200000
    $pattern = [System.Text.Encoding]::ASCII.GetBytes('ABC')
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = $pattern[$i % 3] }
    [System.IO.File]::WriteAllBytes($file, $bytes)

    & compact.exe /C /I /Q $file | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "compact.exe failed with exit $LASTEXITCODE" }
    $attrs = (Get-Item $file -Force).Attributes
    if (($attrs -band [System.IO.FileAttributes]::Compressed) -eq 0) {
        throw 'Windows did not mark comp.txt as NTFS-compressed'
    }
}

# Symlinks created by Windows, and the target Windows reports for each, so
# fs_ntfs_readlink is graded against the platform rather than against the
# reparse bytes this crate's own writer produces (tests/readlink_windows_oracle.rs).
# The report is .NET's LinkTarget, which is the reparse point's PrintName.
$symlinkReport = [System.IO.Path]::GetFullPath('test-disks/ntfs-symlink.targets.tsv')
New-WindowsFixture -ImagePath 'test-disks/ntfs-symlink.img' -Label 'SYMLINK' -Populate {
    param($root)
    New-Item -ItemType Directory -Path (Join-Path $root 'sub') | Out-Null
    [System.IO.File]::WriteAllBytes((Join-Path $root 'sub\file.txt'), [byte[]](0x41))
    [System.IO.File]::WriteAllBytes((Join-Path $root "sub\na$([char]0x00EF)ve-$([char]0x65E5)$([char]0x672C).txt"), [byte[]](0x42))

    $links = [ordered]@{
        'abs-dir'  = 'C:\Windows\System32'
        'rel-file' = 'sub\file.txt'
        'rel-uni'  = "sub\na$([char]0x00EF)ve-$([char]0x65E5)$([char]0x672C).txt"
    }
    $lines = foreach ($name in $links.Keys) {
        $link = Join-Path $root $name
        New-Item -ItemType SymbolicLink -Path $link -Value $links[$name] | Out-Null
        $reported = (Get-Item -LiteralPath $link -Force).LinkTarget
        if (-not $reported) { throw "Windows reports no link target for $name" }
        "$name`t$reported"
    }
    [System.IO.File]::WriteAllText($symlinkReport, (($lines -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
}

# Files Windows writes and hashes itself, so `fs.ntfs ls` and `fs.ntfs read`
# are graded against the platform rather than against bytes this crate's own
# writer produced (tests/cli-oracle/test-windows-authored.sh). The sizes sit
# on the boundaries that change how NTFS stores a file: empty, resident in the
# MFT record, one cluster, one cluster and a byte, and a large random file.
$cliReadReport = [System.IO.Path]::GetFullPath('test-disks/ntfs-cli-read.sha256.tsv')
New-WindowsFixture -ImagePath 'test-disks/ntfs-cli-read.img' -Label 'CLIREAD' -Populate {
    param($root)
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    New-Item -ItemType Directory -Path (Join-Path $root 'sub\deep') -Force | Out-Null
    $files = [ordered]@{
        'empty.bin'            = 0
        'one.bin'              = 1
        'resident.bin'         = 500
        'cluster.bin'          = 4096
        'cluster-plus-one.bin' = 4097
        'random.bin'           = 1048576
        'sub/deep/file.bin'    = 5000
        "na$([char]0x00EF)ve-$([char]0x65E5)$([char]0x672C).txt" = 42
    }
    $lines = foreach ($rel in $files.Keys) {
        $size = $files[$rel]
        $bytes = New-Object byte[] $size
        if ($size -gt 0) { $rng.GetBytes($bytes) }
        $path = Join-Path $root ($rel -replace '/', '\')
        [System.IO.File]::WriteAllBytes($path, $bytes)
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        "/$rel`t$size`t$hash"
    }
    [System.IO.File]::WriteAllText($cliReadReport, (($lines -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
}

# Extended attributes Windows sets itself, so the bytes this crate stores for
# an EA name are graded against what NTFS stores for the same call
# (tests/ea_names_windows_oracle.rs, #482). Each is set with NtSetEaFile, the
# call every Windows EA API reaches, in a different case.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class WindowsEa {
    [StructLayout(LayoutKind.Sequential)]
    public struct IoStatusBlock { public IntPtr Status; public IntPtr Information; }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern SafeFileHandle CreateFileW(
        string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);

    [DllImport("ntdll.dll")]
    public static extern int NtSetEaFile(
        SafeFileHandle handle, out IoStatusBlock status, byte[] buffer, uint length);
}
'@
$eaReport = [System.IO.Path]::GetFullPath('test-disks/ntfs-ea.names.tsv')
New-WindowsFixture -ImagePath 'test-disks/ntfs-ea.img' -Label 'EA' -Populate {
    param($root)
    $file = Join-Path $root 'ea.bin'
    [System.IO.File]::WriteAllBytes($file, [byte[]](0x45))
    $eas = [ordered]@{ 'user.a' = 'lower'; 'User.Mixed' = 'mixed'; 'UPPER.B' = 'upper' }
    # FILE_READ_EA | FILE_WRITE_EA | SYNCHRONIZE, OPEN_EXISTING: a synchronous
    # handle, which NtSetEaFile needs.
    $handle = [WindowsEa]::CreateFileW($file, [uint32]0x100018, [uint32]7, [IntPtr]::Zero,
        [uint32]3, [uint32]0x80, [IntPtr]::Zero)
    if ($handle.IsInvalid) {
        throw "open ea.bin for EAs: Win32 error $([System.Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    try {
        foreach ($name in $eas.Keys) {
            $n = [System.Text.Encoding]::ASCII.GetBytes($name)
            $v = [System.Text.Encoding]::ASCII.GetBytes($eas[$name])
            # FILE_FULL_EA_INFORMATION: next entry 0, flags 0, name length,
            # value length, the name and its NUL, then the value.
            $buf = New-Object byte[] (8 + $n.Length + 1 + $v.Length)
            $buf[5] = [byte]$n.Length
            [BitConverter]::GetBytes([uint16]$v.Length).CopyTo($buf, 6)
            $n.CopyTo($buf, 8)
            $v.CopyTo($buf, 8 + $n.Length + 1)
            $status = New-Object WindowsEa+IoStatusBlock
            $rc = [WindowsEa]::NtSetEaFile($handle, [ref]$status, $buf, [uint32]$buf.Length)
            if ($rc -ne 0) { throw ("NtSetEaFile {0}: NTSTATUS 0x{1:X8}" -f $name, $rc) }
        }
    } finally { $handle.Dispose() }
    $lines = foreach ($name in $eas.Keys) { "$name`t$($eas[$name])" }
    [System.IO.File]::WriteAllText($eaReport, (($lines -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))

    # Two independently specified entries: compact 9 + 10 bytes, query
    # 16 + 16 bytes. Windows writes the eight-byte on-disk summary itself.
    foreach ($needCount in 0..2) {
        $file = Join-Path $root "ea-summary-$needCount.bin"
        [System.IO.File]::WriteAllBytes($file, [byte[]](0x45))
        $entries = @(
            @{ Name = 'A'; Value = 'aaa'; Flags = $(if ($needCount -eq 2) { 0x80 } else { 0 }) },
            @{ Name = 'LONG'; Value = 'x'; Flags = $(if ($needCount -gt 0) { 0x80 } else { 0 }) }
        )
        $handle = [WindowsEa]::CreateFileW($file, [uint32]0x100018, [uint32]7, [IntPtr]::Zero,
            [uint32]3, [uint32]0x80, [IntPtr]::Zero)
        if ($handle.IsInvalid) {
            throw "open ea-summary-$needCount.bin for EAs: Win32 error $([System.Runtime.InteropServices.Marshal]::GetLastWin32Error())"
        }
        try {
            foreach ($entry in $entries) {
                $n = [System.Text.Encoding]::ASCII.GetBytes($entry.Name)
                $v = [System.Text.Encoding]::ASCII.GetBytes($entry.Value)
                $buf = New-Object byte[] (8 + $n.Length + 1 + $v.Length)
                $buf[4] = [byte]$entry.Flags
                $buf[5] = [byte]$n.Length
                [BitConverter]::GetBytes([uint16]$v.Length).CopyTo($buf, 6)
                $n.CopyTo($buf, 8)
                $v.CopyTo($buf, 8 + $n.Length + 1)
                $status = New-Object WindowsEa+IoStatusBlock
                $rc = [WindowsEa]::NtSetEaFile($handle, [ref]$status, $buf, [uint32]$buf.Length)
                if ($rc -ne 0) { throw ("NtSetEaFile {0}: NTSTATUS 0x{1:X8}" -f $entry.Name, $rc) }
            }
        } finally { $handle.Dispose() }
    }
}

# What Windows' formatter writes into $MFTMirr when a cluster holds more than
# the four records the mirror is for, so the formatter here is held to
# Windows rather than to mkntfs (tests/mftmirr_windows_oracle.rs, #481).
New-WindowsFixture -ImagePath 'test-disks/ntfs-format-64k.img' -Label 'C64K' `
    -AllocationUnitSize 65536 -Populate { param($root) }

# A volume with a change journal Windows created and wrote to, plus what
# Windows itself reports of it: ntfs-usnjrnl.query.txt, `fsutil usn
# queryjournal`'s answer, and ntfs-usnjrnl.records.csv, every record `fsutil
# usn readjournal` reads back. Each change below is one this crate also
# makes, on a file named for it, so the records show what Windows journals
# for each (#479).
$usnQuery = [System.IO.Path]::GetFullPath('test-disks/ntfs-usnjrnl.query.txt')
$usnRecords = [System.IO.Path]::GetFullPath('test-disks/ntfs-usnjrnl.records.csv')
New-WindowsFixture -ImagePath 'test-disks/ntfs-usnjrnl.img' -Label 'USNJRNL' -Populate {
    param($root)
    $volume = $root.TrimEnd('\')
    $o = & fsutil.exe usn createjournal m=8388608 a=1048576 $volume 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil usn createjournal: $o" }

    [System.IO.File]::WriteAllBytes((Join-Path $root 'create-empty.txt'), [byte[]]@())
    [System.IO.File]::WriteAllText((Join-Path $root 'write-resident.txt'), 'resident')
    [System.IO.File]::WriteAllBytes((Join-Path $root 'write-nonresident.bin'), (New-Object byte[] 10000))
    $f = [System.IO.File]::Open((Join-Path $root 'truncate.bin'), 'Create')
    try { $f.SetLength(8192); $f.SetLength(100) } finally { $f.Close() }
    New-Item -ItemType Directory -Path (Join-Path $root 'mkdir') | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $root 'rmdir') | Out-Null
    Remove-Item -LiteralPath (Join-Path $root 'rmdir')
    [System.IO.File]::WriteAllText((Join-Path $root 'rename-from.txt'), 'r')
    Rename-Item -LiteralPath (Join-Path $root 'rename-from.txt') -NewName 'rename-to.txt'
    [System.IO.File]::WriteAllText((Join-Path $root 'move.txt'), 'm')
    Move-Item -LiteralPath (Join-Path $root 'move.txt') -Destination (Join-Path $root 'mkdir\move.txt')
    [System.IO.File]::WriteAllText((Join-Path $root 'unlink.txt'), 'u')
    Remove-Item -LiteralPath (Join-Path $root 'unlink.txt')
    [System.IO.File]::WriteAllText((Join-Path $root 'link-target.txt'), 'l')
    New-Item -ItemType HardLink -Path (Join-Path $root 'link-name.txt') `
        -Target (Join-Path $root 'link-target.txt') | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $root 'ads.txt'), 'a')
    [System.IO.File]::WriteAllText((Join-Path $root 'ads.txt:stream'), 's')
    [System.IO.File]::WriteAllText((Join-Path $root 'attributes.txt'), 'h')
    [System.IO.File]::SetAttributes((Join-Path $root 'attributes.txt'), 'Hidden')
    [System.IO.File]::WriteAllText((Join-Path $root 'times.txt'), 't')
    [System.IO.File]::SetLastWriteTimeUtc((Join-Path $root 'times.txt'), [datetime]'2020-01-01T00:00:00Z')

    $q = & fsutil.exe usn queryjournal $volume 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil usn queryjournal: $q" }
    [System.IO.File]::WriteAllText($usnQuery, (($q | Out-String).Trim() + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
    $r = & fsutil.exe usn readjournal $volume csv 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil usn readjournal: $r" }
    [System.IO.File]::WriteAllText($usnRecords, (($r | Out-String).Trim() + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
}

# A volume whose MFT slots Windows allocated, freed and reused, plus the
# file reference Windows reports for each surviving file:
# ntfs-inventory.fileids.txt holds one `fsutil file queryfileid` answer per
# line. Deleting a file and creating another reuses the freed slot with a
# bumped sequence number, so the references carry sequences above one
# (#609).
$inventoryIds = [System.IO.Path]::GetFullPath('test-disks/ntfs-inventory.fileids.txt')
New-WindowsFixture -ImagePath 'test-disks/ntfs-inventory.img' -Label 'INVENTORY' -Populate {
    param($root)
    foreach ($i in 1..8) {
        [System.IO.File]::WriteAllText((Join-Path $root "gone-$i.txt"), 'g')
    }
    foreach ($i in 1..8) { Remove-Item -LiteralPath (Join-Path $root "gone-$i.txt") }
    foreach ($i in 1..8) {
        [System.IO.File]::WriteAllText((Join-Path $root "reused-$i.txt"), 'r')
    }
    New-Item -ItemType Directory -Path (Join-Path $root 'dir') | Out-Null
    # Only the names created above are queried: a forced root listing also
    # returns System Volume Information, which Windows refuses to open for
    # fsutil (Error 5: Access is denied).
    $created = @(1..8 | ForEach-Object { "reused-$_.txt" }) + 'dir'
    $lines = foreach ($name in ($created | Sort-Object)) {
        $o = & fsutil.exe file queryfileid (Join-Path $root $name) 2>&1
        if ($LASTEXITCODE -ne 0) { throw "fsutil file queryfileid ${name}: $o" }
        "$name`t$(($o | Out-String).Trim())"
    }
    [System.IO.File]::WriteAllText($inventoryIds, (($lines -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
}

Get-Item test-disks/ntfs-attrlist.img,test-disks/ntfs-compressed.img,test-disks/ntfs-symlink.img,test-disks/ntfs-cli-read.img,test-disks/ntfs-ea.img,test-disks/ntfs-format-64k.img,test-disks/ntfs-usnjrnl.img,test-disks/ntfs-inventory.img |
    Format-Table Name,Length
