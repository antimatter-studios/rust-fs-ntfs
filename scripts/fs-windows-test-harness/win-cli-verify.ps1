# win-cli-verify.ps1 -- does WINDOWS read back what fs.ntfs wrote?
#
# The oracle for `fs.ntfs write`, `mkdir` and `set label`: mounts the .img
# (re-using a prior op's .vhd if `keep_image=true` was set on the prior
# step) and checks the manifest scripts/cli-oracle.sh `populate` wrote
# beside it, line by line, through ntfs.sys:
#
#   file<TAB>/path<TAB>size<TAB>sha256   Get-FileHash -Algorithm SHA256, and the length
#   dir<TAB>/path                        a directory exists there
#   label<TAB>text                       Get-Volume's FileSystemLabel
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
            'dir' {
                $target = "${letter}:" + ($f[1] -replace '/', '\')
                if (Test-Path -LiteralPath $target -PathType Container) {
                    $report += "ok   dir  $($f[1])"
                } else {
                    $report += "FAIL dir  $($f[1]): not a directory on Windows"
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
