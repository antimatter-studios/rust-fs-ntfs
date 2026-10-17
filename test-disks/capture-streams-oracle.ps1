# Names and lengths come from Get-Item; allocation comes from FileStreamInfo.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public static class WindowsStreams {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandleEx(SafeFileHandle h, int kind,
        [Out] byte[] info, uint size);
    public static Dictionary<string, long> Allocations(string path) {
        using (var h = CreateFileW(path, 0x80, 7, IntPtr.Zero, 3, 0x02200000, IntPtr.Zero)) {
            if (h.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), path);
            var data = new byte[65536];
            if (!GetFileInformationByHandleEx(h, 7, data, (uint)data.Length))
                throw new Win32Exception(Marshal.GetLastWin32Error(), path);
            var result = new Dictionary<string, long>();
            int offset = 0;
            do {
                int next = BitConverter.ToInt32(data, offset);
                int count = BitConverter.ToInt32(data, offset + 4);
                string name = Encoding.Unicode.GetString(data, offset + 24, count);
                if (name != "::$DATA" && name.StartsWith(":") && name.EndsWith(":$DATA"))
                    result.Add(name.Substring(1, name.Length - 7), BitConverter.ToInt64(data, offset + 16));
                if (next == 0) break;
                offset += next;
            } while (offset < data.Length);
            return result;
        }
    }
}
'@

function Write-NamedStreamOracle {
    param([string]$File, [string]$VolumePath, [string]$Report)
    $allocated = [WindowsStreams]::Allocations($File)
    $lines = foreach ($stream in @(Get-Item -LiteralPath $File -Stream * -Force)) {
        if ($stream.Stream -eq ':$DATA') { continue }
        if (-not $allocated.ContainsKey($stream.Stream)) { throw "FileStreamInfo lost $($stream.Stream)" }
        "$VolumePath`t$($stream.Stream)`t$($stream.Length)`t$($allocated[$stream.Stream])"
    }
    [IO.File]::AppendAllText($Report, (($lines -join "`n") + "`n"),
        [Text.UTF8Encoding]::new($false))
}
