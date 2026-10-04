# tests/scripts/capture-chkdsk.ps1 -- what the logfile capture makes of
# chkdsk's answer to `chkdsk /X /L:<KB>`, with no Windows.
#
#   pwsh -NoProfile -File tests/scripts/capture-chkdsk.ps1

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../test-disks/_capture-chkdsk.ps1"

$script:fails = 0
function Ok($what) { Write-Host "ok   $what" }
function Fail($what) { Write-Host "FAIL $what"; $script:fails++ }
function Accepts($code, $out, $what) {
    try { Assert-LogResize $code $out 2048; Ok $what } catch { Fail "$what (threw: $_)" }
}
function Refuses($code, $out, $what) {
    try { Assert-LogResize $code $out 2048; Fail "$what (returned)" } catch { Ok $what }
}

# chkdsk /X /L:2048 on a freshly formatted 127 MiB volume, exit 1: lines
# taken verbatim from its report in logfile-oracle run 37173621544, which
# the capture refused. Resizing the log is a correction to chkdsk, so it
# exits 1.
$Resized = @(
    'The type of the file system is NTFS.',
    'Volume label is ORACLE366.',
    '256 file records processed.',
    '11 data files processed.',
    'CHKDSK is adjusting the size of the log file.',
    'Windows has made corrections to the file system.',
    'No further action is required.',
    '2048 KB occupied by the log file.'
)

# Constructed, not captured: a pass with nothing to correct, exit 0.
$Unchanged = @(
    'The type of the file system is NTFS.',
    'Windows has scanned the file system and found no problems.',
    'No further action is required.'
)

# Constructed, not captured: exit 1 for a correction that is not the
# resize. The volume itself was repaired, which a capture must never run on.
$Repaired = @(
    'The type of the file system is NTFS.',
    'Windows has made corrections to the file system.',
    'No further action is required.'
)

Accepts 0 $Unchanged 'exit 0 is accepted'
Accepts 1 $Resized 'exit 1 after chkdsk adjusted the log size is accepted'
Refuses 1 $Repaired 'exit 1 for any other correction is refused'
Refuses 3 $Resized 'exit 3 is refused whatever the report says'

# chkdsk /L after resizing to 2048 KB, exit 0: the lines verbatim from
# logsize.txt of logfile-oracle run 37179118078.
$Size2048 = @(
    'The type of the file system is NTFS.',
    'The current log file size is 2048 KB.',
    'The default log file size for this volume is 2048 KB.'
)
# Constructed, not captured: a log of 12048 KB, whose report contains the
# text "2048 KB".
$Size12048 = @(
    'The type of the file system is NTFS.',
    'The current log file size is 12048 KB.'
)
function SizeAccepts($code, $out, $what) {
    try { Assert-LogSize $code $out 2048; Ok $what } catch { Fail "$what (threw: $_)" }
}
function SizeRefuses($code, $out, $what) {
    try { Assert-LogSize $code $out 2048; Fail "$what (returned)" } catch { Ok $what }
}
SizeAccepts 0 $Size2048 'a readback of 2048 KB is accepted for 2048'
SizeRefuses 0 $Size12048 'a readback of 12048 KB is refused for 2048'
SizeRefuses 3 $Size2048 'a readback chkdsk failed is refused whatever it printed'

if ($script:fails -gt 0) { Write-Host "capture-chkdsk: $script:fails failed"; exit 1 }
Write-Host 'capture-chkdsk: all checks passed'
