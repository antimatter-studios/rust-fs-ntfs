# win-ntfsinfo.ps1 -- does WINDOWS read the geometry the format plan promised?
#
# The oracle for `mkfs.ntfs --dry-run`'s plan: mounts the .img (re-using a
# prior op's .vhd if `keep_image=true` was set on the prior step) and
# compares, field by field, what ntfs.sys reports for the mounted volume --
# `fsutil fsinfo ntfsinfo` and Get-Volume's label -- with the plan
# cli-oracle.sh's mkfs-plan step wrote beside the image. Nothing here reads
# the volume through this crate.
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Plan       The plan: `key<TAB>value` lines (IMAGE.plan).
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for ntfsinfo.txt (fsutil's own words).
#
# Exit code:
#   0 when every field Windows reports equals the plan's
#   1 when one differs, the plan lacks a field, or fsutil omits one

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Plan,
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

function Fail([string]$Message) {
    [Console]::Error.WriteLine("win-ntfsinfo: $Message")
    exit 1
}

# A number as fsutil prints it: decimal, perhaps with thousands separators,
# or 0x-prefixed hex. The first token of the value only: newer releases
# follow some numbers with a size in parentheses.
function ConvertFrom-FsutilNumber([string]$Text) {
    $token = ($Text.Trim() -split '\s+')[0] -replace ',', ''
    if ($token -match '^0[xX]([0-9a-fA-F]+)$') { return [Convert]::ToUInt64($Matches[1], 16) }
    if ($token -match '^[0-9]+$') { return [UInt64]::Parse($token) }
    throw "not a number: '$Text'"
}

if (-not (Test-Path -LiteralPath $Plan)) { Fail "no plan at $Plan" }
$planned = @{}
foreach ($line in Get-Content -LiteralPath $Plan) {
    $parts = $line -split "`t", 2
    if ($parts.Count -eq 2) { $planned[$parts[0]] = $parts[1] }
}
foreach ($key in 'bytes_per_sector', 'cluster_size', 'mft_record_size', 'total_sectors',
                 'total_clusters', 'mft_lcn', 'mftmirr_lcn', 'serial', 'label') {
    if (-not $planned.ContainsKey($key)) { Fail "the plan has no $key ($Plan)" }
}

# fsutil's name for each field, and the plan's.
$fields = [ordered]@{
    'Bytes Per Sector'             = 'bytes_per_sector'
    'Bytes Per Cluster'            = 'cluster_size'
    'Bytes Per FileRecord Segment' = 'mft_record_size'
    'Total Sectors'                = 'total_sectors'
    'Total Clusters'               = 'total_clusters'
    'Mft Start Lcn'                = 'mft_lcn'
    'Mft2 Start Lcn'               = 'mftmirr_lcn'
    'NTFS Volume Serial Number'    = 'serial'
}

$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $info = @(& fsutil fsinfo ntfsinfo "${letter}:" 2>&1)
    $fsutilExit = $LASTEXITCODE
    $info | Out-File "$Diag\ntfsinfo.txt" -Encoding UTF8
    if ($fsutilExit -ne 0) { Fail "fsutil fsinfo ntfsinfo ${letter}: exited ${fsutilExit}: $($info -join ' ')" }

    $reported = @{}
    foreach ($line in $info) {
        if ("$line" -match '^\s*([^:]+?)\s*:\s*(.+)$') {
            if (-not $reported.ContainsKey($Matches[1])) { $reported[$Matches[1]] = $Matches[2] }
        }
    }
    # Older releases call Total Sectors "Number Sectors".
    if (-not $reported.ContainsKey('Total Sectors') -and $reported.ContainsKey('Number Sectors')) {
        $reported['Total Sectors'] = $reported['Number Sectors']
    }

    $wrong = @()
    foreach ($name in $fields.Keys) {
        $key = $fields[$name]
        if (-not $reported.ContainsKey($name)) { Fail "fsutil reported no '$name' line (see $Diag\ntfsinfo.txt)" }
        $windows = ConvertFrom-FsutilNumber $reported[$name]
        # The serial is 16 hex digits in the plan, as --serial takes it.
        $expected = if ($key -eq 'serial') { [Convert]::ToUInt64($planned[$key], 16) } else { [UInt64]::Parse($planned[$key]) }
        if ($windows -ne $expected) {
            $wrong += "${name}: Windows reports $windows, the plan says $expected ($key)"
        } else {
            Write-Host "win-ntfsinfo: ${name} = $windows, as planned"
        }
    }

    $label = (Get-Volume -DriveLetter $letter).FileSystemLabel
    if ($label -cne $planned['label']) {
        $wrong += "label: Windows reports '$label', the plan says '$($planned['label'])'"
    } else {
        Write-Host "win-ntfsinfo: label = '$label', as planned"
    }

    if ($wrong.Count -gt 0) { Fail ($wrong -join '; ') }
    Write-Host "win-ntfsinfo: Windows reads the volume the plan described"
    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
