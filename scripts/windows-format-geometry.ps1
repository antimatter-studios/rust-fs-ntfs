# Clean-room observations of Microsoft's formatter; no driver parser is used.
# Dot-sourcing only defines functions. The caller owns the capture lifecycle.
function Get-FormatGeometryRequests {
    foreach ($large in @($false, $true)) {
        for ($size = 512; $size -le 2MB; $size *= 2) {
            [pscustomobject]@{ allocation_unit_bytes = $size; large_frs = $large }
        }
    }
}
function Get-FormatEncodedSize([int]$Byte, [long]$Unit, [switch]$SectorByte) {
    if ($Byte -eq 0) { throw 'zero size encoding' }
    if ($Byte -lt 128 -or ($SectorByte -and $Byte -eq 128)) { return $Byte * $Unit }
    $exponent = 256 - $Byte
    if ($exponent -gt 31) { throw 'size exponent exceeds capture bounds' }
    $size = [long][Math]::Pow(2, $exponent)
    if ($SectorByte) { return $size * $Unit }
    return $size
}
function ConvertFrom-FormatBoot([byte[]]$Boot) {
    if ($Boot.Length -ne 512 -or [Text.Encoding]::ASCII.GetString($Boot, 3, 8) -cne 'NTFS    ' -or
        $Boot[510] -ne 85 -or $Boot[511] -ne 170) { throw 'not a complete NTFS boot sector' }
    $bps = [BitConverter]::ToUInt16($Boot, 11)
    if ($bps -ne 512) { throw "expected 512-byte logical sectors, found $bps" }
    $cluster = Get-FormatEncodedSize $Boot[13] $bps -SectorByte
    [pscustomobject][ordered]@{
        bytes_per_sector = $bps
        sectors_per_cluster_byte = [int]$Boot[13]
        clusters_per_record_byte = [int]$Boot[64]
        clusters_per_index_block_byte = [int]$Boot[68]
        cluster_bytes = $cluster
        record_bytes = Get-FormatEncodedSize $Boot[64] $cluster
        index_block_bytes = Get-FormatEncodedSize $Boot[68] $cluster
        mft_lcn = [BitConverter]::ToUInt64($Boot, 48)
        mftmirr_lcn = [BitConverter]::ToUInt64($Boot, 56)
    }
}
function ConvertFrom-GeometryNtfsInfo([string[]]$Lines) {
    $fields = [ordered]@{ bytes_per_sector = 'Bytes Per Sector'; cluster_bytes = 'Bytes Per Cluster';
        record_bytes = 'Bytes Per FileRecord Segment'; mft_lcn = 'Mft Start Lcn'; mftmirr_lcn = 'Mft2 Start Lcn' }
    $result = [ordered]@{}
    foreach ($key in $fields.Keys) {
        # fsutil renders some byte counts with a parenthesized unit, e.g.
        # `Bytes Per Cluster : 4096 (4 KB)`. Parse the numeric value while
        # allowing that display-only annotation.
        $pattern = '^\s*' + [regex]::Escape($fields[$key]) + '\s*:\s*(0x[0-9a-fA-F]+|[0-9]+)(?:\s+\([0-9]+\s*[A-Za-z]+\))?\s*$'
        $values = @($Lines | ForEach-Object { if ($_ -match $pattern) { $Matches[1] } })
        if ($values.Count -ne 1) { throw "fsutil must report exactly one $($fields[$key]) field" }
        $result[$key] = if ($values[0].StartsWith('0x')) {
            [Convert]::ToUInt64($values[0].Substring(2), 16)
        } else { [Convert]::ToUInt64($values[0], 10) }
    }
    [pscustomobject]$result
}
function Assert-FormatGeometryMatch($Boot, $Native, [int]$Requested) {
    foreach ($key in 'bytes_per_sector', 'cluster_bytes', 'record_bytes', 'mft_lcn', 'mftmirr_lcn') {
        if ($Boot.$key -ne $Native.$key) { throw "BPB and fsutil disagree on $key" }
    }
    if ($Boot.cluster_bytes -ne $Requested) { throw 'Windows did not use the requested allocation unit' }
}
function Assert-FormatGeometryRows([object[]]$Rows) {
    $expected = @(Get-FormatGeometryRequests)
    if ($Rows.Count -ne 26) { throw "expected 26 requests, found $($Rows.Count)" }
    foreach ($request in $expected) {
        $row = @($Rows | Where-Object { $_.allocation_unit_bytes -eq $request.allocation_unit_bytes -and
            $_.large_frs -eq $request.large_frs })
        if ($row.Count -ne 1) { throw 'missing or duplicate geometry request' }
        if (-not $row[0].accepted -and -not $row[0].error_id) { throw 'refusal lacks a native diagnostic' }
    }
    foreach ($large in @($false, $true)) {
        if (-not @($Rows | Where-Object { $_.allocation_unit_bytes -eq 4096 -and
            $_.large_frs -eq $large -and $_.accepted }).Count) {
            throw '4 KiB control format failed; capture infrastructure is not validated'
        }
    }
}
function Get-LargeClusterLayoutRequests { 65536, 131072, 262144, 524288, 1048576, 2097152 }
function ConvertFrom-RetrievalExtents([object[]]$Extents) {
    if (-not $Extents -or $Extents.Count -eq 0) { throw 'Windows reported no extents' }
    $next = 0L
    foreach ($extent in $Extents) {
        if ($extent.vcn -ne $next) { throw "extent at VCN $($extent.vcn) does not follow VCN $next" }
        if ($extent.next_vcn -le $extent.vcn) { throw "empty extent at VCN $($extent.vcn)" }
        if ($extent.lcn -lt 0) { throw "extent at VCN $($extent.vcn) is not allocated" }
        $next = $extent.next_vcn
    }
    [pscustomobject][ordered]@{ lcn = [long]$Extents[0].lcn; clusters = [long]$next; extents = @($Extents) }
}
function Assert-LargeClusterLayoutRows([object[]]$Rows) {
    $expected = @(Get-LargeClusterLayoutRequests)
    if ($Rows.Count -ne $expected.Count) { throw "expected $($expected.Count) layout rows, found $($Rows.Count)" }
    foreach ($size in $expected) {
        $row = @($Rows | Where-Object { $_.allocation_unit_bytes -eq $size })
        if ($row.Count -ne 1) { throw "missing or duplicate layout row for $size-byte clusters" }
        if (-not $row[0].accepted) { throw "Windows did not format the $size-byte layout request" }
        foreach ($file in 'mftmirr', 'sds', 'root_index') {
            $value = $row[0].$file
            if ($null -eq $value -or $null -eq $value.lcn -or -not $value.clusters) {
                throw "layout row for $size-byte clusters lacks $file"
            }
        }
    }
}
# FSCTL_GET_RETRIEVAL_POINTERS is Windows' own allocation answer: the
# filesystem driver reads the attribute's runs, not this crate's parser.
function Get-NtfsStreamExtents([string]$Target) {
    if (-not ('GeometryRetrievalPointers' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class GeometryRetrievalPointers {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle device, uint code, ref long input, int inputSize,
        byte[] output, int outputSize, out int returned, IntPtr overlapped);
    const uint FsctlGetRetrievalPointers = 0x00090073;
    const int ErrorMoreData = 234;
    // FILE_READ_ATTRIBUTES, all share modes, OPEN_EXISTING, and
    // FILE_FLAG_BACKUP_SEMANTICS so a directory's index stream opens.
    public static long[][] Query(string target) {
        using (SafeFileHandle handle = CreateFileW(target, 0x80, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero)) {
            if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), "cannot open " + target);
            List<long[]> extents = new List<long[]>();
            long start = 0;
            byte[] buffer = new byte[64 * 1024];
            while (true) {
                int returned;
                bool ok = DeviceIoControl(handle, FsctlGetRetrievalPointers, ref start, 8, buffer, buffer.Length,
                    out returned, IntPtr.Zero);
                int error = ok ? 0 : Marshal.GetLastWin32Error();
                if (!ok && error != ErrorMoreData) throw new Win32Exception(error, "no retrieval pointers for " + target);
                // RETRIEVAL_POINTERS_BUFFER: count, StartingVcn at 8, then {NextVcn, Lcn} pairs.
                int count = BitConverter.ToInt32(buffer, 0);
                long vcn = BitConverter.ToInt64(buffer, 8);
                for (int i = 0; i < count; i++) {
                    long next = BitConverter.ToInt64(buffer, 16 + i * 16);
                    extents.Add(new long[] { vcn, next, BitConverter.ToInt64(buffer, 24 + i * 16) });
                    vcn = next;
                }
                if (ok) return extents.ToArray();
                if (count == 0) throw new InvalidOperationException("retrieval pointers made no progress for " + target);
                start = vcn;
            }
        }
    }
}
'@
    }
    @([GeometryRetrievalPointers]::Query($Target) | ForEach-Object {
        [pscustomobject][ordered]@{ vcn = $_[0]; next_vcn = $_[1]; lcn = $_[2] } })
}
function Get-LargeClusterLayout([string]$Letter, [int]$Size) {
    $targets = [ordered]@{ mftmirr = $Letter + ':\$MFTMirr'; sds = $Letter + ':\$Secure:$SDS';
        root_index = $Letter + ':\:$I30:$INDEX_ALLOCATION' }
    $layout = [ordered]@{}
    foreach ($file in $targets.Keys) {
        try {
            $layout[$file] = ConvertFrom-RetrievalExtents (Get-NtfsStreamExtents $targets[$file])
        } catch { throw "layout of $file at $Size-byte clusters is unreadable: $($_.Exception.Message)" }
    }
    $layout
}
function Invoke-WindowsFormatGeometryCapture([string]$OutDir) {
    $ErrorActionPreference = 'Stop'
    if (-not $IsWindows) { throw 'the capture requires an elevated hosted Windows runner' }
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'the capture requires Administrator'
    }
    $out = [IO.Path]::GetFullPath($OutDir)
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    $vhd = Join-Path $out ("geometry-" + [guid]::NewGuid() + '.vhd')
    $diskpart = Join-Path $out 'create-disk.txt'
    # Dynamic VHD (not VHDX) has 512-byte logical sectors. Get-Disk verifies
    # that property before any format request; this file belongs to this run.
    Set-Content -LiteralPath $diskpart -Encoding ascii -Value @(
        "create vdisk file=`"$vhd`" maximum=1024 type=expandable", 'exit')
    $rows = [Collections.Generic.List[object]]::new()
    $layoutRows = [Collections.Generic.List[object]]::new()
    $layoutSizes = @(Get-LargeClusterLayoutRequests)
    $attached = $false
    try {
        $process = Start-Process diskpart.exe -ArgumentList '/s', "`"$diskpart`"" -PassThru -NoNewWindow `
            -RedirectStandardOutput (Join-Path $out 'create-disk.stdout.txt') `
            -RedirectStandardError (Join-Path $out 'create-disk.stderr.txt')
        if (-not $process.WaitForExit(60000)) { $process.Kill(); $process.WaitForExit(); throw 'diskpart creation timed out' }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $vhd)) { throw 'diskpart failed to create the scratch VHD' }
        # Set ownership before mount: cleanup also handles a partially successful mount.
        $attached = $true
        $disk = Mount-DiskImage -ImagePath $vhd -PassThru | Get-Disk
        if ($disk.LogicalSectorSize -ne 512) { throw 'scratch disk does not expose 512-byte sectors' }
        Initialize-Disk -Number $disk.Number -PartitionStyle GPT | Out-Null
        $partition = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
        if (-not $partition.DriveLetter) { throw 'scratch partition has no drive letter' }
        $letter = $partition.DriveLetter
        $head = (& git rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or $head -notmatch '^[0-9a-f]{40}$') { throw 'cannot record source revision' }
        $os = Get-CimInstance Win32_OperatingSystem
        $provenance = [ordered]@{ schema = 1; source_revision = $head; runner_image = $env:ImageOS;
            runner_image_version = $env:ImageVersion; windows_caption = $os.Caption;
            windows_version = $os.Version; windows_build = $os.BuildNumber;
            windows_update_build_revision = Get-ItemPropertyValue 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' UBR;
            captured_utc = [DateTime]::UtcNow.ToString('o'); disk_bytes = $disk.Size;
            partition_bytes = $partition.Size; partition_offset = $partition.Offset;
            logical_sector_bytes = $disk.LogicalSectorSize;
            formatter = 'Format-Volume -DriveLetter <scratch-drive> -FileSystem NTFS -AllocationUnitSize <bytes> -UseLargeFRS:<bool> -NewFileSystemLabel GEOMETRY -Force -Confirm:$false';
            native_info = 'fsutil fsinfo ntfsinfo <scratch-drive>:';
            boot_read = '512 bytes at the scratch partition offset through PhysicalDrive';
            layout_source = 'FSCTL_GET_RETRIEVAL_POINTERS on <scratch-drive>:\$MFTMirr, <scratch-drive>:\$Secure:$SDS and <scratch-drive>:\:$I30:$INDEX_ALLOCATION, opened with FILE_READ_ATTRIBUTES and FILE_FLAG_BACKUP_SEMANTICS, for 64 KiB to 2 MiB clusters without large records' }
        $provenance | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $out 'provenance.json') -Encoding utf8
        foreach ($request in Get-FormatGeometryRequests) {
            $stem = "$($request.allocation_unit_bytes)-$($request.large_frs)"
            Write-Host "format-geometry: $stem"
            $row = [ordered]@{ allocation_unit_bytes = $request.allocation_unit_bytes;
                large_frs = $request.large_frs; accepted = $false; error_id = $null;
                diagnostic = $null; boot = $null; fsutil = $null;
                command = "Format-Volume -DriveLetter $letter -FileSystem NTFS -AllocationUnitSize $($request.allocation_unit_bytes) -UseLargeFRS:`$$($request.large_frs) -NewFileSystemLabel GEOMETRY -Force -Confirm:`$false" }
            try {
                Format-Volume -DriveLetter $letter -FileSystem NTFS -AllocationUnitSize $request.allocation_unit_bytes `
                    -UseLargeFRS:$request.large_frs -NewFileSystemLabel GEOMETRY -Force -Confirm:$false | Out-Null
                $row.accepted = $true
            } catch [Microsoft.Management.Infrastructure.CimException] {
                # Only formatter errors become measured unsuccessful requests.
                # Disk setup, raw reads, fsutil and decoding failures escape.
                $row.error_id = $_.FullyQualifiedErrorId
                $row.diagnostic = $_.Exception.Message
                $_ | Format-List * -Force | Out-String | Set-Content (Join-Path $out "$stem-format-error.txt")
            }
            if ($row.accepted) {
                $info = @(& fsutil fsinfo ntfsinfo "${letter}:")
                if ($LASTEXITCODE -ne 0) { throw "fsutil failed for $stem" }
                $info | Set-Content (Join-Path $out "$stem-ntfsinfo.txt") -Encoding utf8
                $row.fsutil = ConvertFrom-GeometryNtfsInfo $info
                $stream = [IO.File]::Open("\\.\PhysicalDrive$($disk.Number)", 'Open', 'Read', 'ReadWrite')
                try {
                    $null = $stream.Seek($partition.Offset, 'Begin')
                    $boot = [byte[]]::new(512)
                    $stream.ReadExactly($boot, 0, 512)
                } finally { $stream.Dispose() }
                $row.boot = ConvertFrom-FormatBoot $boot
                Assert-FormatGeometryMatch $row.boot $row.fsutil $request.allocation_unit_bytes
            }
            if (-not $request.large_frs -and $layoutSizes -contains $request.allocation_unit_bytes) {
                $layout = [ordered]@{ allocation_unit_bytes = $request.allocation_unit_bytes;
                    accepted = $row.accepted; mftmirr = $null; sds = $null; root_index = $null }
                if ($row.accepted) {
                    $measured = Get-LargeClusterLayout $letter $request.allocation_unit_bytes
                    foreach ($file in $measured.Keys) { $layout[$file] = $measured[$file] }
                }
                $layoutRows.Add([pscustomobject]$layout)
                ConvertTo-Json -InputObject @($layoutRows.ToArray()) -Depth 8 |
                    Set-Content (Join-Path $out 'large-cluster-layout.json') -Encoding utf8
            }
            $rows.Add([pscustomobject]$row)
            # Keep partial evidence if a later request fails, never a partial success.
            ConvertTo-Json -InputObject @($rows.ToArray()) -Depth 8 |
                Set-Content (Join-Path $out 'measurements.json') -Encoding utf8
        }
        Assert-FormatGeometryRows @($rows.ToArray())
        Assert-LargeClusterLayoutRows @($layoutRows.ToArray())
        Write-Host 'format-geometry: 26 requests captured; both 4 KiB controls passed; 6 large-cluster layouts measured'
    } finally {
        if ($attached) {
            $image = Get-DiskImage -ImagePath $vhd -ErrorAction Stop
            if ($image.Attached) { Dismount-DiskImage -ImagePath $vhd -ErrorAction Stop }
            if ((Get-DiskImage -ImagePath $vhd).Attached) { throw 'scratch VHD remains attached' }
        }
        if (Test-Path -LiteralPath $vhd) { Remove-Item -LiteralPath $vhd -Force -ErrorAction Stop }
    }
}
