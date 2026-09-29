# win-dirty-query.ps1 -- does WINDOWS say the volume is dirty?
#
# The oracle for `fs.ntfs set dirty` and for fsck.ntfs's `dirty` finding:
# mounts the .img (re-using a prior op's .vhd if `keep_image=true` was set
# on the prior step) and asks `fsutil dirty query`, which reads the flag
# through ntfs.sys rather than through our reading of $Volume.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Expect     `Dirty` or `Clean`: what Windows must answer.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for dirty-query.txt (fsutil's own words).
#
# Exit code:
#   0 when Windows' answer is -Expect
#   1 when it is not, or fsutil gave neither answer
#   2 for a bad -Expect

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Expect,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

if ($Expect -ne 'Dirty' -and $Expect -ne 'Clean') {
    [Console]::Error.WriteLine("win-dirty-query: -Expect must be Dirty or Clean; got '$Expect'")
    exit 2
}

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null

$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    # "Volume - X: is Dirty" / "Volume - X: is NOT Dirty".
    $answer = (& fsutil dirty query "${letter}:" 2>&1 | Out-String).Trim()
    $answer | Out-File "$Diag\dirty-query.txt" -Encoding UTF8
    $got = if ($answer -match 'is NOT Dirty') { 'Clean' }
           elseif ($answer -match 'is Dirty') { 'Dirty' }
           else { $null }
    if ($null -eq $got) {
        [Console]::Error.WriteLine("win-dirty-query: fsutil answered neither: $answer")
        exit 1
    }
    if ($got -ne $Expect) {
        [Console]::Error.WriteLine("win-dirty-query: Windows says the volume is $got, expected $Expect ($answer)")
        exit 1
    }
    Write-Host "win-dirty-query: Windows says $got, as expected"
    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
