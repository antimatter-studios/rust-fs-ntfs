# test-disks/_capture-chkdsk.ps1 -- what capture-interrupted-logfile.ps1
# makes of chkdsk's answer when it sets the $LogFile size. Dot-sourced by the
# capture and by tests/scripts/capture-chkdsk.ps1, which drives it with
# chkdsk's own reports and no Windows.

# Assert-LogResize EXITCODE OUTPUT KB: returns when `chkdsk /X /L:KB`
# succeeded, throws naming the status and the report when it did not.
function Assert-LogResize([int]$ExitCode, [string[]]$Output, [int]$KB) {
    if ($ExitCode -ne 0) { throw "chkdsk /L:$KB failed ($ExitCode): $Output" }
}
