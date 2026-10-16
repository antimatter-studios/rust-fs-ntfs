# Dot-sourced by the existing Windows fixture builder; uses its mounted image.
# Native IDs and the complete Windows FSCTL output are retained without edits.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class DirectoryOracleNative {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern SafeFileHandle CreateFileW(string p, uint a, uint s,
        IntPtr sec, uint d, uint f, IntPtr t);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool DeviceIoControl(SafeFileHandle h, uint c, byte[] i,
        uint il, byte[] o, uint ol, out uint n, IntPtr overlap);
    public static byte[] Record(string volume, ulong ordinal) {
        using (var h = CreateFileW(volume, 0x80000000, 7, IntPtr.Zero, 3, 0, IntPtr.Zero)) {
            if (h.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            // Query the native record size, rather than assume 1024 bytes.
            var data = new byte[1024]; uint count;
            if (!DeviceIoControl(h, 0x90064, null, 0, data, (uint)data.Length,
                                 out count, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            if (count < 64) throw new Exception("short NTFS_VOLUME_DATA_BUFFER");
            uint size = BitConverter.ToUInt32(data, 48);
            if (size < 48 || size > 65536) throw new Exception("invalid file record size");
            var output = new byte[16 + size];
            var input = BitConverter.GetBytes(ordinal);
            if (!DeviceIoControl(h, 0x90068, input, 8, output, (uint)output.Length,
                                 out count, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            Array.Resize(ref output, checked((int)count));
            return output;
        }
    }
}
'@

function New-PythonDirectoryOracle {
    param([string]$Root)
    $dir = Join-Path $Root 'oracle-dir'
    New-Item -ItemType Directory -Path $dir | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $dir 'empty') | Out-Null
    $unicodeDir = "dir-$([char]0x65E5)$([char]0x672C)"
    New-Item -ItemType Directory -Path (Join-Path $dir $unicodeDir) | Out-Null
    [IO.File]::WriteAllText((Join-Path $dir 'ascii.txt'), 'ascii')
    [IO.File]::WriteAllText((Join-Path $dir "na$([char]0x00EF)ve-$([char]0x65E5)$([char]0x672C).txt"), 'unicode')
    # Enumerate only our controlled directories, including both empty cases.
    $listings = foreach ($relative in @('oracle-dir', 'oracle-dir/empty', "oracle-dir/$unicodeDir")) {
        $path = Join-Path $Root $relative
        # The ABI also synthesizes dot entries. Query the native self/parent
        # handles explicitly; do not enumerate any unrelated root entries.
        $items = @(
            @{ Item = (Get-Item -LiteralPath $path); Name = '.' },
            @{ Item = (Get-Item -LiteralPath (Split-Path -Parent $path)); Name = '..' }
        ) + @(Get-ChildItem -LiteralPath $path -Force | ForEach-Object {
            @{ Item = $_; Name = $_.Name }
        })
        $entries = @(foreach ($item in $items) {
            $entry = $item.Item
            $answer = @(& fsutil.exe file queryfileid $entry.FullName 2>&1)
            if ($LASTEXITCODE -ne 0) { throw "queryfileid failed: $answer" }
            $text = ($answer | Out-String).Trim()
            $match = [regex]::Match($text, '0x[0-9a-fA-F]+')
            if (-not $match.Success) { throw "no native ID: $text" }
            $id = [System.Numerics.BigInteger]::Parse(('0' + $match.Value.Substring(2)),
                [System.Globalization.NumberStyles]::AllowHexSpecifier)
            if ($id -gt [uint64]::MaxValue) { throw 'native ID exceeds NTFS 64-bit reference' }
            # Documented FILE_REFERENCE: 48-bit ordinal plus 16-bit sequence.
            # The consumer validates BOTH against the Windows-returned record.
            $ordinal = [uint64]($id % [System.Numerics.BigInteger]281474976710656)
            $record = [DirectoryOracleNative]::Record(('\\.\' + $Root.TrimEnd('\')), $ordinal)
            [ordered]@{
                name = $item.Name
                native_path = $entry.FullName
                directory = [bool]$entry.PSIsContainer
                attributes = [int]$entry.Attributes
                native_id = $match.Value
                queryfileid_output = $text
                fsctl_output_hex = [Convert]::ToHexString($record)
            }
        })
        [ordered]@{ path = ('/' + $relative); entries = $entries }
    }
    return [ordered]@{ schema = 1; listings = @($listings) }
}
