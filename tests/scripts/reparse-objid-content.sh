#!/usr/bin/env bash
# win-verify-reparse-objid.ps1 decides whether a file shows what mac-write
# wrote. The harness quotes the content as '{step.content}': sh removes the
# quotes, but on a Windows CI host the runner hands the command to cmd.exe,
# which passes them to the binary verbatim (fs-windows-test-harness
# runner/src/dispatch.rs), so run 43228668128 read back 'target' with its
# quotes. That is the transport, not the volume: the verifier accepts the
# content bare or in exactly one pair of single quotes, and nothing else.
# Reading through the link must also give exactly the target's own bytes.
#
# This loads the verifier's own functions from its syntax tree under pwsh and
# runs them on strings; only the Windows job runs the op on a real volume.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
op="$ROOT/scripts/fs-windows-test-harness/win-verify-reparse-objid.ps1"

if ! command -v pwsh >/dev/null 2>&1; then
    printf 'FAIL pwsh is required to load the verifier: install PowerShell on the runner\n'
    exit 1
fi

sandbox="$(mktemp -d)"
trap 'rm -r "$sandbox"' EXIT
cat >"$sandbox/check.ps1" <<'PS'
$ErrorActionPreference = 'Stop'
$op = $env:OP
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($op, [ref]$tokens, [ref]$errors)
$fails = 0
function Ok([string]$m) { Write-Output "ok   $m" }
function Bad([string]$m) { $script:fails++; Write-Output "FAIL $m" }
if ($errors.Count -ne 0) { Bad "the verifier does not parse: $($errors[0])" } else { Ok 'the verifier parses' }
foreach ($name in 'Test-WrittenContent', 'Get-LinkContentError') {
    $fn = $ast.Find({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
    if ($null -eq $fn) { Bad "the verifier defines no $name"; continue }
    . ([scriptblock]::Create($fn.Extent.Text))
}
if ($fails -ne 0) { exit 1 }

$q = "'"
$cases = @(
    @('bare', 'target', $true),
    @('one pair of quotes', "${q}target${q}", $true),
    @('two pairs of quotes', "${q}${q}target${q}${q}", $false),
    @('a leading quote only', "${q}target", $false),
    @('a trailing quote only', "target${q}", $false),
    @('double quotes', '"target"', $false),
    @('another case', 'Target', $false),
    @('corrupt bytes', "tar`0et", $false),
    @('a trailing newline', "target`n", $false),
    @('empty', '', $false)
)
foreach ($c in $cases) {
    $got = Test-WrittenContent $c[1] 'target'
    if ($got -eq $c[2]) { Ok "written content, $($c[0]): $got" } else { Bad "written content, $($c[0]): $got, want $($c[2])" }
}

$err = Get-LinkContentError 'E:\listed.bin' 'target' 'target' 'target'
if ($null -eq $err) { Ok 'link: bare bytes equal to the target pass' } else { Bad "link: bare: $err" }
$err = Get-LinkContentError 'E:\listed.bin' "${q}target${q}" "${q}target${q}" 'target'
if ($null -eq $err) { Ok 'link: quoted bytes equal to the target pass' } else { Bad "link: quoted: $err" }
$err = Get-LinkContentError 'E:\listed.bin' 'target' 'targeT' 'target'
if ($err -like '*but its target reads*') { Ok 'link: bytes unlike the direct target are refused' } else { Bad "link: unlike target: '$err'" }
$err = Get-LinkContentError 'E:\listed.bin' 'garbage' 'garbage' 'target'
if ($err -like "*want 'target'*") { Ok 'link: corrupt bytes equal to the target are refused' } else { Bad "link: corrupt: '$err'" }

if ($fails -ne 0) { Write-Output "reparse-objid-content: $fails check(s) failed"; exit 1 }
Write-Output 'reparse-objid-content: all checks passed'
PS
OP="$op" pwsh -NoProfile -NonInteractive -File "$sandbox/check.ps1"
