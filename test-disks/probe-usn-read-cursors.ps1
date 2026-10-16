# Records what FSCTL_READ_USN_JOURNAL does with three cursors on the change
# journal of the disposable volume capture-usn-journal-data.ps1 created (#609).
#
# Read-only: the journal is opened for reading and queried with
# FSCTL_QUERY_USN_JOURNAL and FSCTL_READ_USN_JOURNAL only. It is never
# created, deleted or reset, and no file is written to the volume being
# probed except this manifest. The three cursors are the journal's own
# NextUsn, a USN 16 MiB beyond it, and NextUsn under a journal ID that differs
# from this journal's in its lowest bit. Each read is non-blocking
# (BytesToWaitFor 0) through READ_USN_JOURNAL_DATA_V0.
#
# Nothing here decides what Windows should answer. Each probe row records
# whether DeviceIoControl succeeded, GetLastError when it did not, the byte
# count, and the next USN Windows returned. The manifest names the SHA-256 of
# test-disks/usn-journal-data.tsv, the fsutil capture of the same journal, so
# tests/usn_cursor_read_probe_windows.rs can tie the two together.
#
# Output: test-disks/usn-read-probe.tsv. capture-usn-journal-data.ps1 invokes
# this with -Volume while its disposable volume is still attached, so both
# files describe one journal. The runner's working and system drives are
# refused, and the probe fails when the journal it reads is not the one the
# capture recorded; the manifest is written first so that evidence survives.
# -JournalCapture and -Out exist for the capture script's wrong-journal control,
# which reads a copy of the capture metadata and writes a separate manifest.
param(
    [Parameter(Mandatory = $true)][string]$Volume,
    [string]$JournalCapture = 'test-disks/usn-journal-data.tsv',
    [string]$Out = 'test-disks/usn-read-probe.tsv'
)

$ErrorActionPreference = 'Stop'

if ($Volume -notmatch '^[A-Za-z]:$') { throw "-Volume must be a drive such as E:, got '$Volume'" }
$Volume = $Volume.ToUpperInvariant()
foreach ($refused in @(((Get-Location).Drive.Name + ':'), $env:SystemDrive)) {
    if ($refused -and $Volume -eq $refused.ToUpperInvariant()) {
        throw "refusing to probe ${Volume}: it is a runner drive, not the disposable capture volume"
    }
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class UsnReadProbe {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share,
        IntPtr sa, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle h, uint code, byte[] inBuf,
        int inLen, byte[] outBuf, int outLen, out int returned, IntPtr ov);
    const uint FSCTL_QUERY_USN_JOURNAL = 0x000900F4;
    const uint FSCTL_READ_USN_JOURNAL = 0x000900BB;

    public static SafeFileHandle Open(string drive) {
        var h = CreateFileW(@"\\.\" + drive, 0x80000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
        if (h.IsInvalid) throw new System.ComponentModel.Win32Exception();
        return h;
    }

    // USN_JOURNAL_DATA_V0: JournalID, FirstUsn, NextUsn, LowestValidUsn,
    // MaxUsn, MaximumSize, AllocationDelta, eight bytes each.
    public static long[] Query(SafeFileHandle h) {
        var o = new byte[80]; int n;
        if (!DeviceIoControl(h, FSCTL_QUERY_USN_JOURNAL, null, 0, o, o.Length, out n, IntPtr.Zero))
            throw new System.ComponentModel.Win32Exception();
        var r = new long[7];
        for (int k = 0; k < 7; k++) r[k] = BitConverter.ToInt64(o, k * 8);
        return r;
    }

    // One non-blocking FSCTL_READ_USN_JOURNAL call. Returns
    // { ok, GetLastError, bytes returned, next USN (0 when not ok) }.
    public static long[] Read(SafeFileHandle h, long journalId, long startUsn) {
        var i = new byte[40];
        BitConverter.GetBytes(startUsn).CopyTo(i, 0);
        BitConverter.GetBytes(0xFFFFFFFFu).CopyTo(i, 8);
        BitConverter.GetBytes(journalId).CopyTo(i, 32);
        var o = new byte[65536]; int n;
        bool ok = DeviceIoControl(h, FSCTL_READ_USN_JOURNAL, i, i.Length, o, o.Length, out n, IntPtr.Zero);
        long err = ok ? 0 : Marshal.GetLastWin32Error();
        long next = (ok && n >= 8) ? BitConverter.ToInt64(o, 0) : 0;
        return new long[] { ok ? 1 : 0, err, ok ? n : 0, next };
    }
}
'@

function Hex([long]$v) { '0x{0:x16}' -f $v }

$volume = $Volume
$journalCapture = [System.IO.Path]::GetFullPath($JournalCapture)
if (-not (Test-Path -LiteralPath $journalCapture)) {
    throw "$journalCapture is missing: run test-disks/capture-usn-journal-data.ps1 first"
}
$sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $journalCapture).Hash.ToLowerInvariant()
$capturedId = Get-Content -LiteralPath $journalCapture |
    Where-Object { $_ -match '^journal_id\t(0x[0-9a-fA-F]+)$' } |
    ForEach-Object { [Convert]::ToUInt64($Matches[1].Substring(2), 16) } |
    Select-Object -First 1
if ($null -eq $capturedId) { throw "$journalCapture has no journal_id row" }
$out = [System.IO.Path]::GetFullPath($Out)

$h = [UsnReadProbe]::Open($volume)
try {
    $q = [UsnReadProbe]::Query($h)
    $id = $q[0]; $next = $q[2]
    $cursors = @(
        @('current', $id, $next),
        # Parenthesised: the comma binds tighter than +, so without them
        # this is ('future', $id, $next) + 0x1000000, a four-element row
        # whose read starts at $next itself.
        @('future', $id, ($next + 0x1000000)),
        @('wrong_journal_id', ($id -bxor 1), $next)
    )
    $results = foreach ($c in $cursors) { , @($c + [UsnReadProbe]::Read($h, $c[1], $c[2])) }
} finally {
    $h.Dispose()
}

$names = 'journal_id', 'first_usn', 'next_usn', 'lowest_valid_usn', 'max_usn', 'maximum_size', 'allocation_delta'
$lines = @("# FSCTL_READ_USN_JOURNAL probe of $volume", "journal_capture_sha256`t$sha")
for ($k = 0; $k -lt 7; $k++) { $lines += "journal`t$($names[$k])`t$(Hex $q[$k])" }
$lines += '# probe name, journal id, start usn, ok, GetLastError, bytes, returned next usn'
foreach ($r in $results) {
    $lines += "probe`t$($r[0])`t$(Hex $r[1])`t$(Hex $r[2])`t$($r[3])`t$($r[4])`t$($r[5])`t$(Hex $r[6])"
}
[System.IO.File]::WriteAllLines($out, $lines, (New-Object System.Text.UTF8Encoding $false))
Get-Content -LiteralPath $out
$probedId = [BitConverter]::ToUInt64([BitConverter]::GetBytes([long]$id), 0)
if ($probedId -ne $capturedId) {
    throw ("journal on {0} is {1}, but {2} recorded 0x{3:x16}: not the captured journal" -f `
        $volume, (Hex $id), $journalCapture, $capturedId)
}
