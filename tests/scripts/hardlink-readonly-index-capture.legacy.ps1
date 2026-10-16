# Captures how Windows refreshes the FILE_NAME copies of a hard-linked file
# when its read-only attribute changes. One Windows-formatted image, a file
# with two hard links in two known directories plus an unchanged control,
# frozen as a raw image after each stage:
#   s0-created   both links and the control exist
#   s1-readonly  SetFileAttributes(READONLY) through the first link
#   s2-reopen    the second link opened and closed again
#   s3-rename    the second link renamed within its directory
# Nothing here decides what the records should hold: the manifest carries
# only what Windows APIs reported and the SHA-256 of each frozen image.
param(
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$img = Join-Path (Resolve-Path $OutDir) 'hardlink-oracle.vhd'
$letter = 'R'
$manifest = Join-Path (Resolve-Path $OutDir) 'manifest.tsv'
$lines = New-Object System.Collections.Generic.List[string]
$lines.Add("head`t$env:GITHUB_SHA")

function Invoke-Diskpart([string[]]$script) {
    $file = Join-Path $env:RUNNER_TEMP ("diskpart-" + [guid]::NewGuid() + '.txt')
    Set-Content -Path $file -Value $script -Encoding ascii
    $out = diskpart /s $file
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed ($LASTEXITCODE): $out" }
}

function Record-Path([string]$stage, [string]$label, [string]$path) {
    $id = (fsutil file queryfileid $path) -join ' '
    if ($LASTEXITCODE -ne 0) { throw "fsutil file queryfileid failed for $path" }
    $attrs = [int][System.IO.File]::GetAttributes($path)
    $lines.Add("api`t$stage`t$label`t$path`t$attrs`t$id")
}

function Freeze([string]$stage) {
    $copy = Join-Path (Resolve-Path $OutDir) "$stage.img"
    Invoke-Diskpart @("select vdisk file=`"$img`"", 'detach vdisk')
    Copy-Item -LiteralPath $img -Destination $copy
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $copy).Hash.ToLowerInvariant()
    $lines.Add("image`t$stage`t$stage.img`t$hash")
    Invoke-Diskpart @("select vdisk file=`"$img`"", 'attach vdisk')
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path "${letter}:\")) {
        if ((Get-Date) -gt $deadline) { throw "volume $letter did not return after $stage" }
        Start-Sleep -Milliseconds 200
    }
}

try {
Invoke-Diskpart @(
    "create vdisk file=`"$img`" maximum=64 type=fixed",
    "select vdisk file=`"$img`"",
    'attach vdisk',
    'convert mbr',
    'create partition primary',
    'format fs=ntfs quick label=HLORACLE',
    "assign letter=$letter"
)
$lines.Add("offset`t$((Get-Partition -DriveLetter $letter).Offset)")

$first = "${letter}:\alpha\linked.txt"
$second = "${letter}:\beta\second.txt"
$renamed = "${letter}:\beta\renamed.txt"
$control = "${letter}:\alpha\control.txt"
New-Item -ItemType Directory -Path "${letter}:\alpha", "${letter}:\beta" | Out-Null
Set-Content -Path $first -Value 'hardlink oracle' -Encoding ascii
Set-Content -Path $control -Value 'control' -Encoding ascii
fsutil hardlink create $second $first | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'fsutil hardlink create failed' }

$lines.Add("op`t1`ts0-created`tcreate alpha\linked.txt, alpha\control.txt; fsutil hardlink create beta\second.txt")
foreach ($p in @($first, $second, $control)) { Record-Path 's0-created' 'before' $p }
Freeze 's0-created'

$attrs = [System.IO.File]::GetAttributes($first) -bor [System.IO.FileAttributes]::ReadOnly
[System.IO.File]::SetAttributes($first, $attrs)
$lines.Add("op`t2`ts1-readonly`tSetFileAttributes(alpha\linked.txt, existing|READONLY)")
foreach ($p in @($first, $second, $control)) { Record-Path 's1-readonly' 'after' $p }
Freeze 's1-readonly'

$stream = [System.IO.File]::Open($second, 'Open', 'Read', 'ReadWrite')
$stream.Close()
$lines.Add("op`t3`ts2-reopen`topen+close beta\second.txt for read")
foreach ($p in @($first, $second, $control)) { Record-Path 's2-reopen' 'after' $p }
Freeze 's2-reopen'

Rename-Item -LiteralPath $second -NewName 'renamed.txt'
$lines.Add("op`t4`ts3-rename`trename beta\second.txt to beta\renamed.txt")
foreach ($p in @($first, $renamed, $control)) { Record-Path 's3-rename' 'after' $p }
Freeze 's3-rename'

Invoke-Diskpart @("select vdisk file=`"$img`"", 'detach vdisk')
Set-Content -Path $manifest -Value $lines -Encoding ascii
} finally {
    if (Test-Path -LiteralPath $img) {
        try { Invoke-Diskpart @("select vdisk file=`"$img`"", 'detach vdisk') } catch { }
        Remove-Item -LiteralPath $img -Force
    }
}
