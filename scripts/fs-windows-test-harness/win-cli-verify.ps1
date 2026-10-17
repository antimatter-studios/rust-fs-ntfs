# win-cli-verify.ps1 -- does WINDOWS read back what fs.ntfs wrote?
#
# The oracle for `fs.ntfs write`, `mkdir`, `mv`, `ln` and `set label`: mounts the .img
# (re-using a prior op's .vhd if `keep_image=true` was set on the prior
# step) and checks the manifest a scripts/cli-oracle.sh step wrote
# beside it, line by line, through ntfs.sys:
#
#   file<TAB>/path<TAB>size<TAB>sha256   Get-FileHash -Algorithm SHA256, and the length
#   truncate<TAB>/path<TAB>old<TAB>size<TAB>intermediate
#                                        Windows creates old bytes of Z, repeats the
#                                        SetLength sequence and compares every byte
#                                        by SHA-256, including every grown zero tail
#   dir<TAB>/path                        a directory exists there
#   label<TAB>text                       Get-Volume's FileSystemLabel
#   name<TAB>/path                       the parent lists exactly one entry Windows
#                                        takes for the last name, and it is spelled
#                                        exactly so, case included (fs.ntfs mv)
#   absent<TAB>/path                     nothing is there (fs.ntfs mv's old name)
#   links<TAB>/path<TAB>/path...         `fsutil hardlink list` on the first lists
#                                        exactly these names, case included (fs.ntfs ln)
#   symlink<TAB>/path<TAB>text<TAB>size<TAB>sha256
#                                        a symbolic link whose target Windows reads
#                                        as text, `/` given as `\`, and through which
#                                        it reads size bytes of that SHA-256 (fs.ntfs ln -s)
#   refuse<TAB>/path                     Windows refuses to make the directory chain
#                                        /path, as fs.ntfs mkdir -p did: CreateDirectory
#                                        fails because something is in the way
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Manifest   Path on the VM to the manifest (UTF-8, tab-separated).
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for cli-verify.txt (one line per check).
#
# Exit code:
#   0 when every line of the manifest holds on Windows
#   1 when any does not, or the manifest is empty

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Manifest,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null

$lines = @(Get-Content -LiteralPath $Manifest -Encoding UTF8 | Where-Object { $_ -ne '' })
if ($lines.Count -eq 0) {
    [Console]::Error.WriteLine("win-cli-verify: $Manifest is empty")
    exit 1
}

$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $report = @()
    $failures = 0
    foreach ($line in $lines) {
        $f = $line -split "`t"
        switch ($f[0]) {
            'file' {
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                if (-not (Test-Path -LiteralPath $target -PathType Leaf)) {
                    $report += "FAIL file $($f[1]): not found"
                    $failures++
                    continue
                }
                $length = (Get-Item -LiteralPath $target -Force).Length
                $hash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
                if ($length -ne [int64]$f[2] -or $hash -ne $f[3]) {
                    $report += "FAIL file $($f[1]): $length bytes, sha256 $hash; fs.ntfs wrote $($f[2]) bytes, sha256 $($f[3])"
                    $failures++
                } else {
                    $report += "ok   file $($f[1]) ($length bytes)"
                }
            }
            'write-at' {
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                $reference = Join-Path $Diag 'write-at-windows-reference.bin'
                $bytes = New-Object byte[] ([int]$f[2])
                for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = 90 }
                [System.IO.File]::WriteAllBytes($reference, $bytes)
                $patch = New-Object byte[] ([int]$f[4])
                for ($i = 0; $i -lt $patch.Length; $i++) { $patch[$i] = 81 }
                $stream = [System.IO.File]::OpenWrite($reference)
                try {
                    $stream.Position = [int64]$f[3]
                    $stream.Write($patch, 0, $patch.Length)
                } finally { $stream.Dispose() }
                $want = (Get-FileHash -LiteralPath $reference -Algorithm SHA256).Hash
                $got = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
                $length = (Get-Item -LiteralPath $target -Force).Length
                $size = (Get-Item -LiteralPath $reference).Length
                if ($length -ne $size -or $got -ne $want) {
                    $report += "FAIL write-at $($f[1]): $length bytes, SHA256 $got; Windows gives $size bytes, SHA256 $want"
                    $failures++
                } else {
                    $report += "ok   write-at $($f[1]): length and content match Windows positioned Write"
                }
            }
            'truncate' {
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                $reference = Join-Path $Diag 'truncate-windows-reference.bin'
                $bytes = New-Object byte[] ([int]$f[2])
                for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = 90 }
                [System.IO.File]::WriteAllBytes($reference, $bytes)
                $stream = [System.IO.File]::OpenWrite($reference)
                try {
                    if ($f.Count -gt 4 -and $f[4] -ne '') { $stream.SetLength([int64]$f[4]) }
                    $stream.SetLength([int64]$f[3])
                } finally { $stream.Dispose() }
                $want = (Get-FileHash -LiteralPath $reference -Algorithm SHA256).Hash
                $got = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
                $length = (Get-Item -LiteralPath $target -Force).Length
                if ($length -ne [int64]$f[3] -or $got -ne $want) {
                    $report += "FAIL truncate $($f[1]): $length bytes, SHA256 $got; Windows SetLength gives $($f[3]) bytes, SHA256 $want"
                    $failures++
                } else {
                    $report += "ok   truncate $($f[1]): length and content match Windows SetLength"
                }
            }
            'dir' {
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                if (Test-Path -LiteralPath $target -PathType Container) {
                    $report += "ok   dir  $($f[1])"
                } else {
                    $report += "FAIL dir  $($f[1]): not a directory on Windows"
                    $failures++
                }
            }
            'name' {
                $slash = $f[1].LastIndexOf('/')
                $leaf = $f[1].Substring($slash + 1)
                $parent = "${letter}:" + ($f[1].Substring(0, $slash) -replace '/', '\') + '\'
                $found = @(Get-ChildItem -LiteralPath $parent -Force | Where-Object { $_.Name -ieq $leaf } | ForEach-Object { $_.Name })
                if ($found.Count -eq 1 -and $found[0] -ceq $leaf) {
                    $report += "ok   name $($f[1])"
                } else {
                    $report += "FAIL name $($f[1]): Windows lists '$($found -join "', '")'"
                    $failures++
                }
            }
            'absent' {
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                if (Test-Path -LiteralPath $target) {
                    $report += "FAIL absent $($f[1]): still there on Windows"
                    $failures++
                } else {
                    $report += "ok   absent $($f[1])"
                }
            }
            'links' {
                # fsutil prints each name from the volume's root, without
                # the drive letter, one per line.
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                $want = @($f[1..($f.Count - 1)] | ForEach-Object { $_ -replace '/', '\' } | Sort-Object)
                $got = @(& fsutil hardlink list $target | Where-Object { $_.Trim() -ne '' } | ForEach-Object { $_.Trim() } | Sort-Object)
                if ($LASTEXITCODE -eq 0 -and ($got -join '|') -ceq ($want -join '|')) {
                    $report += "ok   links $($f[1]) ($($got.Count) names)"
                } else {
                    $report += "FAIL links $($f[1]): fsutil (exit $LASTEXITCODE) lists '$($got -join "', '")'; fs.ntfs made '$($want -join "', '")'"
                    $failures++
                }
            }
            'symlink' {
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                $want = $f[2] -replace '/', '\'
                $item = Get-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue
                $linkTarget = if ($item) { "$($item.Target)" } else { '' }
                if (-not $item -or $item.LinkType -ne 'SymbolicLink' -or $linkTarget -cne $want) {
                    $report += "FAIL symlink $($f[1]): Windows reads a '$($item.LinkType)' to '$linkTarget'; fs.ntfs made a symbolic link to '$want'"
                    $failures++
                    continue
                }
                # Opening the link follows it: these are the target's bytes.
                try {
                    $stream = [System.IO.File]::OpenRead($target)
                    try { $length = $stream.Length } finally { $stream.Dispose() }
                    $hash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
                } catch {
                    $report += "FAIL symlink $($f[1]) -> ${want}: Windows cannot follow it: $($_.Exception.Message)"
                    $failures++
                    continue
                }
                if ($length -ne [int64]$f[3] -or $hash -ne $f[4]) {
                    $report += "FAIL symlink $($f[1]) -> ${want}: $length bytes, sha256 $hash; its target holds $($f[3]) bytes, sha256 $($f[4])"
                    $failures++
                } else {
                    $report += "ok   symlink $($f[1]) -> $want ($length bytes)"
                }
            }
            'refuse' {
                # Directory.CreateDirectory makes every missing directory on
                # the way, as mkdir -p does. A file on the way or at the path
                # fails it with ERROR_ALREADY_EXISTS (183); ERROR_PATH_NOT_FOUND
                # (3) and ERROR_DIRECTORY (267) say the same of a path below a
                # file. Any other failure is not this refusal, so it fails.
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                $code = $null
                $why = ''
                try {
                    [System.IO.Directory]::CreateDirectory($target) | Out-Null
                } catch {
                    $e = $_.Exception
                    while ($e -is [System.Management.Automation.MethodInvocationException] -and $e.InnerException) { $e = $e.InnerException }
                    $code = $e.HResult -band 0xFFFF
                    $why = "$($e.GetType().Name): $($e.Message)"
                }
                if ($null -eq $code) {
                    $report += "FAIL refuse $($f[1]): Windows made the chain fs.ntfs refused"
                    $failures++
                } elseif ($e -is [System.IO.IOException] -and (@(183, 3, 267) -contains $code)) {
                    $report += "ok   refuse $($f[1]) (Win32 error $code)"
                } else {
                    $report += "FAIL refuse $($f[1]): Windows failed for another reason, Win32 error ${code}: $why"
                    $failures++
                }
            }
            'label' {
                $label = (Get-Volume -DriveLetter $letter).FileSystemLabel
                if ($label -ceq $f[1]) {
                    $report += "ok   label '$label'"
                } else {
                    $report += "FAIL label: Windows reads '$label', fs.ntfs set '$($f[1])'"
                    $failures++
                }
            }
            default {
                $report += "FAIL manifest line not understood: $line"
                $failures++
            }
        }
    }
    ($report -join "`r`n") | Out-File "$Diag\cli-verify.txt" -Encoding UTF8
    $report | Where-Object { $_ -like 'FAIL*' } | ForEach-Object { [Console]::Error.WriteLine("win-cli-verify: $_") }
    if ($failures -gt 0) { exit 1 }
    Write-Host "win-cli-verify: $($lines.Count) manifest lines hold on Windows"
    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
