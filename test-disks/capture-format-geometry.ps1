param([string]$OutDir = 'tmp/format-geometry')
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../scripts/windows-format-geometry.ps1"
Invoke-WindowsFormatGeometryCapture -OutDir $OutDir
