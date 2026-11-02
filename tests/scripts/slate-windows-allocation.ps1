# Windows is the allocation oracle. Files Windows writes on a volume Windows
# formats, and Windows' own account of where they live, are the expected
# values; no NTFS reader from this crate or Slate is consulted. The volume is
# a scratch fixture, so nothing else changes its free count mid-capture.
# Run elevated on windows-latest in the Slate tier (#875).
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../scripts/fs-windows-test-harness/_lib.ps1"
. "$PSScriptRoot/../../test-disks/windows-fixture.ps1"
. "$PSScriptRoot/../../test-disks/capture-slate-windows-allocation.ps1"
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$work = Join-Path $tempRoot ("slate-allocation-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
$checks = 0
function Assert-Allocation([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}
function Get-Stream($Manifest, [string]$Path, [string]$Name) {
    $entry = @($Manifest.entries | Where-Object path -CEQ $Path)[0]
    if (-not $entry) { throw "no entry for $Path" }
    @($entry.streams | Where-Object name -CEQ $Name)[0]
}
$digest = 'b' * 64
# The populate block runs as an inline call, so it fills this table.
$captured = @{}
try {
    New-WindowsFixture -ImagePath (Join-Path $work 'allocation.img') -Label 'SLATEALLOC' `
        -AllocationUnitSize 4096 -Populate {
            param($root)
            $bytes = [byte[]]::new(256KB)
            (New-Object Random 875).NextBytes($bytes)
            New-Item -ItemType Directory -Path "${root}dir" | Out-Null
            [IO.File]::WriteAllBytes("${root}small", [byte[]](1, 2, 3))
            [IO.File]::WriteAllBytes("${root}dir\big.bin", $bytes)
            [IO.File]::WriteAllBytes("${root}dir\big.bin:side", $bytes[0..(64KB - 1)])
            # 1 MiB logical, only its last cluster written: a leading hole.
            [IO.File]::WriteAllBytes("${root}sparse", [byte[]]@())
            & fsutil sparse setflag "${root}sparse" | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "fsutil sparse setflag exited $LASTEXITCODE" }
            $stream = [IO.File]::Open("${root}sparse", 'Open', 'ReadWrite')
            try { $stream.SetLength(1MB) }
            finally { $stream.Dispose() }
            # Make the leading range a hole, then write the tail so its clusters
            # cannot be released with the zero range on Windows.
            & fsutil sparse setrange "${root}sparse" 0 (1MB - 64KB) | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "fsutil sparse setrange exited $LASTEXITCODE" }
            $stream = [IO.File]::Open("${root}sparse", 'Open', 'ReadWrite')
            try {
                $stream.Position = 1MB - 4KB
                $stream.Write($bytes, 0, 4KB)
                $stream.Flush($true)
            } finally { $stream.Dispose() }
            $captured.volumeFree = (Get-Volume -DriveLetter $root.Substring(0, 1)).SizeRemaining
            $captured.manifest = Get-SlateWindowsAllocation -Root $root -Case 'contract' -ImageSha256 $digest
            $captured.again = Get-SlateWindowsAllocation -Root $root -Case 'contract' -ImageSha256 $digest
            $captured.rejectedSha = $false
            try { Get-SlateWindowsAllocation -Root $root -Case 'contract' -ImageSha256 'wrong' | Out-Null }
            catch { $captured.rejectedSha = $true }
            $captured.rejectedRoot = $false
            try { Get-SlateWindowsAllocation -Root "${root}dir" -Case 'contract' -ImageSha256 $digest | Out-Null }
            catch { $captured.rejectedRoot = $true }
            New-Item -ItemType Junction -Path "${root}link" -Target "${root}dir" | Out-Null
            $captured.reparseFailure = ''
            try { Get-SlateWindowsAllocation -Root $root -Case 'contract' -ImageSha256 $digest | Out-Null }
            catch { $captured.reparseFailure = $_.Exception.Message }
        }
    $volumeFree = $captured.volumeFree; $manifest = $captured.manifest; $again = $captured.again
    $rejectedSha = $captured.rejectedSha; $rejectedRoot = $captured.rejectedRoot
    $reparseFailure = $captured.reparseFailure
    Assert-Allocation ($manifest.schema -eq 1 -and $manifest.case -eq 'contract' -and
        $manifest.image_sha256 -ceq $digest) 'the allocation manifest is bound to the case and original image SHA-256'
    $volume = $manifest.volume
    Assert-Allocation ($volume.bytes_per_cluster -eq 4096) 'the cluster size is the one Windows formatted with'
    Assert-Allocation ($volume.total_clusters -gt $volume.free_clusters -and $volume.free_clusters -gt 0) 'the volume records total and free clusters'
    # Get-Volume counts free bytes through another Windows API; it never
    # reports more than the free clusters, nor less than the unreserved ones.
    Assert-Allocation ($volumeFree -le $volume.free_clusters * 4096 -and
        $volumeFree -ge ($volume.free_clusters - $volume.reserved_clusters) * 4096) "Get-Volume's free bytes ($volumeFree) agree with the free clusters"
    $small = Get-Stream $manifest '/small' ''
    Assert-Allocation ($small.extents.Count -eq 0 -and $small.allocated_clusters -eq 0) 'a resident stream has no extents'
    $big = Get-Stream $manifest '/dir/big.bin' ''
    Assert-Allocation ($big.allocated_clusters -eq 64 -and
        (@($big.extents | Where-Object { $null -eq $_.lcn }).Count -eq 0)) 'a written 256 KiB stream holds 64 allocated clusters'
    $vcn = 0
    foreach ($extent in $big.extents) { if ($extent.vcn -ne $vcn) { $vcn = -1; break }; $vcn += $extent.clusters }
    Assert-Allocation ($vcn -eq 64) 'extents cover the stream contiguously from VCN 0'
    $side = Get-Stream $manifest '/dir/big.bin' 'side'
    Assert-Allocation ($side.allocated_clusters -eq 16) 'a named stream records its own 16 clusters'
    $sparse = Get-Stream $manifest '/sparse' ''
    # NTFS may allocate a whole 16-cluster compression unit for the write.
    $runs = @($sparse.extents)
    $last = $runs[$runs.Count - 1]
    $sparseDescription = $sparse | ConvertTo-Json -Depth 8 -Compress
    Assert-Allocation ($runs.Count -ge 2 -and $runs[0].vcn -eq 0 -and $null -eq $runs[0].lcn -and
        $null -ne $last.lcn -and $last.vcn + $last.clusters -eq 256 -and
        $sparse.allocated_clusters -ge 1 -and $sparse.allocated_clusters -le 16) "a sparse stream records its leading hole and its allocated tail: $sparseDescription"
    $directory = @($manifest.entries | Where-Object path -CEQ '/dir')[0]
    Assert-Allocation ($directory.kind -eq 'directory' -and $directory.streams.Count -eq 0) 'a directory without named streams has no stream records'
    Assert-Allocation (($manifest | ConvertTo-Json -Depth 10 -Compress) -ceq
        ($again | ConvertTo-Json -Depth 10 -Compress)) 'repeated capture is deterministic'
    Assert-Allocation $rejectedSha 'an invalid image binding fails'
    Assert-Allocation $rejectedRoot 'a root that is not a volume root fails'
    Assert-Allocation ($reparseFailure -match 'link') 'a reparse point fails by path'
    if ($checks -lt 14) { throw "only $checks allocation contracts executed, floor 14" }
    Write-Host "slate-windows-allocation.ps1: all checks passed ($checks; floor 14)"
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction Stop
}
