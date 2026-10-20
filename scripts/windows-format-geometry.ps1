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
function ConvertFrom-FormatBoot([byte[]]$Boot, [int]$SectorBytes = 512) {
    if ($Boot.Length -ne $SectorBytes -or [Text.Encoding]::ASCII.GetString($Boot, 3, 8) -cne 'NTFS    ' -or
        $Boot[510] -ne 85 -or $Boot[511] -ne 170) { throw 'not a complete NTFS boot sector' }
    $bps = [BitConverter]::ToUInt16($Boot, 11)
    if ($bps -ne $SectorBytes) { throw "expected $SectorBytes-byte logical sectors, found $bps" }
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
        number_sectors = [BitConverter]::ToUInt64($Boot, 40)
    }
}
function ConvertFrom-GeometryNtfsInfo([string[]]$Lines, [switch]$Physical) {
    $fields = [ordered]@{ bytes_per_sector = 'Bytes Per Sector'; cluster_bytes = 'Bytes Per Cluster';
        record_bytes = 'Bytes Per FileRecord Segment'; mft_lcn = 'Mft Start Lcn'; mftmirr_lcn = 'Mft2 Start Lcn' }
    if ($Physical) { $fields['bytes_per_physical_sector'] = 'Bytes Per Physical Sector' }
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
# Only the fixed header of a multi-sector record: magic, USA offset and count.
# The stride is the record length over the count less one, as Windows wrote it.
function ConvertFrom-UpdateSequenceHeader([byte[]]$Record, [string]$Magic) {
    if ($Record.Length -lt 8 -or [Text.Encoding]::ASCII.GetString($Record, 0, 4) -cne $Magic) { throw "not a $Magic record" }
    $offset = [int][BitConverter]::ToUInt16($Record, 4)
    $count = [int][BitConverter]::ToUInt16($Record, 6)
    if ($count -lt 2 -or $offset + 2 * $count -gt $Record.Length) { throw "$Magic update sequence array does not fit the record" }
    if ($Record.Length % ($count - 1) -ne 0) { throw "$Magic update sequence count $count does not divide $($Record.Length) bytes" }
    $stride = [int]($Record.Length / ($count - 1))
    if (($stride -band ($stride - 1)) -ne 0) { throw "$Magic update sequence stride $stride is not a power of two" }
    [pscustomobject][ordered]@{ record_bytes = $Record.Length; usa_offset = $offset; usa_count = $count; stride_bytes = $stride }
}
function Assert-SectorGeometryRow($Row, [int]$SectorBytes = 4096) {
    foreach ($field in 'disk_tool', 'logical_sector_bytes', 'physical_sector_bytes', 'fsutil', 'boot',
        'backup_boot_offsets', 'mft_usa', 'root_index_usa') {
        if ($null -eq $Row.$field) { throw "the $SectorBytes-byte sector row lacks $field" }
    }
    if ($Row.logical_sector_bytes -ne $SectorBytes) {
        throw "the scratch disk reports $($Row.logical_sector_bytes)-byte logical sectors, not $SectorBytes"
    }
    if (@($Row.backup_boot_offsets).Count -eq 0) { throw 'no copy of the boot sector was found where a backup lives' }
    if ($null -eq $Row.fsutil.bytes_per_physical_sector) { throw 'the sector row lacks fsutil bytes_per_physical_sector' }
    if ($Row.boot.bytes_per_sector -ne $SectorBytes) { throw "the boot sector declares $($Row.boot.bytes_per_sector)-byte sectors" }
    Assert-FormatGeometryMatch $Row.boot $Row.fsutil $Row.boot.cluster_bytes
}
# 512e beside 512n: the row comes from a disk reporting 512-byte logical
# sectors and the physical size asked for. Windows keeps the live $LogFile
# metadata file locked against extent queries, so the comparison uses the
# partition and other filesystem geometry that Windows exposes.
function Assert-PhysicalSectorGeometryRow($Row, [int]$PhysicalBytes) {
    foreach ($field in 'partition_offset') {
        if ($null -eq $Row.$field) { throw "the $PhysicalBytes-byte physical sector row lacks $field" }
    }
    Assert-SectorGeometryRow $Row 512
    if ($Row.physical_sector_bytes -ne $PhysicalBytes) {
        throw "the scratch disk reports $($Row.physical_sector_bytes)-byte physical sectors, not $PhysicalBytes"
    }
    if ($Row.fsutil.bytes_per_physical_sector -ne $PhysicalBytes) {
        throw "fsutil reports $($Row.fsutil.bytes_per_physical_sector)-byte physical sectors, not $PhysicalBytes"
    }
}
# Every field of a 512-byte logical sector row except the physical sector size
# itself, which is the variable under test and differs by construction.
function Get-PhysicalSectorComparedFields {
    'partition_offset', 'backup_boot_offsets',
    'boot.bytes_per_sector', 'boot.sectors_per_cluster_byte', 'boot.cluster_bytes', 'boot.clusters_per_record_byte',
    'boot.record_bytes', 'boot.clusters_per_index_block_byte', 'boot.index_block_bytes', 'boot.mft_lcn',
    'boot.mftmirr_lcn', 'boot.number_sectors',
    'fsutil.bytes_per_sector', 'fsutil.cluster_bytes', 'fsutil.record_bytes', 'fsutil.mft_lcn', 'fsutil.mftmirr_lcn',
    'mft_usa.usa_offset', 'mft_usa.usa_count', 'mft_usa.stride_bytes',
    'root_index_usa.usa_offset', 'root_index_usa.usa_count', 'root_index_usa.stride_bytes'
}
function Get-GeometryField($Row, [string]$Field) {
    $value = $Row
    foreach ($part in $Field.Split('.')) { if ($null -ne $value) { $value = $value.$part } }
    if ($value -is [array]) { return (@($value) -join ',') }
    "$value"
}
# The names of the fields where Windows' 512e format differs from its 512n one.
function Compare-PhysicalSectorGeometryRows($Native, $Emulated) {
    foreach ($field in Get-PhysicalSectorComparedFields) {
        if ((Get-GeometryField $Native $field) -cne (Get-GeometryField $Emulated $field)) { $field }
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
# Raw reads through the disk device: Windows' bytes, aligned to its sectors.
function Read-GeometryDisk([int]$DiskNumber, [long]$Offset, [int]$Length) {
    $stream = [IO.File]::Open("\\.\PhysicalDrive$DiskNumber", 'Open', 'Read', 'ReadWrite')
    try {
        $null = $stream.Seek($Offset, 'Begin')
        $bytes = [byte[]]::new($Length)
        $stream.ReadExactly($bytes, 0, $Length)
    } finally { $stream.Dispose() }
    , $bytes
}
# Fallback disk maker: a diskpart VHDX with its sector size metadata items set.
# The metadata region carries no checksum (only the headers and region table
# do), and Get-Disk on the attached image is what proves the result.
function Set-VhdxSectorSize([string]$Path, [int]$Bytes, [int]$PhysicalBytes = $Bytes) {
    $items = @{ ([guid]'8141BF1D-A96F-4709-BA47-F233A8FAAB5F') = $Bytes; ([guid]'CDA348C7-445D-4471-9CC9-E9885251C556') = $PhysicalBytes }
    $metadataRegion = [guid]'8B7CA206-4790-4B9A-B8FE-575F050F886E'
    $stream = [IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
    try {
        $table = [byte[]]::new(65536)
        $null = $stream.Seek(196608, 'Begin'); $stream.ReadExactly($table, 0, 65536)
        if ([Text.Encoding]::ASCII.GetString($table, 0, 4) -cne 'regi') { throw 'VHDX region table signature missing' }
        $metadataOffset = $null
        for ($i = 0; $i -lt [BitConverter]::ToUInt32($table, 8); $i++) {
            $at = 16 + 32 * $i
            if ([guid]::new([byte[]]$table[$at..($at + 15)]) -eq $metadataRegion) {
                $metadataOffset = [BitConverter]::ToInt64($table, $at + 16)
            }
        }
        if ($null -eq $metadataOffset) { throw 'VHDX metadata region missing' }
        $meta = [byte[]]::new(65536)
        $null = $stream.Seek($metadataOffset, 'Begin'); $stream.ReadExactly($meta, 0, 65536)
        if ([Text.Encoding]::ASCII.GetString($meta, 0, 8) -cne 'metadata') { throw 'VHDX metadata table signature missing' }
        $patched = 0
        for ($i = 0; $i -lt [BitConverter]::ToUInt16($meta, 10); $i++) {
            $at = 32 + 32 * $i
            $item = [guid]::new([byte[]]$meta[$at..($at + 15)])
            if ($items.ContainsKey($item)) {
                $null = $stream.Seek($metadataOffset + [BitConverter]::ToUInt32($meta, $at + 16), 'Begin')
                $stream.Write([BitConverter]::GetBytes([uint32]$items[$item]), 0, 4)
                $patched++
            }
        }
        if ($patched -ne 2) { throw "found $patched of the 2 VHDX sector size items" }
    } finally { $stream.Dispose() }
}
# Tries each way of making a disk with the requested logical sector size and
# records every attempt; the last attempt is the tool that made the disk.
function New-SectorGeometryDisk([string]$Out, [int]$SectorBytes, [int]$PhysicalBytes = $SectorBytes) {
    $attempts = [Collections.Generic.List[object]]::new()
    $vhdx = Join-Path $Out ("sector-geometry-" + [guid]::NewGuid() + '.vhdx')
    $tool = "New-VHD -SizeBytes 1GB -Dynamic -LogicalSectorSizeBytes $SectorBytes -PhysicalSectorSizeBytes $PhysicalBytes"
    if (Get-Command New-VHD -ErrorAction SilentlyContinue) {
        try {
            New-VHD -Path $vhdx -SizeBytes 1GB -Dynamic -LogicalSectorSizeBytes $SectorBytes -PhysicalSectorSizeBytes $PhysicalBytes | Out-Null
            $attempts.Add([pscustomobject]@{ tool = $tool; diagnostic = $null })
            return [pscustomobject]@{ path = $vhdx; attempts = $attempts.ToArray() }
        } catch {
            $attempts.Add([pscustomobject]@{ tool = $tool; diagnostic = $_.Exception.Message })
            if (Test-Path -LiteralPath $vhdx) { Remove-Item -LiteralPath $vhdx -Force }
        }
    } else { $attempts.Add([pscustomobject]@{ tool = $tool; diagnostic = 'New-VHD is not available on this runner' }) }
    $tool = "diskpart create vdisk maximum=1024 type=expandable (VHDX), then its logical sector size metadata set to $SectorBytes and its physical sector size to $PhysicalBytes"
    $script = Join-Path $Out 'sector-create-disk.txt'
    Set-Content -LiteralPath $script -Encoding ascii -Value @("create vdisk file=`"$vhdx`" maximum=1024 type=expandable", 'exit')
    try {
        $process = Start-Process diskpart.exe -ArgumentList '/s', "`"$script`"" -PassThru -NoNewWindow `
            -RedirectStandardOutput (Join-Path $Out 'sector-create-disk.stdout.txt') `
            -RedirectStandardError (Join-Path $Out 'sector-create-disk.stderr.txt')
        if (-not $process.WaitForExit(60000)) { $process.Kill(); $process.WaitForExit(); throw 'diskpart creation timed out' }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $vhdx)) { throw 'diskpart failed to create the scratch VHDX' }
        Set-VhdxSectorSize $vhdx $SectorBytes $PhysicalBytes
        $attempts.Add([pscustomobject]@{ tool = $tool; diagnostic = $null })
    } catch {
        $attempts.Add([pscustomobject]@{ tool = $tool; diagnostic = $_.Exception.Message })
        if (Test-Path -LiteralPath $vhdx) { Remove-Item -LiteralPath $vhdx -Force }
        ConvertTo-Json -InputObject @($attempts.ToArray()) -Depth 4 |
            Set-Content (Join-Path $Out "sector-$SectorBytes-$PhysicalBytes-attempts.json") -Encoding utf8
        throw "no tool on this runner made a disk with $SectorBytes-byte logical and $PhysicalBytes-byte physical sectors"
    }
    [pscustomobject]@{ path = $vhdx; attempts = $attempts.ToArray() }
}
function Invoke-WindowsSectorGeometryCapture([string]$OutDir, [int]$SectorBytes = 4096, [int]$PhysicalBytes = $SectorBytes) {
    $ErrorActionPreference = 'Stop'
    if (-not $IsWindows) { throw 'the capture requires an elevated hosted Windows runner' }
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'the capture requires Administrator'
    }
    $out = [IO.Path]::GetFullPath($OutDir)
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    $made = New-SectorGeometryDisk $out $SectorBytes $PhysicalBytes
    $vhdx = $made.path
    $name = if ($PhysicalBytes -eq $SectorBytes) { "sector-$SectorBytes" } else { "sector-$SectorBytes-$PhysicalBytes" }
    $rowPath = Join-Path $out "$name.json"
    $row = [ordered]@{ disk_tool = $made.attempts[-1].tool; disk_attempts = $made.attempts;
        logical_sector_bytes = $null; physical_sector_bytes = $null; partition_offset = $null; fsutil = $null; boot = $null;
        backup_boot_offsets = $null; mft_usa = $null; root_index_usa = $null; logfile = $null;
        formatter = 'Format-Volume -DriveLetter <scratch-drive> -FileSystem NTFS -NewFileSystemLabel SECTORS -Force -Confirm:$false' }
    $attached = $false
    try {
        $attached = $true
        $disk = Mount-DiskImage -ImagePath $vhdx -PassThru | Get-Disk
        $row.logical_sector_bytes = $disk.LogicalSectorSize
        $row.physical_sector_bytes = $disk.PhysicalSectorSize
        ConvertTo-Json -InputObject ([pscustomobject]$row) -Depth 8 | Set-Content $rowPath -Encoding utf8
        if ($disk.LogicalSectorSize -ne $SectorBytes) {
            throw "the scratch disk exposes $($disk.LogicalSectorSize)-byte logical sectors, not $SectorBytes"
        }
        if ($SectorBytes -eq 512 -and $disk.PhysicalSectorSize -ne $PhysicalBytes) {
            throw "the scratch disk exposes $($disk.PhysicalSectorSize)-byte physical sectors, not $PhysicalBytes"
        }
        Initialize-Disk -Number $disk.Number -PartitionStyle GPT | Out-Null
        $partition = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
        if (-not $partition.DriveLetter) { throw 'scratch partition has no drive letter' }
        $letter = $partition.DriveLetter
        $row.partition_offset = $partition.Offset
        $head = (& git rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or $head -notmatch '^[0-9a-f]{40}$') { throw 'cannot record source revision' }
        $os = Get-CimInstance Win32_OperatingSystem
        [ordered]@{ schema = 1; source_revision = $head; runner_image = $env:ImageOS;
            runner_image_version = $env:ImageVersion; windows_caption = $os.Caption;
            windows_version = $os.Version; windows_build = $os.BuildNumber;
            windows_update_build_revision = Get-ItemPropertyValue 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' UBR;
            captured_utc = [DateTime]::UtcNow.ToString('o'); disk_tool = $row.disk_tool; disk_bytes = $disk.Size;
            partition_bytes = $partition.Size; partition_offset = $partition.Offset;
            formatter = $row.formatter; native_info = 'fsutil fsinfo ntfsinfo <scratch-drive>:';
            disk_info = 'Get-Disk LogicalSectorSize and PhysicalSectorSize of the attached image';
            boot_read = "$SectorBytes bytes at the scratch partition offset through PhysicalDrive";
            backup_boot_read = "$SectorBytes bytes at NumberSectors x $SectorBytes and at the partition's last sector, kept when the first 512 bytes equal the boot sector's";
            logfile_read = 'not captured: Windows denies opening the live NTFS $LogFile metadata file for extent queries';
            usa_read = "the boot sector's record size at Mft Start Lcn, and its index block size at the first LCN FSCTL_GET_RETRIEVAL_POINTERS reports for <scratch-drive>:\:`$I30:`$INDEX_ALLOCATION, both through PhysicalDrive" } |
            ConvertTo-Json -Depth 5 | Set-Content (Join-Path $out "$name-provenance.json") -Encoding utf8
        Format-Volume -DriveLetter $letter -FileSystem NTFS -NewFileSystemLabel SECTORS -Force -Confirm:$false | Out-Null
        $info = @(& fsutil fsinfo ntfsinfo "${letter}:")
        if ($LASTEXITCODE -ne 0) { throw "fsutil failed for the $SectorBytes-byte sector volume" }
        $info | Set-Content (Join-Path $out "$name-ntfsinfo.txt") -Encoding utf8
        $row.fsutil = ConvertFrom-GeometryNtfsInfo $info -Physical
        $base = [long]$partition.Offset
        $span = [long]$partition.Size
        $boot = Read-GeometryDisk $disk.Number $base $SectorBytes
        $row.boot = ConvertFrom-FormatBoot $boot -SectorBytes $SectorBytes
        $primary = [Convert]::ToBase64String($boot, 0, 512)
        $backupFromBoot = [long]$row.boot.number_sectors * [long]$SectorBytes
        $backupFromEnd = $span - [long]$SectorBytes
        $backupCandidates = @($backupFromBoot, $backupFromEnd)
        $row.backup_boot_offsets = @($backupCandidates |
            Sort-Object -Unique | Where-Object { $_ -gt 0 -and $_ + $SectorBytes -le $span } | Where-Object {
                [Convert]::ToBase64String((Read-GeometryDisk $disk.Number ($base + $_) $SectorBytes), 0, 512) -eq $primary })
        $mft = $base + [long]$row.boot.mft_lcn * $row.boot.cluster_bytes
        $row.mft_usa = ConvertFrom-UpdateSequenceHeader (Read-GeometryDisk $disk.Number $mft $row.boot.record_bytes) 'FILE'
        $root = ConvertFrom-RetrievalExtents (Get-NtfsStreamExtents ($letter + ':\:$I30:$INDEX_ALLOCATION'))
        $index = $base + $root.lcn * $row.boot.cluster_bytes
        $row.root_index_usa = ConvertFrom-UpdateSequenceHeader (Read-GeometryDisk $disk.Number $index $row.boot.index_block_bytes) 'INDX'
        ConvertTo-Json -InputObject ([pscustomobject]$row) -Depth 8 | Set-Content $rowPath -Encoding utf8
        if ($SectorBytes -eq 512) { Assert-PhysicalSectorGeometryRow ([pscustomobject]$row) $PhysicalBytes }
        else { Assert-SectorGeometryRow ([pscustomobject]$row) $SectorBytes }
        Write-Host "sector-geometry: Windows' own NTFS format of a $SectorBytes-byte logical, $PhysicalBytes-byte physical sector disk captured with $($row.disk_tool)"
    } finally {
        if ($attached) {
            $image = Get-DiskImage -ImagePath $vhdx -ErrorAction Stop
            if ($image.Attached) { Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop }
            if ((Get-DiskImage -ImagePath $vhdx).Attached) { throw 'scratch VHDX remains attached' }
        }
        if (Test-Path -LiteralPath $vhdx) { Remove-Item -LiteralPath $vhdx -Force -ErrorAction Stop }
    }
}
# Windows formats a 512n and a 512e disk of the same size and the capture keeps
# the names of the fields that differ; an empty list is a measured result.
function Invoke-WindowsPhysicalSectorGeometryCapture([string]$OutDir) {
    $ErrorActionPreference = 'Stop'
    Invoke-WindowsSectorGeometryCapture -OutDir $OutDir -SectorBytes 512 -PhysicalBytes 512
    Invoke-WindowsSectorGeometryCapture -OutDir $OutDir -SectorBytes 512 -PhysicalBytes 4096
    $out = [IO.Path]::GetFullPath($OutDir)
    $native = Get-Content -Raw (Join-Path $out 'sector-512.json') | ConvertFrom-Json
    $emulated = Get-Content -Raw (Join-Path $out 'sector-512-4096.json') | ConvertFrom-Json
    Assert-PhysicalSectorGeometryRow $native 512
    Assert-PhysicalSectorGeometryRow $emulated 4096
    $differing = @(Compare-PhysicalSectorGeometryRows $native $emulated)
    [ordered]@{ native_512n = 'sector-512.json'; emulated_512e = 'sector-512-4096.json';
        compared_fields = @(Get-PhysicalSectorComparedFields); differing_fields = $differing;
        not_compared = 'physical_sector_bytes and fsutil.bytes_per_physical_sector: the variable under test' } |
        ConvertTo-Json -Depth 4 | Set-Content (Join-Path $out 'sector-512e-diff.json') -Encoding utf8
    Write-Host "sector-geometry: Windows' 512e format differs from its 512n format in $($differing.Count) field(s): $($differing -join ', ')"
}
