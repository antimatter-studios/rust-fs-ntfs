# Fixture-free collector contracts. Synthetic bytes test decoding only;
# the hosted capture obtains expected filesystem fields from Windows itself.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../scripts/windows-format-geometry.ps1"
$checks = 0
function Assert-Geometry([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}
function Assert-GeometryFailure([scriptblock]$Action, [string]$Message) {
    $failed = $false
    try { & $Action | Out-Null } catch { $failed = $true }
    Assert-Geometry $failed $Message
}
$requests = @(Get-FormatGeometryRequests)
Assert-Geometry ($requests.Count -eq 26) 'exactly 26 geometry requests'
Assert-Geometry (($requests | Where-Object allocation_unit_bytes -eq 512).Count -eq 2) 'both 512-byte modes'
Assert-Geometry (($requests | Where-Object allocation_unit_bytes -eq 2097152).Count -eq 2) 'both 2 MiB modes'
Assert-Geometry ((Get-PhysicalSectorComparedFields) -notcontains 'logfile.lcn') 'a live $LogFile extent is not queried or compared'
Assert-Geometry (($requests | Where-Object large_frs).Count -eq 13) '13 large-record requests'
$boot = [byte[]]::new(512)
[Text.Encoding]::ASCII.GetBytes('NTFS    ').CopyTo($boot, 3)
[BitConverter]::GetBytes([uint16]512).CopyTo($boot, 11)
$boot[13] = 8; $boot[64] = 246; $boot[68] = 1
[BitConverter]::GetBytes([uint64]4).CopyTo($boot, 48)
[BitConverter]::GetBytes([uint64]128).CopyTo($boot, 56)
$boot[510] = 85; $boot[511] = 170
$decoded = ConvertFrom-FormatBoot $boot
Assert-Geometry ($decoded.bytes_per_sector -eq 512 -and $decoded.sectors_per_cluster_byte -eq 8) 'raw sector fields'
Assert-Geometry ($decoded.clusters_per_record_byte -eq 246 -and $decoded.record_bytes -eq 1024) 'negative record exponent'
Assert-Geometry ($decoded.clusters_per_index_block_byte -eq 1 -and $decoded.index_block_bytes -eq 4096) 'positive index count'
Assert-Geometry ($decoded.mft_lcn -eq 4 -and $decoded.mftmirr_lcn -eq 128) 'both MFT locations'
$boot[13] = 244; $boot[64] = 244; $boot[68] = 244
$large = ConvertFrom-FormatBoot $boot
Assert-Geometry ($large.cluster_bytes -eq 2097152 -and $large.record_bytes -eq 4096) 'large-cluster signed exponent'
$boot[13] = 128
Assert-Geometry ((ConvertFrom-FormatBoot $boot).cluster_bytes -eq 65536) '128 sectors remains unsigned'
Assert-GeometryFailure { ConvertFrom-FormatBoot ([byte[]]::new(511)) } 'short boot sector fails'
$boot[510] = 0
Assert-GeometryFailure { ConvertFrom-FormatBoot $boot } 'missing signature fails'
$boot[510] = 85; $boot[3] = 0
Assert-GeometryFailure { ConvertFrom-FormatBoot $boot } 'non-NTFS boot sector fails'
$lines = @('Bytes Per Sector : 512', 'Bytes Per Cluster : 4096 (4 KB)',
    'Bytes Per FileRecord Segment : 1024', 'Mft Start Lcn : 0x0000000000000004',
    'Mft2 Start Lcn : 0x0000000000000080')
$native = ConvertFrom-GeometryNtfsInfo $lines
Assert-Geometry ($native.cluster_bytes -eq 4096 -and $native.record_bytes -eq 1024 -and
    $native.mftmirr_lcn -eq 128) 'fsutil decimal, hex, and parenthesized units'
Assert-GeometryFailure { ConvertFrom-GeometryNtfsInfo $lines[0..3] } 'missing fsutil field fails'
Assert-GeometryFailure { ConvertFrom-GeometryNtfsInfo ($lines + $lines[0]) } 'duplicate fsutil field fails'
Assert-FormatGeometryMatch $decoded $native 4096
$checks++
$native.record_bytes = 4096
Assert-GeometryFailure { Assert-FormatGeometryMatch $decoded $native 4096 } 'native disagreement fails'
Assert-GeometryFailure { Assert-FormatGeometryMatch $decoded (ConvertFrom-GeometryNtfsInfo $lines) 8192 } 'wrong requested allocation fails'
$rows = @($requests | ForEach-Object {
    [pscustomobject]@{ allocation_unit_bytes = $_.allocation_unit_bytes; large_frs = $_.large_frs;
        accepted = ($_.allocation_unit_bytes -eq 4096); error_id = 'native-refusal' }
})
Assert-FormatGeometryRows $rows
$checks++
Assert-GeometryFailure { Assert-FormatGeometryRows $rows[0..24] } 'incomplete request set fails'
Assert-GeometryFailure { Assert-FormatGeometryRows (@($rows[0]) + @($rows[0..24])) } 'duplicate request fails'
$rows | ForEach-Object { $_.accepted = $false }
Assert-GeometryFailure { Assert-FormatGeometryRows $rows } 'all-refusal infrastructure failure fails'
$rows | ForEach-Object { $_.accepted = ($_.allocation_unit_bytes -eq 4096) }
$rows[0].error_id = ''
Assert-GeometryFailure { Assert-FormatGeometryRows $rows } 'refusal without diagnostic fails'
$layoutRequests = @(Get-LargeClusterLayoutRequests)
Assert-Geometry (($layoutRequests -join ',') -eq '65536,131072,262144,524288,1048576,2097152') '64 KiB control and five large clusters'
$extents = @([pscustomobject]@{ vcn = 0; next_vcn = 2; lcn = 40 }, [pscustomobject]@{ vcn = 2; next_vcn = 5; lcn = 90 })
$run = ConvertFrom-RetrievalExtents $extents
Assert-Geometry ($run.lcn -eq 40 -and $run.clusters -eq 5 -and $run.extents.Count -eq 2) 'first LCN and total clusters'
Assert-GeometryFailure { ConvertFrom-RetrievalExtents @() } 'no extents fails'
Assert-GeometryFailure { ConvertFrom-RetrievalExtents @([pscustomobject]@{ vcn = 0; next_vcn = 1; lcn = -1 }) } 'unallocated extent fails'
Assert-GeometryFailure { ConvertFrom-RetrievalExtents @($extents[0], [pscustomobject]@{ vcn = 3; next_vcn = 5; lcn = 90 }) } 'VCN gap fails'
$layoutRows = @($layoutRequests | ForEach-Object {
    [pscustomobject]@{ allocation_unit_bytes = $_; accepted = $true; mftmirr = $run; sds = $run; root_index = $run }
})
Assert-LargeClusterLayoutRows $layoutRows
$checks++
Assert-GeometryFailure { Assert-LargeClusterLayoutRows $layoutRows[0..4] } 'missing large-cluster size fails'
$layoutRows[3].sds = $null
Assert-GeometryFailure { Assert-LargeClusterLayoutRows $layoutRows } 'missing layout field fails'
$layoutRows[3].sds = $run; $layoutRows[0].accepted = $false
Assert-GeometryFailure { Assert-LargeClusterLayoutRows $layoutRows } 'failed 64 KiB control fails'
# 4Kn: a 4096-byte boot sector, the update sequence header of a raw record,
# and the row that must come from a disk Windows reports as 4096-byte sectors.
$boot4k = [byte[]]::new(4096)
[Text.Encoding]::ASCII.GetBytes('NTFS    ').CopyTo($boot4k, 3)
[BitConverter]::GetBytes([uint16]4096).CopyTo($boot4k, 11)
$boot4k[13] = 1; $boot4k[64] = 1; $boot4k[68] = 1
[BitConverter]::GetBytes([uint64]262143).CopyTo($boot4k, 40)
[BitConverter]::GetBytes([uint64]4).CopyTo($boot4k, 48)
[BitConverter]::GetBytes([uint64]2).CopyTo($boot4k, 56)
$boot4k[510] = 85; $boot4k[511] = 170
$decoded4k = ConvertFrom-FormatBoot $boot4k -SectorBytes 4096
Assert-Geometry ($decoded4k.bytes_per_sector -eq 4096 -and $decoded4k.cluster_bytes -eq 4096 -and
    $decoded4k.record_bytes -eq 4096 -and $decoded4k.index_block_bytes -eq 4096) '4096-byte boot sector fields'
Assert-Geometry ($decoded4k.number_sectors -eq 262143) 'boot sector NumberSectors'
Assert-GeometryFailure { ConvertFrom-FormatBoot $boot4k } '4096-byte sectors fail the 512-byte decoder'
Assert-GeometryFailure { ConvertFrom-FormatBoot ([byte[]]::new(512)) -SectorBytes 4096 } 'short 4096-byte boot sector fails'
$record = [byte[]]::new(4096)
[Text.Encoding]::ASCII.GetBytes('FILE').CopyTo($record, 0)
[BitConverter]::GetBytes([uint16]48).CopyTo($record, 4)
[BitConverter]::GetBytes([uint16]9).CopyTo($record, 6)
$usa = ConvertFrom-UpdateSequenceHeader $record 'FILE'
Assert-Geometry ($usa.usa_offset -eq 48 -and $usa.usa_count -eq 9 -and $usa.stride_bytes -eq 512) 'USA stride from its count'
[BitConverter]::GetBytes([uint16]2).CopyTo($record, 6)
Assert-Geometry ((ConvertFrom-UpdateSequenceHeader $record 'FILE').stride_bytes -eq 4096) 'one 4096-byte stride'
Assert-GeometryFailure { ConvertFrom-UpdateSequenceHeader $record 'INDX' } 'wrong record magic fails'
[BitConverter]::GetBytes([uint16]7).CopyTo($record, 6)
Assert-GeometryFailure { ConvertFrom-UpdateSequenceHeader $record 'FILE' } 'a count that does not divide the record fails'
$lines4k = @('Bytes Per Sector : 4096', 'Bytes Per Physical Sector : 4096', 'Bytes Per Cluster : 4096',
    'Bytes Per FileRecord Segment : 4096', 'Mft Start Lcn : 0x0000000000000004',
    'Mft2 Start Lcn : 0x0000000000000002')
$native4k = ConvertFrom-GeometryNtfsInfo $lines4k -Physical
Assert-Geometry ($native4k.bytes_per_sector -eq 4096 -and $native4k.bytes_per_physical_sector -eq 4096) 'fsutil physical sector field'
Assert-GeometryFailure { ConvertFrom-GeometryNtfsInfo ($lines4k | Where-Object { $_ -notmatch 'Physical' }) -Physical } 'missing physical sector field fails'
$sectorRow = [pscustomobject][ordered]@{ disk_tool = 'New-VHD'; logical_sector_bytes = 4096;
    physical_sector_bytes = 4096; fsutil = $native4k; boot = $decoded4k; backup_boot_offsets = @(1073737728);
    mft_usa = $usa; root_index_usa = $usa }
Assert-SectorGeometryRow $sectorRow
$checks++
$sectorRow.logical_sector_bytes = 512
Assert-GeometryFailure { Assert-SectorGeometryRow $sectorRow } 'a 512-byte scratch disk cannot pass as 4Kn'
$sectorRow.logical_sector_bytes = 4096; $sectorRow.root_index_usa = $null
Assert-GeometryFailure { Assert-SectorGeometryRow $sectorRow } 'missing 4Kn field fails'
$sectorRow.root_index_usa = $usa; $sectorRow.backup_boot_offsets = @()
Assert-GeometryFailure { Assert-SectorGeometryRow $sectorRow } 'no backup boot sector found fails'
$sectorRow.backup_boot_offsets = @(1073737728); $native4k.bytes_per_sector = 512
Assert-GeometryFailure { Assert-SectorGeometryRow $sectorRow } 'fsutil disagreeing with the 4Kn boot sector fails'
# 512e beside 512n: both rows must come from disks reporting 512-byte logical
# sectors and the physical size asked for, and the diff names what Windows changed.
$bootBytes512 = [byte[]]::new(512)
[Text.Encoding]::ASCII.GetBytes('NTFS    ').CopyTo($bootBytes512, 3)
[BitConverter]::GetBytes([uint16]512).CopyTo($bootBytes512, 11)
$bootBytes512[13] = 8; $bootBytes512[64] = 246; $bootBytes512[68] = 1
[BitConverter]::GetBytes([uint64]2097151).CopyTo($bootBytes512, 40)
[BitConverter]::GetBytes([uint64]4).CopyTo($bootBytes512, 48)
[BitConverter]::GetBytes([uint64]128).CopyTo($bootBytes512, 56)
$bootBytes512[510] = 85; $bootBytes512[511] = 170
$boot512 = ConvertFrom-FormatBoot $bootBytes512
$usa512 = [pscustomobject][ordered]@{ record_bytes = 1024; usa_offset = 48; usa_count = 3; stride_bytes = 512 }
function New-PhysicalRow([int]$Physical) {
    [pscustomobject][ordered]@{ disk_tool = 'New-VHD'; logical_sector_bytes = 512; physical_sector_bytes = $Physical;
        partition_offset = 16777216; fsutil = (ConvertFrom-GeometryNtfsInfo @('Bytes Per Sector : 512',
            "Bytes Per Physical Sector : $Physical", 'Bytes Per Cluster : 4096', 'Bytes Per FileRecord Segment : 1024',
            'Mft Start Lcn : 0x0000000000000004', 'Mft2 Start Lcn : 0x0000000000000080') -Physical)
        boot = $boot512; backup_boot_offsets = @(1073741312); mft_usa = $usa512; root_index_usa = $usa512; logfile = $null }
}
$row512e = New-PhysicalRow 4096
Assert-Geometry ($row512e.fsutil.bytes_per_physical_sector -eq 4096 -and $row512e.fsutil.cluster_bytes -eq 4096) '512e fsutil row decodes'
Assert-PhysicalSectorGeometryRow $row512e 4096
$checks++
Assert-PhysicalSectorGeometryRow (New-PhysicalRow 512) 512
$checks++
$row512e.physical_sector_bytes = 512
Assert-GeometryFailure { Assert-PhysicalSectorGeometryRow $row512e 4096 } 'a 512n scratch disk cannot pass as 512e'
$row512e.physical_sector_bytes = 4096; $row512e.logical_sector_bytes = 4096
Assert-GeometryFailure { Assert-PhysicalSectorGeometryRow $row512e 4096 } 'a 4Kn scratch disk cannot pass as 512e'
$row512e.logical_sector_bytes = 512; $row512e.fsutil.bytes_per_physical_sector = 512
Assert-GeometryFailure { Assert-PhysicalSectorGeometryRow $row512e 4096 } 'fsutil disagreeing with the disk physical size fails'
$row512e.fsutil.bytes_per_physical_sector = 4096; $row512e.partition_offset = $null
Assert-GeometryFailure { Assert-PhysicalSectorGeometryRow $row512e 4096 } 'missing partition offset fails'
$row512e.partition_offset = 16777216
$row512n = New-PhysicalRow 512
Assert-Geometry (@(Compare-PhysicalSectorGeometryRows $row512n $row512e).Count -eq 0) 'only the physical sector size differs: empty diff'
$row512e.boot = ConvertFrom-FormatBoot $bootBytes512
$row512e.boot.mft_lcn = 8
$diff = @(Compare-PhysicalSectorGeometryRows $row512n $row512e)
Assert-Geometry (($diff -join ',') -eq 'boot.mft_lcn') 'the diff names each changed field and excludes the locked $LogFile'
$row512e.partition_offset = 1048576
Assert-Geometry (@(Compare-PhysicalSectorGeometryRows $row512n $row512e) -contains 'partition_offset') 'the diff names a moved partition'
# The index VCN unit (#704): one unit dividing every later block's offset.
$vcnBlocks = @([pscustomobject]@{ offset = 0; vcn = 0 }, [pscustomobject]@{ offset = 4096; vcn = 8 },
    [pscustomobject]@{ offset = 8192; vcn = 16 })
Assert-Geometry ((Measure-IndexVcnUnit $vcnBlocks) -eq 512) 'blocks 4096 bytes apart at VCNs 8 apart measure a 512-byte unit'
$clusterBlocks = @([pscustomobject]@{ offset = 0; vcn = 0 }, [pscustomobject]@{ offset = 65536; vcn = 1 },
    [pscustomobject]@{ offset = 131072; vcn = 2 })
Assert-Geometry ((Measure-IndexVcnUnit $clusterBlocks) -eq 65536) 'blocks a cluster apart at consecutive VCNs measure a cluster unit'
Assert-GeometryFailure { Measure-IndexVcnUnit @($vcnBlocks[0], $vcnBlocks[1], $clusterBlocks[2]) } 'blocks disagreeing on the unit fail'
Assert-GeometryFailure { Measure-IndexVcnUnit @($vcnBlocks[0], $vcnBlocks[1]) } 'one block past the first measures nothing'
Assert-GeometryFailure { Measure-IndexVcnUnit @($vcnBlocks[1], $vcnBlocks[2]) } 'no block at offset 0 fails'
Assert-GeometryFailure { Measure-IndexVcnUnit @($vcnBlocks[0], [pscustomobject]@{ offset = 4096; vcn = 3 },
    [pscustomobject]@{ offset = 8192; vcn = 6 }) } 'a VCN that does not divide its offset fails'
# The smallest volume Windows formats (#961): the sizes asked for, chkdsk's
# report of the live $LogFile, fsutil's cluster count, and the rows' shape.
Assert-Geometry (((Get-SmallVolumeRequests) -join ',') -eq '1,2,3,4,5,6,7,8,12,16') 'small volumes from 1 to 16 MiB, ascending'
$chkdskL = @('The type of the file system is NTFS.', 'The current log file size is 2048 KB.',
    'The default log file size for this volume is 2048 KB.')
Assert-Geometry ((ConvertFrom-ChkdskLogSize $chkdskL) -eq 2097152) 'chkdsk /L reports the current log size in KB'
Assert-GeometryFailure { ConvertFrom-ChkdskLogSize @($chkdskL[0], $chkdskL[2]) } 'a report without the current size fails'
Assert-Geometry ((Get-NtfsInfoField @('Total Clusters :                  0x0000000000000800') 'Total Clusters') -eq 2048) 'fsutil total clusters in hex'
Assert-Geometry ((Get-NtfsInfoField @('Total Clusters    :                    1,535  (  6.0 MB)') 'Total Clusters') -eq 1535) 'fsutil total clusters with separators and a decimal size'
function New-SmallRow([int]$Mib, [bool]$Accepted) {
    [pscustomobject][ordered]@{ size_mib = $Mib; accepted = $Accepted; log_bytes = $(if ($Accepted) { 1048576 });
        mft_lcn = $(if ($Accepted) { 4 }); mftmirr_lcn = $(if ($Accepted) { 255 }); total_clusters = $(if ($Accepted) { 511 }) }
}
$smallRows = @((New-SmallRow 1 $false), (New-SmallRow 2 $true), (New-SmallRow 3 $true))
Assert-Geometry ((Assert-SmallVolumeRows $smallRows) -eq 2) 'the smallest accepted size is reported'
Assert-GeometryFailure { Assert-SmallVolumeRows @((New-SmallRow 1 $false), (New-SmallRow 2 $false)) } 'no accepted size fails'
Assert-GeometryFailure { Assert-SmallVolumeRows @((New-SmallRow 2 $true), (New-SmallRow 1 $false)) } 'rows out of order fail'
$unmeasured = New-SmallRow 4 $true
$unmeasured.log_bytes = $null
Assert-GeometryFailure { Assert-SmallVolumeRows @((New-SmallRow 1 $false), $unmeasured) } 'an accepted size without its $LogFile size fails'
Assert-GeometryFailure { Assert-SmallVolumeRows @() } 'no rows fail'
if ($checks -ne 75) { throw "only $checks geometry contracts executed, expected 75" }
Write-Host "windows-format-geometry.ps1: all checks passed ($checks; floor 75)"
