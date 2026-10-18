# Builds the fixtures whose shapes only Windows can author:
#   * ntfs-attrlist.img: named $DATA streams overflow into extension records
#   * ntfs-compressed.img: classic NTFS/LZNT1-compressed $DATA
#   * ntfs-symlink.img: symlinks Windows created, plus
#     ntfs-symlink.targets.tsv, the target Windows itself reports for each
#   * ntfs-cli-read.img: files Windows wrote, plus ntfs-cli-read.sha256.tsv,
#     each file's path, size and the SHA-256 Windows itself computes
#   * ntfs-ea.img: extended attributes Windows set through NtSetEaFile, plus
#     ntfs-ea.names.tsv, each name and value as it was handed to Windows;
#     ntfs-ea.query.tsv, stored names, sizes, flags and bytes from NtQueryEaFile;
#     ea-summary-{0,1,2}.bin distinguish compact/query sizes and NEED_EA counts;
#     ea-large.bin, ea-full.bin and ea-shrunk.bin carry EA sets no record
#     holds resident
#   * ntfs-format-64k.img: an empty volume Windows formatted with 64 KiB
#     clusters, whose $MFTMirr the formatter is held to
#   * ntfs-usnjrnl.img: a volume with an active change journal, plus
#     ntfs-usnjrnl.query.txt and ntfs-usnjrnl.records.csv, what fsutil
#     reports of the journal and of every record in it
#   * ntfs-stat.img: one of each thing `fs.ntfs stat` reports on, plus
#     ntfs-stat.windows.tsv, what Windows reports for each path
#
# Run from the repository root on an elevated Windows host. rust-img-vhd must be
# on PATH; CI installs the pinned rust-img-vhd release before invoking this.

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"
. "$PSScriptRoot\capture-python-directory-oracle.ps1"
. "$PSScriptRoot\windows-fixture.ps1"
. "$PSScriptRoot\capture-streams-oracle.ps1"

$attrStreamsReport = [IO.Path]::GetFullPath('test-disks/ntfs-attrlist.streams.tsv')
[IO.File]::WriteAllText($attrStreamsReport, '')

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
    Write-NamedStreamOracle $file '/many.bin' $attrStreamsReport
}

# Stream names whose order differs under ordinal UTF-16, upcase folding,
# length-first and code-point comparison (#1047): B/a/Ac/ab/abc for case,
# prefix and length, '_' between the cases, e9/ca and ff/100 for Latin-1 and
# Latin Extended upcase, ff41 against a surrogate pair for UTF-16 against code
# points. No two names fold to one, so each stays its own stream. The padding
# only forces the $ATTRIBUTE_LIST. Every name is reported as UTF-16 code units
# in ntfs-attrlist-names.names.tsv; tests/attrlist_name_order_windows.rs keeps
# the order Windows wrote them in.
$attrNamesReport = [IO.Path]::GetFullPath('test-disks/ntfs-attrlist-names.names.tsv')
New-WindowsFixture -ImagePath 'test-disks/ntfs-attrlist-names.img' -Label 'ATTRNAMES' -Populate {
    param($root)
    $file = Join-Path $root 'names.bin'
    [System.IO.File]::WriteAllBytes($file, [byte[]](0x42))
    $names = @('B', 'a', 'Ac', 'ab', 'abc', '_',
        "$([char]0x00E9)", "$([char]0x00CA)", "$([char]0x00FF)", "$([char]0x0100)",
        "$([char]0xFF41)", "$([char]0xD83D)$([char]0xDE00)")
    for ($i = 0; $i -lt 96; $i++) { $names += 'pad-{0:D3}' -f $i }
    $lines = foreach ($name in $names) {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes("payload-$name")
        [System.IO.File]::WriteAllBytes("${file}:$name", $bytes)
        ($name.ToCharArray() | ForEach-Object { '{0:x4}' -f [int]$_ }) -join ' '
    }
    [IO.File]::WriteAllText($attrNamesReport, (($lines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
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
        'random.bin'           = 3145729
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
    & chkdsk.exe ($root.TrimEnd('\')) | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'chkdsk rejected the Windows CLI read/export fixture' }
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

    // Microsoft FILE_FULL_EA_INFORMATION and ZwQueryEaFile document this
    // buffer and call: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-zwqueryeafile
    [DllImport("ntdll.dll")]
    static extern int NtQueryEaFile(SafeFileHandle handle, out IoStatusBlock status,
        [Out] byte[] buffer, uint length, byte single, IntPtr list,
        uint listLength, IntPtr index, byte restart);

    public static string[] Query(SafeFileHandle handle) {
        var buffer = new byte[128 * 1024];
        IoStatusBlock status;
        int rc = NtQueryEaFile(handle, out status, buffer, (uint)buffer.Length,
            0, IntPtr.Zero, 0, IntPtr.Zero, 1);
        if (rc != 0) throw new Exception("NtQueryEaFile: " + rc.ToString("X8"));
        var rows = new System.Collections.Generic.List<string>();
        int at = 0, used = checked((int)status.Information.ToInt64());
        while (true) {
            if (at + 8 > used) throw new Exception("short EA header");
            int next = checked((int)BitConverter.ToUInt32(buffer, at));
            int n = buffer[at + 5], v = BitConverter.ToUInt16(buffer, at + 6);
            if (at + 9 + n + v > used) throw new Exception("short EA value");
            string name = System.Text.Encoding.ASCII.GetString(buffer, at + 8, n);
            string hex = BitConverter.ToString(buffer, at + 9 + n, v).Replace("-", "").ToLowerInvariant();
            rows.Add(name + "\t" + v + "\t" + buffer[at + 4] + "\t" + hex);
            if (next == 0) break;
            if (next < 9 + n + v) throw new Exception("overlapping EA entries");
            at = checked(at + next);
        }
        return rows.ToArray();
    }
}
'@
$eaReport = [System.IO.Path]::GetFullPath('test-disks/ntfs-ea.names.tsv')
$eaQueryReport = [System.IO.Path]::GetFullPath('test-disks/ntfs-ea.query.tsv')
[System.IO.File]::WriteAllText($eaQueryReport, '')
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

    # A binary value proves the CLI does not decode, wrap or terminate it.
    $file = Join-Path $root 'ea-binary.bin'
    [System.IO.File]::WriteAllBytes($file, [byte[]](0x45))
    $handle = [WindowsEa]::CreateFileW($file, [uint32]0x100018, [uint32]7, [IntPtr]::Zero,
        [uint32]3, [uint32]0x80, [IntPtr]::Zero)
    if ($handle.IsInvalid) { throw 'open ea-binary.bin for EAs failed' }
    try {
        $name = [Text.Encoding]::ASCII.GetBytes('USER.BINARY')
        $buf = New-Object byte[] (9 + $name.Length + 4)
        $buf[5] = [byte]$name.Length
        $buf[6] = 4
        $name.CopyTo($buf, 8)
        ([byte[]](0, 255, 128, 10)).CopyTo($buf, 9 + $name.Length)
        $status = New-Object WindowsEa+IoStatusBlock
        $rc = [WindowsEa]::NtSetEaFile($handle, [ref]$status, $buf, [uint32]$buf.Length)
        if ($rc -ne 0) { throw ("NtSetEaFile binary: {0:X8}" -f $rc) }
    } finally { $handle.Dispose() }

    # EA sets too large for a resident $EA in any 1 KiB record, so the
    # layout Windows gives them is what tests/non_resident_ea.rs holds this
    # crate's to (#661). Each change is one NtSetEaFile call; EA i holds
    # "ea <i> " repeated to its size, and a change of no value removes it.
    #   ea-large.bin   USER.E1..USER.E3 of 2000 bytes each
    #   ea-full.bin    USER.E1..USER.E4 of 15000 bytes each
    #   ea-shrunk.bin  USER.E1 of 2000 bytes and USER.E2 of 10, then USER.E1
    #                  removed: whether Windows takes $EA back into the record
    $eaChanges = [ordered]@{
        'ea-large.bin'  = @(@(1, 2000), @(2, 2000), @(3, 2000))
        'ea-full.bin'   = @(@(1, 15000), @(2, 15000), @(3, 15000), @(4, 15000))
        'ea-shrunk.bin' = @(@(1, 2000), @(2, 10), @(1, 0))
    }
    foreach ($fileName in $eaChanges.Keys) {
        $file = Join-Path $root $fileName
        [System.IO.File]::WriteAllBytes($file, [byte[]](0x45))
        $handle = [WindowsEa]::CreateFileW($file, [uint32]0x100018, [uint32]7, [IntPtr]::Zero,
            [uint32]3, [uint32]0x80, [IntPtr]::Zero)
        if ($handle.IsInvalid) {
            throw "open ${fileName} for EAs: Win32 error $([System.Runtime.InteropServices.Marshal]::GetLastWin32Error())"
        }
        try {
            foreach ($change in $eaChanges[$fileName]) {
                $i = $change[0]
                $size = $change[1]
                $n = [System.Text.Encoding]::ASCII.GetBytes("USER.E$i")
                $unit = [System.Text.Encoding]::ASCII.GetBytes("ea $i ")
                $buf = New-Object byte[] (8 + $n.Length + 1 + $size)
                $buf[5] = [byte]$n.Length
                [BitConverter]::GetBytes([uint16]$size).CopyTo($buf, 6)
                $n.CopyTo($buf, 8)
                for ($b = 0; $b -lt $size; $b++) { $buf[8 + $n.Length + 1 + $b] = $unit[$b % $unit.Length] }
                $status = New-Object WindowsEa+IoStatusBlock
                $rc = [WindowsEa]::NtSetEaFile($handle, [ref]$status, $buf, [uint32]$buf.Length)
                if ($rc -ne 0) { throw ("NtSetEaFile {0} USER.E{1}: NTSTATUS 0x{2:X8}" -f $fileName, $i, $rc) }
            }
        } finally { $handle.Dispose() }
    }
    # Query through Windows, independently of our parser: 18 EAs across
    # eight files, including NEED_EA, non-resident storage and binary content.
    foreach ($file in Get-ChildItem -LiteralPath $root -Filter 'ea*.bin') {
        $handle = [WindowsEa]::CreateFileW($file.FullName, [uint32]0x100008, [uint32]7,
            [IntPtr]::Zero, [uint32]3, [uint32]0x80, [IntPtr]::Zero)
        if ($handle.IsInvalid) { throw "open $($file.Name) for EA query failed" }
        try {
            $lines = @([WindowsEa]::Query($handle) | ForEach-Object { "/$($file.Name)`t$_" })
            [IO.File]::AppendAllText($eaQueryReport, (($lines -join "`n") + "`n"),
                [Text.UTF8Encoding]::new($false))
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

# What Windows reports for one path, through a handle that opens a symlink
# or junction itself rather than what it names (FILE_FLAG_OPEN_REPARSE_POINT;
# FILE_FLAG_BACKUP_SEMANTICS so a directory opens at all). Read gives the
# file ID, the link count, the attributes, the reparse tag, the end of file
# and allocation sizes, then the four $STANDARD_INFORMATION times as
# FILETIMEs: creation, last access, last write, change. SetTimes sets those
# four through FILE_BASIC_INFO, which leaves the attributes alone when they
# are zero.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class WindowsStat {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandle(SafeFileHandle handle, [Out] byte[] info);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandleEx(SafeFileHandle handle, int kind,
        [Out] byte[] info, uint size);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetFileInformationByHandle(SafeFileHandle handle, int kind,
        byte[] info, uint size);

    static SafeFileHandle Open(string path, uint access) {
        var h = CreateFileW(path, access, 7, IntPtr.Zero, 3, 0x02000000 | 0x00200000, IntPtr.Zero);
        if (h.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), path);
        return h;
    }

    static byte[] Query(SafeFileHandle h, int kind, uint size, string path) {
        var info = new byte[size];
        if (!GetFileInformationByHandleEx(h, kind, info, size))
            throw new Win32Exception(Marshal.GetLastWin32Error(), path);
        return info;
    }

    public static void SetTimes(string path, long created, long accessed, long written, long changed) {
        var basic = new byte[40];
        BitConverter.GetBytes(created).CopyTo(basic, 0);
        BitConverter.GetBytes(accessed).CopyTo(basic, 8);
        BitConverter.GetBytes(written).CopyTo(basic, 16);
        BitConverter.GetBytes(changed).CopyTo(basic, 24);
        // FILE_WRITE_ATTRIBUTES; FileBasicInfo = 0.
        using (var h = Open(path, 0x100)) {
            if (!SetFileInformationByHandle(h, 0, basic, 40))
                throw new Win32Exception(Marshal.GetLastWin32Error(), path);
        }
    }

    public static string[] Read(string path) {
        // FILE_READ_ATTRIBUTES, which moves no time.
        using (var h = Open(path, 0x80)) {
            var byHandle = new byte[52];
            if (!GetFileInformationByHandle(h, byHandle))
                throw new Win32Exception(Marshal.GetLastWin32Error(), path);
            var basic = Query(h, 0, 40, path);     // FileBasicInfo
            var standard = Query(h, 1, 24, path);  // FileStandardInfo
            var tag = Query(h, 9, 8, path);        // FileAttributeTagInfo
            ulong id = ((ulong)BitConverter.ToUInt32(byHandle, 44) << 32)
                | BitConverter.ToUInt32(byHandle, 48);
            return new string[] {
                id.ToString("x16"),
                BitConverter.ToUInt32(byHandle, 40).ToString(),
                BitConverter.ToUInt32(basic, 32).ToString(),
                BitConverter.ToUInt32(tag, 4).ToString("x8"),
                BitConverter.ToInt64(standard, 8).ToString(),
                BitConverter.ToInt64(standard, 0).ToString(),
                BitConverter.ToInt64(basic, 0).ToString(),
                BitConverter.ToInt64(basic, 8).ToString(),
                BitConverter.ToInt64(basic, 16).ToString(),
                BitConverter.ToInt64(basic, 24).ToString(),
            };
        }
    }
}
'@

# A volume holding one of each thing `fs.ntfs stat` reports on, plus
# ntfs-stat.windows.tsv: for every path, what Windows itself reports of it
# (WindowsStat.Read), the names `fsutil hardlink list` gives a file, the
# named streams `Get-Item -Stream *` finds on it, and the EAs Windows
# accepted for it (tests/cli-oracle/test-stat-windows.sh, #642).
#
# Every time is set explicitly, last, each to a different value with its
# own fraction of a second, and one before 1970: NTFS defers writing a
# last-access time it updates itself, so a time Windows chose could be
# reported before it is on the disk, and four times a moment apart would
# not show two of them swapped. The root's times are not set: Windows
# keeps writing System Volume Information there while it is mounted.
$statReport = [System.IO.Path]::GetFullPath('test-disks/ntfs-stat.windows.tsv')
$statStreamsReport = [IO.Path]::GetFullPath('test-disks/ntfs-stat.streams.tsv')
[IO.File]::WriteAllText($statStreamsReport, '')
New-WindowsFixture -ImagePath 'test-disks/ntfs-stat.img' -Label 'STAT' -Populate {
    param($root)
    $at = {
        param($rel)
        if ($rel -eq '/') { return $root }
        Join-Path $root ($rel.TrimStart('/') -replace '/', '\')
    }
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $bytes = New-Object byte[] 5000
    $rng.GetBytes($bytes)
    [System.IO.File]::WriteAllBytes((& $at '/plain.txt'), $bytes)
    [System.IO.File]::WriteAllText((& $at '/small.txt'), 'resident')
    New-Item -ItemType Directory -Path (& $at '/dir') | Out-Null
    [System.IO.File]::WriteAllText((& $at '/dir/inner.txt'), 'inner')
    [System.IO.File]::WriteAllText((& $at '/hard-a.txt'), 'linked')
    New-Item -ItemType HardLink -Path (& $at '/dir/hard-b.txt') -Target (& $at '/hard-a.txt') | Out-Null
    New-Item -ItemType SymbolicLink -Path (& $at '/link-file') -Value 'plain.txt' | Out-Null
    New-Item -ItemType Junction -Path (& $at '/link-dir') -Value (& $at '/dir') | Out-Null
    [System.IO.File]::WriteAllText((& $at '/ads.txt'), 'main')
    [System.IO.File]::WriteAllText((& $at '/ads.txt') + ':one', 'first stream')
    [System.IO.File]::WriteAllText((& $at '/ads.txt') + ':two', 'second stream')
    [IO.File]::WriteAllBytes((& $at '/ads.txt') + ':empty', [byte[]]@())
    $streamBytes = [byte[]]::new(2097169)
    $rng.GetBytes($streamBytes)
    [IO.File]::WriteAllBytes((& $at '/ads.txt') + ':large', $streamBytes)
    [IO.File]::WriteAllBytes((& $at '/ads.txt') + ":na$([char]0x00EF)ve", [byte[]](1, 2, 3))
    [IO.File]::WriteAllBytes((& $at '/dir') + ':directory-data', [byte[]](7, 8, 9))
    Write-NamedStreamOracle (& $at '/ads.txt') '/ads.txt' $statStreamsReport
    Write-NamedStreamOracle (& $at '/dir') '/dir' $statStreamsReport
    [System.IO.File]::WriteAllText((& $at '/old.txt'), 'old')
    [System.IO.File]::WriteAllText((& $at '/attrs.txt'), 'attributes')

    # Two EAs through NtSetEaFile, as for ntfs-ea.img above.
    [System.IO.File]::WriteAllText((& $at '/ea.txt'), 'ea')
    $eas = [ordered]@{ '/ea.txt' = 2 }
    $handle = [WindowsEa]::CreateFileW((& $at '/ea.txt'), [uint32]0x100018, [uint32]7,
        [IntPtr]::Zero, [uint32]3, [uint32]0x80, [IntPtr]::Zero)
    if ($handle.IsInvalid) {
        throw "open ea.txt for EAs: Win32 error $([System.Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    try {
        foreach ($ea in @(@('STAT.ONE', 'one'), @('STAT.TWO', 'two'))) {
            $n = [System.Text.Encoding]::ASCII.GetBytes($ea[0])
            $v = [System.Text.Encoding]::ASCII.GetBytes($ea[1])
            $buf = New-Object byte[] (8 + $n.Length + 1 + $v.Length)
            $buf[5] = [byte]$n.Length
            [BitConverter]::GetBytes([uint16]$v.Length).CopyTo($buf, 6)
            $n.CopyTo($buf, 8)
            $v.CopyTo($buf, 8 + $n.Length + 1)
            $status = New-Object WindowsEa+IoStatusBlock
            $rc = [WindowsEa]::NtSetEaFile($handle, [ref]$status, $buf, [uint32]$buf.Length)
            if ($rc -ne 0) { throw ("NtSetEaFile {0}: NTSTATUS 0x{1:X8}" -f $ea[0], $rc) }
        }
    } finally { $handle.Dispose() }
    [System.IO.File]::SetAttributes((& $at '/attrs.txt'), 'Hidden, ReadOnly, NotContentIndexed')

    $paths = @('/', '/plain.txt', '/small.txt', '/dir', '/dir/inner.txt', '/hard-a.txt',
        '/dir/hard-b.txt', '/link-file', '/link-dir', '/ads.txt', '/ea.txt', '/attrs.txt', '/old.txt')
    $files = @('/plain.txt', '/small.txt', '/dir/inner.txt', '/hard-a.txt', '/dir/hard-b.txt',
        '/ads.txt', '/ea.txt', '/attrs.txt', '/old.txt')
    $names = @{}
    $streams = @{}
    foreach ($rel in $files) {
        $o = & fsutil.exe hardlink list (& $at $rel) 2>&1
        if ($LASTEXITCODE -ne 0) { throw "fsutil hardlink list ${rel}: $o" }
        $names[$rel] = (@($o | ForEach-Object { "$_".Trim() -replace '\\', '/' } |
            Where-Object { $_ }) -join '|')
        $streams[$rel] = @(Get-Item -LiteralPath (& $at $rel) -Stream * -Force |
            Where-Object { $_.Stream -ne ':$DATA' }).Count
    }

    # 2001-02-03 04:05:06 UTC, a day apart per path; the last access an
    # hour after the creation, the last write two, the change three.
    $timed = @('/plain.txt', '/small.txt', '/dir/inner.txt', '/hard-a.txt', '/link-file',
        '/ads.txt', '/ea.txt', '/attrs.txt', '/dir', '/link-dir')
    $hour = 36000000000
    for ($i = 0; $i -lt $timed.Count; $i++) {
        $created = 126256467060000000 + $i * 24 * $hour + 1234567
        [WindowsStat]::SetTimes((& $at $timed[$i]), $created, $created + $hour + 1111111,
            $created + 2 * $hour + 2222222, $created + 3 * $hour + 3333333)
    }
    # 1960-01-02 03:04:05.5 UTC and after: signed seconds, positive nanoseconds.
    $old = 113289518455000000
    [WindowsStat]::SetTimes((& $at '/old.txt'), $old, $old + 1, $old + 9999999, $old + 10000001)

    $lines = foreach ($rel in $paths) {
        $read = [WindowsStat]::Read((& $at $rel))
        $n = if ($names.ContainsKey($rel)) { $names[$rel] } else { '-' }
        $s = if ($streams.ContainsKey($rel)) { $streams[$rel] } else { '-' }
        $e = if ($eas.Contains($rel)) { $eas[$rel] } else { 0 }
        (@($rel) + $read + @($n, $s, $e)) -join "`t"
    }
    [System.IO.File]::WriteAllText($statReport, (($lines -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
}

# Get-ChildItem supplies the paths; WindowsStat opens the reparse itself
# for its metadata. No expected entry comes from this crate (#644).
$walkReport = [IO.Path]::GetFullPath('test-disks/ntfs-ls-recursive.windows.tsv')
New-WindowsFixture -ImagePath 'test-disks/ntfs-ls-recursive.img' -Label 'LSRECURSIVE' -Populate {
    param($root)
    $system = Join-Path $root 'System Volume Information'
    if (Test-Path -LiteralPath $system) {
        & takeown.exe /F $system /A | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'takeown of fixture system directory failed' }
        & icacls.exe $system /grant '*S-1-5-32-544:(OI)(CI)F' /T | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'grant of fixture enumeration access failed' }
    }
    foreach ($dir in @('a/b/c', 'empty')) {
        New-Item -ItemType Directory -Path (Join-Path $root $dir) -Force | Out-Null
    }
    foreach ($file in @('a/b/c/deep', 'a/b/file', 'a/hard', 'a/with space', 'B', "$([char]0x00E9)")) {
        [IO.File]::WriteAllText((Join-Path $root $file), "contents of $file")
    }
    New-Item -ItemType HardLink -Path (Join-Path $root 'z') -Target (Join-Path $root 'a/hard') | Out-Null
    New-Item -ItemType SymbolicLink -Path (Join-Path $root 'dir-link') -Value 'a' | Out-Null
    New-Item -ItemType SymbolicLink -Path (Join-Path $root 'a/back') -Value '..' | Out-Null
    New-Item -ItemType SymbolicLink -Path (Join-Path $root 'dangling') -Value 'missing' | Out-Null
    New-Item -ItemType Junction -Path (Join-Path $root 'junction') -Value (Join-Path $root 'a') | Out-Null
    # Include root's empty directories too; never follow a reparse point.
    $items = @(Get-ChildItem -LiteralPath $root -Force) +
        @(Get-ChildItem -LiteralPath $root -Recurse -Force -Attributes !ReparsePoint) +
        @(Get-ChildItem -LiteralPath $root -Recurse -Force -Attributes ReparsePoint)
    $lines = foreach ($item in ($items | Sort-Object FullName -Unique)) {
        $rel = '/' + $item.FullName.Substring($root.Length).Replace('\', '/')
        $info = [WindowsStat]::Read($item.FullName)
        $kind = if ($info[3] -eq 'a000000c') { 'symlink' }
            elseif ($info[3] -eq 'a0000003') { 'junction' }
            elseif ($item.PSIsContainer) { 'dir' } else { 'file' }
        $record = [Convert]::ToUInt64($info[0], 16) -band 0xFFFFFFFFFFFFL
        "$rel`t$kind`t$($info[4])`t$record`t$($rel.Split('/').Count - 1)"
    }
    [IO.File]::WriteAllText($walkReport, (($lines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
}

Get-Item test-disks/ntfs-attrlist.img,test-disks/ntfs-attrlist-names.img,test-disks/ntfs-compressed.img,test-disks/ntfs-symlink.img,test-disks/ntfs-cli-read.img,test-disks/ntfs-ea.img,test-disks/ntfs-format-64k.img,test-disks/ntfs-usnjrnl.img,test-disks/ntfs-inventory.img,test-disks/ntfs-stat.img,test-disks/ntfs-ls-recursive.img |
    Format-Table Name,Length
