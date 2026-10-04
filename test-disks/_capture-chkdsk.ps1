# test-disks/_capture-chkdsk.ps1 -- what capture-interrupted-logfile.ps1
# makes of chkdsk's answer when it sets the $LogFile size. Dot-sourced by the
# capture and by tests/scripts/capture-chkdsk.ps1, which drives it with
# chkdsk's own reports and no Windows.

# Assert-LogResize EXITCODE OUTPUT KB: returns when `chkdsk /X /L:KB`
# succeeded, throws naming the status and the report when it did not.
#
# Resizing the log is a correction to chkdsk, which reports it with exit
# status 1 and "CHKDSK is adjusting the size of the log file" (run
# 37173621544). Status 1 without that line is some other correction, and
# any other non-zero status a failure.
function Assert-LogResize([int]$ExitCode, [string[]]$Output, [int]$KB) {
    if ($ExitCode -eq 0) { return }
    $resized = ($Output -join ' ') -match 'adjusting the size of the log file'
    if ($ExitCode -eq 1 -and $resized) { return }
    throw "chkdsk /L:$KB failed ($ExitCode): $Output"
}

# Assert-LogSize EXITCODE OUTPUT KB: returns when `chkdsk /L` reports a
# $LogFile of KB kilobytes, throws naming what it reported otherwise.
function Assert-LogSize([int]$ExitCode, [string[]]$Output, [int]$KB) {
    # A readback chkdsk failed proves nothing, whatever it printed.
    if ($ExitCode -ne 0) { throw "chkdsk /L failed ($ExitCode): $Output" }
    if (($Output -join ' ') -notmatch 'current log file size is (\d+) KB' -or [int]$Matches[1] -ne $KB) {
        throw "chkdsk /L:$KB left a log of another size: $Output"
    }
}
