# Data-volume fixtures need NTFS metadata, never the formatter's boot program.
function Clear-NtfsBootstrap([string]$Path, [long]$PartitionOffset, [long]$PartitionSize) {
    $stream = [IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
    try {
        if ($PartitionOffset -lt 0 -or $PartitionSize -lt 8704 -or
            $PartitionOffset -gt $stream.Length - $PartitionSize) {
            throw 'NTFS partition is outside the fixed VHD'
        }
        $boot = [byte[]]::new(512)
        $stream.Position = $PartitionOffset
        $stream.ReadExactly($boot, 0, 512)
        if ([Text.Encoding]::ASCII.GetString($boot, 3, 8) -ne 'NTFS    ' -or
            [BitConverter]::ToUInt16($boot, 11) -ne 512 -or
            $boot[510] -ne 0x55 -or $boot[511] -ne 0xaa) {
            throw 'NTFS BPB or boot signature is invalid'
        }
        $sectors = [BitConverter]::ToUInt64($boot, 40)
        if ($sectors -ne ($PartitionSize / 512 - 1)) {
            throw 'NTFS backup boot sector does not end the partition'
        }
        $backupOffset = $PartitionOffset + $PartitionSize - 512
        $backup = [byte[]]::new(512)
        $stream.Position = $backupOffset
        $stream.ReadExactly($backup, 0, 512)
        if ($backup[510] -ne 0x55 -or $backup[511] -ne 0xaa) {
            throw 'NTFS backup boot signature is invalid'
        }
        for ($i = 0; $i -lt 84; $i++) {
            if ($backup[$i] -ne $boot[$i]) { throw 'NTFS backup BPB differs' }
        }
        # Validate everything before the first write. Preserve jump, BPB,
        # signatures, all filesystem state, partition table and VHD footer.
        $stream.Position = $PartitionOffset + 84
        $stream.Write([byte[]]::new(426), 0, 426)
        $stream.Position = $PartitionOffset + 512
        $stream.Write([byte[]]::new(7680), 0, 7680)
        $stream.Position = $backupOffset + 84
        $stream.Write([byte[]]::new(426), 0, 426)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
}
