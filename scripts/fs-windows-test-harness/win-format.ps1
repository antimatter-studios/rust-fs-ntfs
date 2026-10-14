# scripts/v2/win-format.ps1 -- format a freshly-shipped empty .img with
# Microsoft's canonical NTFS formatter (Format-Volume -FileSystem NTFS).
#
# Recipe shape this op fits into:
#
#   init-image                                                # host-side empty .img
#    -> ship-to-vm   (src={scenario.image}, dest=<vm.img>)    # ship empty .img to VM
#    -> win-format   (image_path=<vm.img>, label=..., keep)   # this op
#    -> ship-to-host (src=<vm.img>, dest={scenario.image})    # pull formatted .img back
#    -> mac-enumerate                                         # rust-ntfs ls of the now-NTFS .img
#
# Why a separate op rather than reusing rust-ntfs format on the host:
# the win-format-* scenarios exist precisely to feed our reader a volume
# that was produced by the canonical Microsoft formatter (Format-Volume
# under the hood calls fmifs.dll's NTFS path -- the same code that
# format.com /FS:NTFS uses). If our reader copes with that, it copes
# with anything Windows itself writes; conversely a regression here
# isolates a reader bug from a writer bug.
#
# Mount lifecycle:
#   1. Initialize-VhdFromImg wraps the .img into a VHD, GPT-inits,
#      partitions, streams the (zero) bytes into the partition. This
#      works on a fresh zero .img -- it just lays down a partition table
#      on the wrapper and writes zeros into the partition data area.
#   2. Mount-VhdAndGetLetter remounts so a drive letter is assigned.
#      The volume is RAW at this point (no NTFS structure yet); the
#      lib helper's Set-Partition fallback covers the case where
#      Windows declines to auto-assign a letter to a RAW volume.
#   3. Format-Volume -DriveLetter <letter> -FileSystem NTFS does the
#      actual formatting in-place on the mounted drive letter.
#   4. Dismount-VhdAndCleanup tears the wrapper down. KeepImage=true
#      leaves the .img + .vhd on the VM so a follow-on win-* op
#      (e.g. win-write-content) or ship-to-host can read it.
#
# Args:
#   -ImagePath  Path on the VM to the (zero-filled) .img file.
#   -Label      Volume label for the formatted NTFS volume.
#   -AllocationUnitSize  Cluster size in bytes. Omitted, Format-Volume picks
#               its default (4 KiB on a 256 MiB volume); the
#               win-format-alloc op passes the scenario's alloc_unit_size.
#   -KeepImage  'true' to leave .img + .vhd for a follow-on op (default 'false').
#   -Diag       Directory for format-result.txt + wrapper-create.txt.
#
# Exit code:
#   0 on success
#   1 on Format-Volume failure (logged to format-error.txt)
#   2 for arg errors

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Label,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$AllocationUnitSize = '',
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

# Same boolean-from-string idiom as siblings (win-chkdsk, win-write,
# win-rename, win-mkdir, win-delete, win-repeat-mount): empty -> false,
# accept any case-insensitive truthy spelling so recipes can write
# "True" / "true" / "1" / "yes" without surprises.
$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

if ($Label.Trim() -eq '') {
    [Console]::Error.WriteLine("win-format: -Label must be non-empty")
    exit 2
}

$sizeArgs = @{}
if ($AllocationUnitSize.Trim() -ne '') {
    $unit = 0
    if (-not [int]::TryParse($AllocationUnitSize, [ref]$unit) -or $unit -lt 512) {
        [Console]::Error.WriteLine("win-format: -AllocationUnitSize must be a cluster size in bytes; got '$AllocationUnitSize'")
        exit 2
    }
    $sizeArgs['AllocationUnitSize'] = $unit
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null

$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    try {
        # -Force + -Confirm:$false matches the non-interactive mode the
        # rest of the v2 ops use; without these, Format-Volume would
        # prompt to confirm wiping the (RAW) volume and hang us.
        # AllocationUnitSize is left at the FS default (4 KiB on a 256
        # MiB volume) unless -AllocationUnitSize names one: the
        # win-format op does not pass it, win-format-alloc passes the
        # scenario's alloc_unit_size, for scenarios that measure how
        # Windows lays a file out at a given cluster size (#512).
        $result = Format-Volume -DriveLetter $letter `
            -FileSystem NTFS `
            -NewFileSystemLabel $Label `
            @sizeArgs `
            -Force `
            -Confirm:$false
        $result | Format-List | Out-File "$Diag\format-result.txt" -Encoding UTF8
    } catch {
        $_.Exception.Message | Out-File "$Diag\format-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
