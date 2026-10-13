# scripts/fs-windows-test-harness/win-write-objids.ps1 -- write files to a
# mounted NTFS volume and have Windows give each one an object id, so that
# Windows itself grows $Extend\$ObjId:$O past its root into INDX blocks.
#
# Each file is `<Dir>\f<N>.txt` (N zero-padded to three digits, from 000),
# holding its own name, and `fsutil objectid create` gives it an id. A
# 1 KiB record's $INDEX_ROOT holds a handful of $O entries, so a few dozen
# spill the index into blocks: the shape a host-side unlink of one of
# those files has to take its entry out of (#571).
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Dir        Directory inside the volume, e.g. "/objid". Created.
#   -Count      Positive integer: how many files.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for write-objids-result.txt + write-objids-error.txt.
#
# Exit code:
#   0 on success
#   1 on a write or fsutil failure (logged to write-objids-error.txt)
#   2 for arg errors

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Dir,
    [Parameter(Mandatory=$true)] [string]$Count,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

$countInt = 0
if (-not [int]::TryParse($Count, [ref]$countInt) -or $countInt -lt 1 -or $countInt -gt 999) {
    [Console]::Error.WriteLine("invalid -Count: '$Count' (expected 1 to 999)")
    exit 2
}

$relDir = $Dir -replace '^[/\\]+', ''
if ($relDir -eq '') {
    [Console]::Error.WriteLine("win-write-objids: -Dir must name a directory under the volume root; got '$Dir'")
    exit 2
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $target = "${letter}:\$relDir"
    try {
        [System.IO.Directory]::CreateDirectory($target) | Out-Null
        for ($i = 0; $i -lt $countInt; $i++) {
            $file = Join-Path $target ('f{0:D3}.txt' -f $i)
            [System.IO.File]::WriteAllBytes($file, [System.Text.Encoding]::UTF8.GetBytes(('f{0:D3}' -f $i)))
            $o = & fsutil.exe objectid create $file 2>&1
            if ($LASTEXITCODE -ne 0) { throw "fsutil objectid create $file failed: $o" }
        }
        "wrote $countInt files with object ids under $target" |
            Out-File "$Diag\write-objids-result.txt" -Encoding UTF8
    } catch {
        $_.Exception.Message | Out-File "$Diag\write-objids-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
