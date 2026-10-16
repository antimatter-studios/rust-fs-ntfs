# CI-only producer: retain mounted Windows logs before clean detach downgrades
# the log format. Reuse the native VSS experiment, bounded to three snapshots.
$ErrorActionPreference = 'Stop'
$capture = Join-Path $env:RUNNER_TEMP 'logfile-checkpoint-capture'
& "$PSScriptRoot/capture-interrupted-logfile.ps1" -Candidates 3 -SizeMB 128 `
    -WorkloadSeconds 24 -Writers 1 -ClusterSize 4096 -CheckpointProbe -Out $capture
$layout = Get-Content (Join-Path $capture 'layout.json') -Raw | ConvertFrom-Json
$offset = [long]$layout.partition_offset
$length = [long]$layout.partition_size
if ($offset -lt 512 -or $length -le 0 -or $length -gt 128MB) {
    throw 'Native capture returned unexpected partition bounds'
}
$rows = @()
foreach ($candidate in 1..4) {
    $source = Join-Path $capture "pre-$candidate.vhd"
    $output = Join-Path $PSScriptRoot "logfile-checkpoint-$candidate.img"
    $inputStream = [System.IO.File]::OpenRead($source)
    try {
        if ($offset + $length -gt $inputStream.Length - 512) { throw 'Partition exceeds fixed VHD payload' }
        $inputStream.Position = $offset
        $outputStream = [System.IO.File]::Create($output)
        try {
            $buffer = New-Object byte[] 1048576
            $remaining = $length
            while ($remaining -gt 0) {
                $count = $inputStream.Read($buffer, 0, [int][Math]::Min($buffer.Length, $remaining))
                if ($count -eq 0) { throw 'Truncated Windows VSS capture' }
                $outputStream.Write($buffer, 0, $count)
                $remaining -= $count
            }
        } finally { $outputStream.Dispose() }
    } finally { $inputStream.Dispose() }
    $rows += [ordered]@{
        candidate = $candidate; partition_offset = $offset; partition_bytes = $length
        image = [System.IO.Path]::GetFileName($output)
        sha256 = (Get-FileHash $output -Algorithm SHA256).Hash.ToLowerInvariant()
        vhd_sha256 = (Get-Content (Join-Path $capture "pre-$candidate.sha256") -Raw).Trim()
    }
}
[ordered]@{
    producer = 'capture-interrupted-logfile.ps1'; candidates = @($rows | Where-Object candidate -le 3)
    checkpoint_probe = $rows[3]
    checkpoint_probe_workload = @(Get-Content (Join-Path $capture 'checkpoint-probe.tsv'))
    snapshots = @(Get-Content (Join-Path $capture 'snapshots.tsv'))
    windows = [Environment]::OSVersion.VersionString
} | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $PSScriptRoot 'logfile-checkpoint-provenance.json')
