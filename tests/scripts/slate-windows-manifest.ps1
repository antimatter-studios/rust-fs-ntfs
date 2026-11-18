# Windows is the byte oracle. This contract uses real NTFS files and ADS,
# without a VM or a raw-image mount. Run on windows-latest in the Slate tier.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../test-disks/capture-slate-windows-manifest.ps1"
$work = Join-Path ([IO.Path]::GetTempPath()) ("slate-manifest-" + [guid]::NewGuid())
$outside = Join-Path ([IO.Path]::GetTempPath()) ("slate-manifest-outside-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path "$outside/tree" -Force | Out-Null
$checks = 0
function Assert-Manifest([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}
try {
    New-Item -ItemType Directory -Path "$work/nested", "$work/empty-dir", "$work/`$Recycle.Bin" -Force | Out-Null
    [IO.File]::WriteAllBytes("$work/nested/雪.bin", [byte[]](0, 255, 128, 10))
    [IO.File]::WriteAllBytes("$work/empty", [byte[]]@())
    [IO.File]::WriteAllBytes("$work/nested/雪.bin:note", [byte[]](7, 0, 8))
    [IO.File]::WriteAllBytes("$work/empty-dir:directory-note", [byte[]](42))
    [IO.File]::WriteAllText("$work/`$Recycle.Bin/mount-owned", 'excluded')
    $digest = 'a' * 64
    $manifest = Get-SlateWindowsManifest -Root $work -Case 'contract' -ImageSha256 $digest
    Assert-Manifest ($manifest.schema -eq 1 -and $manifest.case -eq 'contract' -and
        $manifest.image_sha256 -ceq $digest) 'the manifest is bound to the case and original image SHA-256'
    $paths = @($manifest.entries | ForEach-Object { $_.path })
    Assert-Manifest (($paths -join '|') -ceq '/empty|/empty-dir|/nested|/nested/雪.bin') 'all user paths are retained in ordinal order'
    $file = @($manifest.entries | Where-Object path -CEQ '/nested/雪.bin')[0]
    $unnamed = @($file.streams | Where-Object name -CEQ '')[0]
    $named = @($file.streams | Where-Object name -CEQ 'note')[0]
    Assert-Manifest ($unnamed.size -eq 4 -and $unnamed.sha256 -ceq
        (Get-FileHash -LiteralPath "$work/nested/雪.bin" -Algorithm SHA256).Hash.ToLowerInvariant()) 'Windows hashes the unnamed binary bytes'
    Assert-Manifest ($named.size -eq 3 -and $named.sha256 -ceq
        (Get-FileHash -LiteralPath "$work/nested/雪.bin:note" -Algorithm SHA256).Hash.ToLowerInvariant()) 'Windows hashes named-stream bytes'
    $empty = @($manifest.entries | Where-Object path -CEQ '/empty')[0]
    Assert-Manifest ($empty.streams.Count -eq 1 -and $empty.streams[0].size -eq 0 -and
        $empty.streams[0].sha256 -ceq 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855') 'empty files retain their unnamed stream and SHA-256'
    $directory = @($manifest.entries | Where-Object path -CEQ '/empty-dir')[0]
    Assert-Manifest ($directory.kind -eq 'directory' -and $directory.streams.Count -eq 1 -and
        $directory.streams[0].name -ceq 'directory-note' -and $directory.streams[0].size -eq 1) 'directories and their named streams are retained'
    Assert-Manifest (@($manifest.capabilities.excluded_root_names).Contains('$Recycle.Bin') -and
        @($manifest.capabilities.excluded_root_names).Contains('$Extend')) 'all namespace exclusions are explicit'
    $again = Get-SlateWindowsManifest -Root $work -Case 'contract' -ImageSha256 $digest
    Assert-Manifest (($manifest | ConvertTo-Json -Depth 10 -Compress) -ceq
        ($again | ConvertTo-Json -Depth 10 -Compress)) 'repeated capture is deterministic'
    $rejected = $false
    try { Get-SlateWindowsManifest -Root "$work/missing" -Case 'contract' -ImageSha256 $digest | Out-Null } catch { $rejected = $true }
    Assert-Manifest $rejected 'an unreadable root fails rather than publishing an empty namespace'
    $rejected = $false
    try { Get-SlateWindowsManifest -Root $work -Case 'contract' -ImageSha256 'wrong' | Out-Null } catch { $rejected = $true }
    Assert-Manifest $rejected 'an invalid image binding fails'
    # A read failure must name the path and must not silently omit its stream.
    $held = [IO.File]::Open("$work/empty", [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        $failure = ''
        try { Get-SlateWindowsManifest -Root $work -Case 'contract' -ImageSha256 $digest | Out-Null } catch { $failure = $_.Exception.Message }
        Assert-Manifest ($failure -match 'empty') 'a sharing violation fails by path'
    } finally { $held.Dispose() }
    # Symbolic links are recorded with the target Windows reports and never
    # followed (#1357): a relative and an absolute link inside the root, and
    # links to a file and a directory outside it, whose bytes and names must
    # not reach the manifest.
    [IO.File]::WriteAllBytes("$outside/secret.bin", [byte[]](9, 9, 9, 9, 9))
    [IO.File]::WriteAllBytes("$outside/tree/inside-outside.bin", [byte[]](1))
    New-Item -ItemType SymbolicLink -Path "$work/rel-link" -Value 'nested\雪.bin' | Out-Null
    New-Item -ItemType SymbolicLink -Path "$work/nested/abs-link" -Value "$work\empty" | Out-Null
    New-Item -ItemType SymbolicLink -Path "$work/out-file" -Value "$outside\secret.bin" | Out-Null
    New-Item -ItemType SymbolicLink -Path "$work/out-dir" -Value "$outside\tree" | Out-Null
    $linked = Get-SlateWindowsManifest -Root $work -Case 'contract' -ImageSha256 $digest
    $links = @{}
    foreach ($entry in $linked.entries) { $links[$entry.path] = $entry }
    Assert-Manifest ($links['/rel-link'].kind -ceq 'symlink' -and $links['/rel-link'].target -ceq 'nested\雪.bin' -and
        $links['/rel-link'].streams.Count -eq 0) 'a relative file symlink is recorded with its target and no followed stream'
    Assert-Manifest ($links['/nested/abs-link'].kind -ceq 'symlink' -and
        $links['/nested/abs-link'].target -ceq "$work\empty" -and
        $links['/nested/abs-link'].streams.Count -eq 0) 'an absolute file symlink is recorded with its target and no followed stream'
    $leaked = @($linked.entries | Where-Object { $_.path -like '/out-dir/*' })
    $outsideHash = (Get-FileHash -LiteralPath "$outside/secret.bin" -Algorithm SHA256).Hash.ToLowerInvariant()
    $json = $linked | ConvertTo-Json -Depth 10 -Compress
    Assert-Manifest ($links['/out-file'].kind -ceq 'symlink' -and $links['/out-dir'].kind -ceq 'symlink' -and
        $leaked.Count -eq 0 -and -not $json.Contains($outsideHash) -and
        -not $json.Contains('inside-outside.bin')) 'targets outside the captured root are never traversed'
    if ($checks -lt 14) { throw "only $checks manifest contracts executed, floor 14" }
    Write-Host "slate-windows-manifest.ps1: all checks passed ($checks; floor 14)"
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction Stop
    Remove-Item -LiteralPath $outside -Recurse -Force -ErrorAction Stop
}
