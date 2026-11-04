#!/usr/bin/env bash
# win-quota-enforce.ps1 is a measurement only while the write past the limit
# fails with the status Windows returns for a limit, and leaves the owner at or
# under that limit (#1244). Any other error is a device or an access check, not
# a volume enforcing its quota.
#
# This runs the real op and the real _quota.ps1 under pwsh, from a sandbox whose
# _lib.ps1 is a stand-in: no VHD, fsutil and icacls answering from the text
# below, and a write that raises the error each case asks for from a real .NET
# method, the way FileStream does. So it proves the op's decision and what the
# report says, not what Windows does: only the merge queue's Windows job runs
# the op against a real volume.
#
# The case named "measured" is the report in artifact
# ntfs-quota-enforce-measurement of run 41748660665: HRESULT 0x80070070, Win32
# 112, 65536 bytes accepted, and the owner charged 66560 against a limit of
# 66560. The same status is in the first retained report, run 41558177459.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
harness="$ROOT/scripts/fs-windows-test-harness"
name="$(basename "${BASH_SOURCE[0]}")"
fails=0
ok() { printf 'ok   %s\n' "$*"; }
fail() { fails=$((fails + 1)); printf 'FAIL %s\n' "$*"; }

if ! command -v pwsh >/dev/null 2>&1; then
    printf 'FAIL pwsh is required to run the op: install PowerShell on the runner\n'
    exit 1
fi

sandbox="$(mktemp -d)"
trap 'rm -r "$sandbox"' EXIT
mkdir "$sandbox/harness"
cp "$harness/win-quota-enforce.ps1" "$harness/_quota.ps1" "$sandbox/harness/"

cat >"$sandbox/harness/_lib.ps1" <<'PS'
Add-Type -TypeDefinition @'
namespace StandIn {
    public class Stream {
        private long room;
        private readonly string hresult;
        private readonly string raiser;
        public Stream(long room, string hresult, string raiser) {
            this.room = room;
            this.hresult = hresult;
            this.raiser = raiser;
        }
        private void Raise() {
            int status = System.Convert.ToInt32(hresult, 16);
            throw new System.IO.IOException("stand-in error " + hresult, status);
        }
        public void Write(byte[] buffer, int offset, int count) {
            if (raiser == "write" && count > room) { Raise(); }
            room -= count;
        }
        public void Flush(bool toDisk) { }
        public void Dispose() {
            if (raiser == "dispose") { Raise(); }
        }
    }
}
'@

function Get-VhdPathFor { param($ImagePath) "$ImagePath.vhd" }
function Initialize-VhdFromImg { param($ImagePath, $Diag) @{ Vhd = "$ImagePath.vhd" } }
function Mount-VhdAndGetLetter {
    param($Vhd)
    New-PSDrive -Name Q -PSProvider FileSystem -Root $env:STANDIN_VOLUME -Scope Global | Out-Null
    'Q'
}
function Dismount-VhdAndCleanup { param($Vhd, $ImagePath, $KeepImage) }

$global:StandInLimit = '18446744073709551615'
$global:StandInUsed = '1024'

function icacls.exe { $global:LASTEXITCODE = 0 }
function fsutil.exe {
    $global:LASTEXITCODE = 0
    if ($args[1] -eq 'modify') { $global:StandInLimit = $args[3] }
    @"
FileSystemControlFlags = 0x00000002
    Quotas are tracked and enforced on this volume
    Logging for quota events is not enabled
    The quota values are up to date

Default Quota Threshold = 0xffffffffffffffff
Default Quota Limit     = 0xffffffffffffffff

SID Name        = BUILTIN\Administrators (Alias)
Change time     = Wednesday, November 4, 2026   1:30:57 AM
Quota Used      = 70656
Quota Threshold = 18446744073709551615
Quota Limit     = 18446744073709551615

SID Name        = NT AUTHORITY\LOCAL SERVICE (WellKnownGroup)
Change time     = Wednesday, November 4, 2026   1:31:23 AM
Quota Used      = $global:StandInUsed
Quota Threshold = $global:StandInLimit
Quota Limit     = $global:StandInLimit
"@ -split "`r?`n"
}

function New-Object {
    if ($args[0] -eq 'System.Security.Principal.SecurityIdentifier') {
        $sid = [pscustomobject]@{}
        $sid | Add-Member -MemberType ScriptMethod -Name Translate -Value {
            param($type)
            [pscustomobject]@{ Value = 'NT AUTHORITY\LOCAL SERVICE' }
        }
        return $sid
    }
    if ($args[0] -eq 'System.IO.FileStream') {
        $global:StandInUsed = $env:STANDIN_USAGE_AFTER
        return [StandIn.Stream]::new(65536, $env:STANDIN_HRESULT, $env:STANDIN_RAISER)
    }
    Microsoft.PowerShell.Utility\New-Object @args
}
PS

# run CASE HRESULT RAISER USAGE_AFTER: the op's exit status lands in $status.
# RAISER is `write` (past 64 KiB), `dispose` or `none`.
run() {
    dir="$sandbox/$1"
    mkdir -p "$dir/volume"
    STANDIN_VOLUME="$dir/volume" STANDIN_HRESULT="$2" STANDIN_RAISER="$3" STANDIN_USAGE_AFTER="$4" \
        pwsh -NoProfile -NonInteractive -File "$sandbox/harness/win-quota-enforce.ps1" \
        -ImagePath "$dir/image.img" -Path /limited.bin -HeadroomBytes 65536 -WriteBytes 131072 \
        -Diag "$dir/diag" >"$dir/out" 2>"$dir/err"
    status=$?
}
expect_status() {
    if [[ "$status" -eq "$2" ]]; then
        ok "$1: exit $status"
    else
        fail "$1: exit $status, want $2; stdout: $(cat "$dir/out"); stderr: $(cat "$dir/err")"
    fi
}
# The stand-in's own error text is not evidence: only what the op printed is.
expect_err() {
    if grep -Fq -- "$2" "$dir/err"; then
        ok "$1: stderr names $2"
    else
        fail "$1: stderr does not name $2; stderr: $(cat "$dir/err")"
    fi
}
expect_report() {
    if tr -d '\r' <"$dir/diag/quota-enforce.txt" 2>/dev/null | grep -Fxq -- "$2"; then
        ok "$1: report holds $2"
    else
        fail "$1: report does not hold $2; report: $(head -12 "$dir/diag/quota-enforce.txt" 2>&1)"
    fi
}

run measured 80070070 write 66560
expect_status 'the measured refusal, usage at the limit' 0
expect_report 'the measured refusal' 'error_hresult=0x80070070'
expect_report 'the measured refusal' 'error_win32=112'
expect_report 'the measured refusal' 'limit=66560'
expect_report 'the measured refusal' 'usage_after=66560'

# Access denied, a device error, and a status from another facility sharing
# the low word of the measured one, so a comparison of Win32 codes alone fails.
for got in 80070005 8007045d 80040070; do
    run "other-$got" "$got" write 66560
    expect_status "another error, $got" 1
    expect_err "another error, $got" "0x$got"
    expect_err "another error, $got" '0x80070070'
    expect_report "another error, $got" "error_hresult=0x$got"
done

run above-limit 80070070 write 66561
expect_status 'usage one byte above the limit after the refusal' 1
expect_err 'usage above the limit' '66561'
expect_err 'usage above the limit' '66560'
expect_report 'usage above the limit' 'error_hresult=0x80070070'
expect_report 'usage above the limit' 'usage_after=66561'

# PowerShell wraps what a .NET method raises, and the wrapper has a status of
# its own, so a refusal met when the handle closes has to be unwrapped too.
run at-dispose 80070070 dispose 66560
expect_status 'the measured refusal when the handle closes' 0
expect_report 'the measured refusal when the handle closes' 'error_hresult=0x80070070'

run accepted 80070070 none 66560
expect_status 'a write with no refusal' 1
expect_err 'a write with no refusal' 'accepted all 131072 bytes'
expect_report 'a write with no refusal' 'error_hresult=none'

if (( fails > 0 )); then
    printf '%s: %d check(s) failed\n' "$name" "$fails"
    exit 1
fi
printf '%s: all checks passed\n' "$name"
