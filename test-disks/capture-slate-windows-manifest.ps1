# Windows' filesystem is the read oracle, independently of either NTFS reader.
# Dot-sourcing only defines the capture function. The caller owns the mount,
# and supplies the SHA-256 of the untouched raw image, not its mounted copy.
. "$PSScriptRoot\slate-windows-links.ps1"
function Get-SlateWindowsManifest {
    param(
        [Parameter(Mandatory=$true)] [string]$Root,
        [Parameter(Mandatory=$true)] [string]$Case,
        [Parameter(Mandatory=$true)] [string]$ImageSha256
    )
    if ($ImageSha256 -cnotmatch '^[0-9a-f]{64}$') { throw "$Case has an invalid image SHA-256" }
    $rootItem = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    if (-not $rootItem.PSIsContainer -or
        ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "$Root is not a plain directory" }
    $base = $rootItem.FullName.TrimEnd('\', '/')
    # These root names belong to NTFS or Windows' mount lifecycle. User
    # files named identically inside a directory are still captured.
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
            try { $kind = Get-SlateWindowsEntryKind $item $relative 'read' }
            catch { throw "Windows cannot classify ${relative}: $($_.Exception.Message)" }
            if ($kind -ceq 'symlink') {
                # Recorded by the target Windows reports; opening it would follow it.
                $target = $item.LinkTarget
                if (-not $target) { throw "Windows reports no target for symbolic link $relative" }
                $entries.Add($relative, [ordered]@{ path = $relative; kind = 'symlink'; target = $target; streams = @() })
                continue
            }
            $streams = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
            try {
                foreach ($stream in @(Get-Item -LiteralPath $item.FullName -Stream * -Force -ErrorAction Stop)) {
                    $name = if ($stream.Stream -ceq ':$DATA') { '' } else { $stream.Stream }
                    if ($item.PSIsContainer -and $name -ceq '') { continue }
                    $streamPath = if ($name -ceq '') { $item.FullName } else { "$($item.FullName):$name" }
                    $handle = [IO.File]::OpenRead($streamPath)
                    try {
                        if ($handle.Length -ne $stream.Length) { throw "stream '$name' length changed before hashing" }
                        $hash = (Get-FileHash -InputStream $handle -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
                        if ($handle.Position -ne $stream.Length) { throw "stream '$name' was not read completely" }
                    } finally { $handle.Dispose() }
                    if ($hash -cnotmatch '^[0-9a-f]{64}$') { throw "invalid SHA-256 for stream '$name'" }
                    $streams.Add($name, [ordered]@{ name = $name; size = [long]$stream.Length; sha256 = $hash })
                }
                if (-not $item.PSIsContainer -and -not $streams.ContainsKey('')) {
                    throw 'Windows did not enumerate the unnamed data stream'
                }
            } catch { throw "Windows cannot read ${relative}: $($_.Exception.Message)" }
            [string[]]$names = @($streams.Keys)
            [Array]::Sort($names, [StringComparer]::Ordinal)
            $entries.Add($relative, [ordered]@{
                path = $relative
                kind = $kind
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
        oracle = 'Windows Get-ChildItem / Get-Item -Stream / Get-FileHash SHA256'
        capabilities = [ordered]@{
            excluded_root_names = $excluded
            exclusion_reason = 'NTFS metadata and Windows mount lifecycle entries'
            reparse_points = "a symbolic link is a 'symlink' entry with the target Windows reports and no streams, never followed; any other reparse point fails by path"
            root_entry = 'implicit directory; root streams are outside this manifest'
        }
        entries = @($paths | ForEach-Object { $entries[$_] })
    }
}
