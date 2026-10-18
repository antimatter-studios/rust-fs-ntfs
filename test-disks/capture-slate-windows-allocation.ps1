# Windows' allocator is the allocation oracle, independently of either NTFS
# reader. Dot-sourcing only defines the capture function. The caller owns
# the mount, and supplies the SHA-256 of the untouched raw image, not its
# mounted copy. The counts are the ones fsutil prints, read from the same
# Windows controls fsutil uses: FSCTL_GET_NTFS_VOLUME_DATA (fsutil fsinfo
# ntfsinfo) and FSCTL_GET_RETRIEVAL_POINTERS (fsutil file queryextents),
# so no locale-dependent text is parsed.
if (-not ('SlateWindowsAllocation' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class SlateWindowsAllocation {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle handle, uint code, byte[] input,
        int inputLength, byte[] output, int outputLength, out int returned, IntPtr overlapped);

    const uint GenericRead = 0x80000000, ReadAttributes = 0x80, ShareAll = 7, OpenExisting = 3;
    const uint BackupSemantics = 0x02000000;
    const uint GetNtfsVolumeData = 0x00090064, GetRetrievalPointers = 0x00090073;
    const int HandleEof = 38, MoreData = 234;

    static SafeFileHandle Open(string path, uint access) {
        SafeFileHandle handle = CreateFileW(path, access, ShareAll, IntPtr.Zero, OpenExisting,
            BackupSemantics, IntPtr.Zero);
        if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), "cannot open " + path);
        return handle;
    }

    // NTFS_VOLUME_DATA_BUFFER: bytes per cluster, total, free and reserved clusters.
    public static long[] Volume(char letter) {
        string device = @"\\.\" + letter + ":";
        using (SafeFileHandle handle = Open(device, GenericRead)) {
            byte[] output = new byte[512];
            int returned;
            if (!DeviceIoControl(handle, GetNtfsVolumeData, null, 0, output, output.Length, out returned, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "FSCTL_GET_NTFS_VOLUME_DATA on " + device);
            return new long[] { BitConverter.ToUInt32(output, 44), BitConverter.ToInt64(output, 16),
                BitConverter.ToInt64(output, 24), BitConverter.ToInt64(output, 32) };
        }
    }

    // RETRIEVAL_POINTERS_BUFFER runs as { vcn, clusters, lcn }; lcn -1 is a hole.
    public static List<long[]> Extents(string path) {
        List<long[]> extents = new List<long[]>();
        using (SafeFileHandle handle = Open(path, ReadAttributes)) {
            byte[] output = new byte[16 + 16 * 1024];
            long vcn = 0;
            while (true) {
                int returned;
                bool done = DeviceIoControl(handle, GetRetrievalPointers, BitConverter.GetBytes(vcn), 8,
                    output, output.Length, out returned, IntPtr.Zero);
                int error = done ? 0 : Marshal.GetLastWin32Error();
                // A resident or empty stream has no clusters to retrieve.
                if (error == HandleEof) break;
                if (!done && error != MoreData)
                    throw new Win32Exception(error, "FSCTL_GET_RETRIEVAL_POINTERS on " + path);
                int count = BitConverter.ToInt32(output, 0);
                long start = BitConverter.ToInt64(output, 8);
                for (int i = 0; i < count; i++) {
                    long next = BitConverter.ToInt64(output, 16 + 16 * i);
                    extents.Add(new long[] { start, next - start, BitConverter.ToInt64(output, 24 + 16 * i) });
                    start = next;
                }
                if (done) break;
                if (count == 0) throw new InvalidOperationException("no progress retrieving extents of " + path);
                vcn = start;
            }
        }
        return extents;
    }
}
'@
}

function Get-SlateWindowsAllocation {
    param(
        [Parameter(Mandatory=$true)] [string]$Root,
        [Parameter(Mandatory=$true)] [string]$Case,
        [Parameter(Mandatory=$true)] [string]$ImageSha256
    )
    if ($ImageSha256 -cnotmatch '^[0-9a-f]{64}$') { throw "$Case has an invalid image SHA-256" }
    # The volume counts belong to the whole volume, so the walk starts at it.
    if ($Root -cnotmatch '^[A-Za-z]:\\$') { throw "$Root is not a volume root such as 'E:\'" }
    $rootItem = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    $base = $rootItem.FullName.TrimEnd('\', '/')
    $counts = [SlateWindowsAllocation]::Volume($Root[0])
    # The same root names the read manifest excludes, for the same reason.
    $excluded = @('$AttrDef', '$BadClus', '$Bitmap', '$Boot', '$Extend', '$LogFile',
        '$MFT', '$MFTMirr', '$Secure', '$UpCase', '$Volume', '$Recycle.Bin', 'System Volume Information')
    $entries = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
    $pending = [Collections.Generic.Stack[string]]::new()
    $pending.Push($rootItem.FullName)
    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        try {
            $children = @(Get-ChildItem -LiteralPath $directory -Force -ErrorAction Stop)
        } catch { throw "Windows cannot enumerate ${directory}: $($_.Exception.Message)" }
        foreach ($item in $children) {
            $relative = '/' + $item.FullName.Substring($base.Length).TrimStart('\', '/').Replace('\', '/')
            if ($directory -eq $rootItem.FullName -and $excluded -contains $item.Name) { continue }
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Windows allocation manifest does not follow reparse point $relative"
            }
            $streams = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
            try {
                foreach ($stream in @(Get-Item -LiteralPath $item.FullName -Stream * -Force -ErrorAction Stop)) {
                    $name = if ($stream.Stream -ceq ':$DATA') { '' } else { $stream.Stream }
                    if ($item.PSIsContainer -and $name -ceq '') { continue }
                    $streamPath = if ($name -ceq '') { $item.FullName } else { "$($item.FullName):$name" }
                    $allocated = [long]0
                    $extents = @(foreach ($run in [SlateWindowsAllocation]::Extents($streamPath)) {
                        $lcn = if ($run[2] -lt 0) { $null } else { [long]$run[2] }
                        if ($null -ne $lcn) { $allocated += $run[1] }
                        [ordered]@{ vcn = [long]$run[0]; clusters = [long]$run[1]; lcn = $lcn }
                    })
                    $streams.Add($name, [ordered]@{ name = $name; allocated_clusters = $allocated; extents = $extents })
                }
                if (-not $item.PSIsContainer -and -not $streams.ContainsKey('')) {
                    throw 'Windows did not enumerate the unnamed data stream'
                }
            } catch { throw "Windows cannot read the allocation of ${relative}: $($_.Exception.Message)" }
            [string[]]$names = @($streams.Keys)
            [Array]::Sort($names, [StringComparer]::Ordinal)
            $entries.Add($relative, [ordered]@{
                path = $relative
                kind = if ($item.PSIsContainer) { 'directory' } else { 'file' }
                streams = @($names | ForEach-Object { $streams[$_] })
            })
            if ($item.PSIsContainer) { $pending.Push($item.FullName) }
        }
    }
    [string[]]$paths = @($entries.Keys)
    [Array]::Sort($paths, [StringComparer]::Ordinal)
    [ordered]@{
        schema = 1
        case = $Case
        image_sha256 = $ImageSha256
        oracle = 'Windows FSCTL_GET_NTFS_VOLUME_DATA / FSCTL_GET_RETRIEVAL_POINTERS (fsutil fsinfo ntfsinfo, fsutil file queryextents)'
        volume = [ordered]@{
            bytes_per_cluster = $counts[0]
            total_clusters = $counts[1]
            free_clusters = $counts[2]
            reserved_clusters = $counts[3]
        }
        capabilities = [ordered]@{
            excluded_root_names = $excluded
            exclusion_reason = 'NTFS metadata and Windows mount lifecycle entries'
            volume_counts = 'after Windows mounted the read wrapper; clusters held by excluded mount-created names are in them'
            holes = 'an extent with a null lcn is a sparse or compressed hole and is not allocated'
            reparse_points = 'fail by path; never followed or silently omitted'
            root_entry = 'implicit directory; root streams are outside this manifest'
        }
        entries = @($paths | ForEach-Object { $entries[$_] })
    }
}
