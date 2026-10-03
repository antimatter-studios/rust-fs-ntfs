# scripts/v2/win-chkdsk.ps1 -- minimal win-side helper for v2 recipes.
#
# A v2 alternative to scripts/run-scenario.ps1's chkdsk lifecycle,
# trimmed to just what's needed when the v2 dispatcher invokes it
# per-step over SSH. The full v1 driver still exists; this script is
# the per-op replacement chain that retires it.
#
# Wraps a host-side .img (already shipped to the VM via the harness's
# built-in `ship-to-vm` op) into a temporary VHD, mounts it on
# Windows, runs chkdsk against the resulting drive letter with the
# requested modes, dismounts, and cleans up.
#
# Args:
#   -ImagePath   Path on the VM to the .img file (typically
#                <vm.workdir>/<scenario.image>).
#   -Modes       Comma-separated list of chkdsk passes to run, in
#                order: readonly, /scan, /spotfix, /F, /F /scan.
#                Empty / absent => run readonly only (matches the
#                default `mac:format -> win:chkdsk` shape).
#   -VerdictShape  `Clean` (default), `RepairRequired` or `Damaged`. See the
#                  comment block above the chkdsk loop for the gating
#                  rules.
#   -KeepImage   If `true` (string, from `{step.keep_image?}`), the
#                .img and .vhd are left in place on the VM after this
#                op completes so a follow-on win-* op can mount them.
#                Default `false` matches the single-win-op recipe shape.
#                The final win-* op in a multi-op recipe must omit
#                this flag (or pass `false`) so cleanup runs.
#   -Diag        Directory to write diag artefacts into:
#                  chkdsk-<mode>.txt        - chkdsk's stdout (an
#                                             offline stand-in for a /scan
#                                             with no snapshot is
#                                             chkdsk--F--X-offline-fallback.txt)
#                  chkdsk-<mode>-exit.txt   - exit code marker
#                  mount-eventlog.txt       - Disk/Ntfs/partmgr events
#                  wrapper-create.txt       - rust-img-vhd output
#                  verdict.json             - final pass/fail summary
#
# Exit code:
#   0 if the verdict for -VerdictShape passed (see _chkdsk-verdict.ps1)
#   1 otherwise; per-mode exit codes are in <Diag>/chkdsk-*-exit.txt
#   2 for config errors (bad -VerdictShape, missing /scan in
#     RepairRequired modes, no modes at all for Damaged)
#
# Phase 1e (done): this script invokes `rust-img-vhd img <vhd> create --type fixed` from
# antimatter-studios/rust-img-vhd to wrap the .img into a VHD before
# mounting (replaced the prior qemu-img dep). The rest of the
# lifecycle (mount, initialize, dd, chkdsk) is unchanged.

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [string]$Modes = "readonly",
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$VerdictShape = 'Clean',
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"
. "$PSScriptRoot\_chkdsk-verdict.ps1"

# Accept empty (from `{step.verdict_shape?}` substitution when omitted)
# as the Clean default so callers don't have to spell it out everywhere.
if (-not $VerdictShape -or $VerdictShape.Trim() -eq '') {
    $VerdictShape = 'Clean'
}
if ($VerdictShape -notin @('Clean', 'RepairRequired', 'Damaged')) {
    Write-Error "invalid -VerdictShape: '$VerdictShape' (expected Clean, RepairRequired or Damaged)"
    exit 2
}

# Same trick for KeepImage: empty -> false. Accept any case-insensitive
# truthy string so recipes can write "True" without surprises.
$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null

$startTime = Get-Date
$state = $null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    # ── chkdsk passes ─────────────────────────────────────────────
    #
    # What the passes mean is _chkdsk-verdict.ps1's, tested without a VM by
    # tests/scripts/chkdsk-verdict.ps1. In short:
    #
    #   Clean (default):
    #     - readonly:  must exit 0
    #     - /scan:     0, 11 (frs.cxx 60f ceiling, known v1 technical
    #                  debt) and 13 pass
    #
    #   RepairRequired:
    #     - run the listed modes (a `set-dirty` scenario expects the
    #       pre-/F /scan to find something), then /F /X, then /scan again
    #     - verdict: pre-/F /scan found something AND /F /X exit 0 AND the
    #       scan after it is clean
    #
    #   Damaged (a volume whose structures were broken on purpose):
    #     - run the listed modes; at least one must find the damage.
    #       `/scan` is not required: the online scan does not compare
    #       $MFTMirr with $MFT, and exits 0 on a volume whose mirror was
    #       changed, while the read-only pass exits 3 on it
    #     - run /F /X; 0 or 1 both pass, 1 being chkdsk's "errors found
    #       and fixed", the answer a damaged volume is expected to get
    #     - run /scan again; it must be clean
    #
    # In every shape, a /scan that could not take its volume snapshot
    # scanned nothing: it is recorded `not-scanned`, never read as a
    # finding, and where a clean scan was required the offline `/F /X`
    # stands in for it (#419, #295).
    #
    # `/F /X`: `/X` forces an exclusive dismount before the fix so chkdsk
    # doesn't hang on a "do you want to dismount?" prompt that we can't
    # answer non-interactively. The scan after it lands in
    # `chkdsk--scan-post.txt` so the pre/post logs are distinct.
    $invokeMode = {
        param([string]$mode, [string]$labelSuffix)
        $modeFile = ($mode -replace '[/\\ ]', '-') + $labelSuffix
        $log = "$Diag\chkdsk-$modeFile.txt"
        $exitFile = "$Diag\chkdsk-$modeFile-exit.txt"
        $argsList = @("${letter}:")
        if ($mode -ne "readonly") {
            $argsList += $mode -split ' '
        }
        $proc = Start-Process -FilePath chkdsk -ArgumentList $argsList -NoNewWindow -PassThru -Wait -RedirectStandardOutput $log
        "$($proc.ExitCode)" | Out-File $exitFile -Encoding ASCII
        $report = ''
        if (Test-Path -LiteralPath $log) { $report = Get-Content -LiteralPath $log -Raw }
        return @{ Exit = $proc.ExitCode; Report = $report }
    }.GetNewClosure()

    $modeList = @($Modes.Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    # Config errors exit 2, before any chkdsk runs. They bypass
    # `Write-Error` because `$ErrorActionPreference = 'Stop'` would turn it
    # into a terminating error, and the outer try/finally would exit 1 --
    # masking the intentional "config error".
    if ($VerdictShape -eq 'Damaged' -and $modeList.Count -eq 0) {
        [Console]::Error.WriteLine("Damaged needs at least one mode in -Modes to find the damage; got -Modes '$Modes'")
        exit 2
    }
    if ($VerdictShape -eq 'RepairRequired' -and $modeList -notcontains '/scan') {
        [Console]::Error.WriteLine("RepairRequired requires '/scan' in -Modes; got -Modes '$Modes'")
        exit 2
    }

    $verdict = Invoke-ChkdskVerdict -Modes $modeList -VerdictShape $VerdictShape -InvokeMode $invokeMode
    $passed = $verdict.passed
    $verdict | ConvertTo-Json -Depth 5 -Compress | Out-File "$Diag\verdict.json" -Encoding ASCII

    # NTFS / Disk / partmgr events fired during this run.
    try {
        Get-WinEvent -LogName 'System' -EA SilentlyContinue |
            Where-Object {
                $_.TimeCreated -ge $startTime -and
                $_.ProviderName -in 'Ntfs','Microsoft-Windows-Ntfs','Disk','Volsnap','partmgr'
            } |
            Select-Object TimeCreated, ProviderName, Id, LevelDisplayName, Message |
            Format-List | Out-File "$Diag\mount-eventlog.txt"
    } catch { }

    if ($passed) { exit 0 } else { exit 1 }

} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
