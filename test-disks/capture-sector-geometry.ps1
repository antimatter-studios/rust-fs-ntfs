param([string]$OutDir = 'tmp/format-geometry')
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../scripts/windows-format-geometry.ps1"
Invoke-WindowsSectorGeometryCapture -OutDir $OutDir -SectorBytes 4096
