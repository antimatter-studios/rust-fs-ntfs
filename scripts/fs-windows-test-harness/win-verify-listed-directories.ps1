# Windows grades every name left by the #818 cycle, including exact case.
param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\_lib.ps1"
New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath
try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
    $report = @()
    foreach ($directory in @('grown', 'folded')) {
        $want = @('a', 'b')
        if ($directory -eq 'grown') {
            for ($i = 0; $i -lt 80; $i++) { $want += ('listed_{0:D4}.txt' -f $i) }
        }
        $entries = @(Get-ChildItem -LiteralPath "${letter}:\$directory" -Force)
        $got = @($entries | ForEach-Object { $_.Name } | Sort-Object)
        $want = @($want | Sort-Object)
        if (($got -join '|') -cne ($want -join '|')) {
            throw "${directory}: Windows lists [$($got -join ', ')], expected [$($want -join ', ')]"
        }
        foreach ($entry in $entries) {
            if ($entry.PSIsContainer -or $entry.Length -ne 0) {
                throw "$($entry.FullName): expected an empty regular file"
            }
        }
        $report += "${directory}: Windows enumerates exactly $($got.Count) expected names"
    }
    $report | Out-File "$Diag\verify-listed-directories.txt" -Encoding UTF8
    exit 0
} catch {
    $_.Exception.Message | Out-File "$Diag\verify-listed-directories-error.txt" -Encoding UTF8
    throw
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage ($KeepImage -match '^(?i:true|1|yes)$')
}
