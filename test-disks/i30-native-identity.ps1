# Independent live-handle identity contract for the Windows $I30 capture.
# API layouts: https://learn.microsoft.com/windows/win32/api/fileapi/ns-fileapi-by_handle_file_information
# https://learn.microsoft.com/windows/win32/api/winbase/ns-winbase-file_id_info
# These are Windows API buffers, not descriptions of on-disk index entries.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class I30NativeIdentity {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandle(SafeFileHandle handle, [Out] byte[] info);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandleEx(SafeFileHandle handle, int kind,
        [Out] byte[] info, uint size);
    public static byte[][] Read(string path) {
        // FILE_FLAG_BACKUP_SEMANTICS also permits opening the directory.
        using (var h = CreateFileW(path, 0, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero)) {
            if (h.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            var basic = new byte[52];
            var extended = new byte[24];
            if (!GetFileInformationByHandle(h, basic))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            // FileIdInfo = 18; retain all 128 identifier bits without conversion.
            if (!GetFileInformationByHandleEx(h, 18, extended, 24))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            return new byte[][] { basic, extended };
        }
    }
}
'@

function Get-I30NativeIdentity([string]$Path) {
    $out = (& fsutil file queryFileID $Path) -join ' '
    if ($LASTEXITCODE -ne 0 -or $out -notmatch '0x([0-9a-fA-F]+)') {
        throw "fsutil gave no file ID for ${Path}: $out"
    }
    $hex = $Matches[1]
    # Parse the entire fsutil number. A value outside the 64-bit NTFS API
    # contract fails explicitly instead of dropping its leading digits.
    $number = [System.Numerics.BigInteger]::Parse(('0' + $hex), [System.Globalization.NumberStyles]::HexNumber)
    if ($number -gt [uint64]::MaxValue) { throw "fsutil ID exceeds the NTFS 64-bit contract: $hex" }
    $id = [uint64]$number
    $api = [I30NativeIdentity]::Read($Path)
    $high = [BitConverter]::ToUInt32($api[0], 44)
    $low = [BitConverter]::ToUInt32($api[0], 48)
    $handleId = ([uint64]$high -shl 32) -bor [uint64]$low
    if ($id -ne $handleId) {
        throw "IDENTITY fsutil/GetFileInformationByHandle disagreement for ${Path}: fsutil=$hex handle=$($handleId.ToString('x16'))"
    }
    return [ordered]@{
        file_id = $id
        handle_file_id = $handleId
        fsutil_hex = $hex
        by_handle_information_hex = [Convert]::ToHexString($api[0]).ToLowerInvariant()
        file_id_info_hex = [Convert]::ToHexString($api[1]).ToLowerInvariant()
    }
}
