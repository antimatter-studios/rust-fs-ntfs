# New-WindowsFixture: formats a 256 MiB raw image as NTFS through Windows,
# hands the mounted root to a populate block, and writes the volume back to
# the raw image. Shared by build-windows-native-read-fixtures.ps1 and
# capture-filename-fields.ps1.
#
# Dot-sourcing this file only defines the function: it builds nothing, mounts
# nothing and changes no state, so either producer can load it safely.
#
# The populate block receives the volume root ("X:\") while the volume is
# still mounted. The caller must have dot-sourced
# scripts/fs-windows-test-harness/_lib.ps1 first, and for the CLIREAD label
# also test-disks/capture-python-directory-oracle.ps1.

function New-WindowsFixture {
    param(
        [Parameter(Mandatory=$true)] [string]$ImagePath,
        [Parameter(Mandatory=$true)] [string]$Label,
        [Parameter(Mandatory=$true)] [scriptblock]$Populate,
        [int]$AllocationUnitSize = 4096
    )

    $tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
    $image = [System.IO.Path]::GetFullPath($ImagePath)
    $diag = Join-Path $tempRoot ([System.IO.Path]::GetFileNameWithoutExtension($image))
    New-Item -ItemType Directory -Path $diag -Force | Out-Null
    Remove-Item $image,([System.IO.Path]::ChangeExtension($image, '.vhd')) `
        -Force -ErrorAction SilentlyContinue

    $vhdPath = Get-VhdPathFor -ImagePath $image
    $complete = $false
    try {
        $stream = [System.IO.File]::Create($image)
        try { $stream.SetLength(256MB) } finally { $stream.Close() }
        $state = Initialize-VhdFromImg -ImagePath $image -Diag $diag
        $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
        Format-Volume -DriveLetter $letter -FileSystem NTFS `
            -AllocationUnitSize $AllocationUnitSize -NewFileSystemLabel $Label `
            -Force -Confirm:$false | Out-Null
        & $Populate "${letter}:\"
        if ($Label -eq 'CLIREAD') {
            $directoryOracle = New-PythonDirectoryOracle -Root "${letter}:\"
        }
        # Windows' own account of the mounted volume's serial and geometry,
        # graded against the Python client in python-volume-info-windows.
        if ($Label -eq 'CLIREAD') {
            $ntfsinfo = & fsutil fsinfo ntfsinfo "${letter}:"
            if ($LASTEXITCODE -ne 0) { throw "fsutil fsinfo ntfsinfo ${letter}: failed" }
        }
        Sync-VhdToImg -Vhd $state.Vhd -ImagePath $image
        if ($Label -eq 'CLIREAD') {
            $digest = (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant()
            $record = @("image sha256 : $digest", "git HEAD : $(git rev-parse HEAD)") + $ntfsinfo
            Set-Content -LiteralPath ([System.IO.Path]::ChangeExtension($image, '.ntfsinfo.txt')) -Value $record
            $head = (& git rev-parse HEAD).Trim()
            if ($LASTEXITCODE -ne 0 -or $head -notmatch '^[0-9a-f]{40}$') { throw 'cannot bind directory oracle to HEAD' }
            $directoryOracle['image sha256'] = $digest
            $directoryOracle['git HEAD'] = $head
            $directoryOracle | ConvertTo-Json -Depth 8 |
                Set-Content -LiteralPath ([IO.Path]::ChangeExtension($image, '.directory.json')) -Encoding utf8
        }
        if (-not (Test-Path $image) -or (Get-Item $image).Length -ne 256MB) {
            throw "fixture was not written at the expected size: $image"
        }
        $complete = $true
    } finally {
        try {
            Get-DiskImage -ImagePath $vhdPath -ErrorAction SilentlyContinue |
                Where-Object Attached |
                Dismount-DiskImage -ErrorAction Stop | Out-Null
        } catch { $complete = $false; throw } finally {
            try {
                if (Test-Path -LiteralPath $vhdPath) { Remove-Item -LiteralPath $vhdPath -Force -ErrorAction Stop }
            } finally {
                if (-not $complete) {
                    if (Test-Path -LiteralPath $image) { Remove-Item -LiteralPath $image -Force -ErrorAction Stop }
                    if ($Label -eq 'CLIREAD') {
                        foreach ($suffix in @('.directory.json', '.ntfsinfo.txt', '.sha256.tsv')) {
                            $partial = [IO.Path]::ChangeExtension($image, $suffix)
                            if (Test-Path -LiteralPath $partial) { Remove-Item -LiteralPath $partial -Force -ErrorAction Stop }
                        }
                    }
                }
            }
        }
    }
}
