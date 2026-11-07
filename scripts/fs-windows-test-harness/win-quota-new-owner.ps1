# scripts/fs-windows-test-harness/win-quota-new-owner.ps1 -- have Windows
# charge one file to an owner SID the volume's $Extend\$Quota:$O already
# maps and one to a SID it does not hold, so the raw reader can measure
# what Windows writes for a new owner (#1085, part of #849).
#
# On a volume with quota tracking on (win-quota-track first), Windows
# writes /known.bin, owned by the account the op runs as, which the
# tracking scan has already given an owner id, and /stranger.bin, whose
# owner icacls then sets to NETWORK SERVICE (S-1-5-20), a SID no file on a
# fresh volume is owned by. NTFS moves that file's charge to the new owner,
# which needs a new owner id, an $O entry and a $Q record. fsutil's account
# of the owners afterwards is kept in quota-new-owner.txt; the host's raw
# reader reads the bytes.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for quota-new-owner.txt (fsutil's own words).
#
# Exit code:
#   0 once both files are charged and fsutil names the new owner
#   1 otherwise

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"
. "$PSScriptRoot\_quota.ps1"

$NewOwner = 'S-1-5-20'

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $bytes = New-Object byte[] 8192
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [byte]($i % 251) }
    foreach ($name in 'known.bin', 'stranger.bin') {
        [System.IO.File]::WriteAllBytes("${letter}:\$name", $bytes)
    }
    # icacls enables the privilege that setting an owner other than the
    # caller needs.
    $out = & icacls.exe "${letter}:\stranger.bin" /setowner "*$NewOwner" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "icacls /setowner $NewOwner failed ($LASTEXITCODE): $out" }
    $owner = (Get-Acl -LiteralPath "${letter}:\stranger.bin").GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if ($owner -ne $NewOwner) { throw "/stranger.bin is owned by $owner after icacls, not $NewOwner" }

    $query = Get-QuotaQuery -Letter $letter
    $query | Out-File "$Diag\quota-new-owner.txt" -Encoding UTF8
    if ($query -notmatch 'NETWORK SERVICE|S-1-5-20') {
        throw "fsutil quota query names no owner $NewOwner after the owner change:`n$query"
    }
    Write-Host "win-quota-new-owner: /known.bin and /stranger.bin written, /stranger.bin owned by $NewOwner and charged to it"
    exit 0
} catch {
    [Console]::Error.WriteLine("win-quota-new-owner: $($_.Exception.Message)")
    exit 1
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
