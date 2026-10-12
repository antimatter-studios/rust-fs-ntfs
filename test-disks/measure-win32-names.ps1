# Measures which candidate names a Win32 program can create as a file, and
# writes test-disks/win32-name-verdicts.tsv: one line per name, the name, a
# tab, and `file` when a file of exactly that name appeared in the directory
# or `refused` when none did.
#
# Win32 path parsing turns the reserved DOS device names (CON, NUL, COM1, and
# so on) into devices, and which spellings it turns has changed between
# Windows releases. The crate refuses exactly the names this Windows refuses
# and creates every name it creates (#498), so the verdicts are measured here
# rather than written down. tests/win32_reserved_names.rs reads the file.
#
# Each name is created through an ordinary Win32 path with CreateFileW, as a
# program would, in its own empty directory; the directory is then listed
# through a \\?\ path, which reaches any name, and removed the same way.
#
# Run from the repository root on Windows. CI runs it in the Windows-authored
# fixtures job.

$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class Win32Names {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern SafeFileHandle CreateFileW(
        string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);
}
'@

$GENERIC_WRITE = [uint32]0x40000000
$CREATE_NEW = [uint32]1
$FILE_ATTRIBUTE_NORMAL = [uint32]0x80

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$root = Join-Path $tempRoot 'win32-names'
if (Test-Path -LiteralPath $root) {
    [System.IO.Directory]::Delete("\\?\$root", $true)
}
New-Item -ItemType Directory -Path $root | Out-Null

# The device names, the superscript-digit COM and LPT ports Microsoft's
# naming page also lists, the console and clock devices, and names that
# only look like them.
$super = @([char]0x00B9, [char]0x00B2, [char]0x00B3)
$bases = @('CON', 'PRN', 'AUX', 'NUL', 'CONIN$', 'CONOUT$', 'CLOCK$')
foreach ($n in 0..9) { $bases += "COM$n"; $bases += "LPT$n" }
foreach ($s in $super) { $bases += "COM$s"; $bases += "LPT$s" }

$names = @()
foreach ($b in $bases) {
    $names += $b
    $names += $b.ToLowerInvariant()
    $names += "$b.txt"
    $names += "$b.tar.gz"
    $names += "$b .txt"
}
$names += @('CONSOLE', 'NULL', 'AUXILIARY', 'PRINTER', 'COM10', 'LPT10',
            'CON1', 'NUL_', 'x.CON', 'x.nul', 'CONx.txt', '.NUL', 'aux-file')

$lines = New-Object System.Collections.Generic.List[string]
$i = 0
foreach ($name in $names) {
    $dir = Join-Path $root "n$i"
    $i++
    New-Item -ItemType Directory -Path $dir | Out-Null
    $handle = [Win32Names]::CreateFileW("$dir\$name", $GENERIC_WRITE, 0, [IntPtr]::Zero,
        $CREATE_NEW, $FILE_ATTRIBUTE_NORMAL, [IntPtr]::Zero)
    if (-not $handle.IsInvalid) { $handle.Dispose() }
    $listed = [System.IO.Directory]::GetFileSystemEntries("\\?\$dir") |
        ForEach-Object { [System.IO.Path]::GetFileName($_) }
    $verdict = if (@($listed) -ccontains $name) { 'file' } else { 'refused' }
    $lines.Add("$name`t$verdict")
}
[System.IO.Directory]::Delete("\\?\$root", $true)

$out = Join-Path (Get-Location) 'test-disks/win32-name-verdicts.tsv'
[System.IO.File]::WriteAllText($out, ($lines -join "`n") + "`n",
    (New-Object System.Text.UTF8Encoding($false)))
$build = [System.Environment]::OSVersion.Version
Write-Output "win32 names: $($lines.Count) measured on Windows $build into $out"
