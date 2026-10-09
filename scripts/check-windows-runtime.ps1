param(
    [Parameter(Mandatory = $true)]
    [string]$Binary
)

$ErrorActionPreference = 'Stop'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$installation = & $vswhere -latest -products '*' -property installationPath
if ($LASTEXITCODE -ne 0 -or -not $installation) {
    throw 'Cannot locate Visual Studio to inspect Windows DLL dependencies.'
}

$dumpbin = Get-ChildItem "$installation\VC\Tools\MSVC\*\bin\Host*\*\dumpbin.exe" |
    Select-Object -First 1
if (-not $dumpbin) {
    throw 'Cannot locate dumpbin.exe.'
}

$dependencies = & $dumpbin.FullName /DEPENDENTS $Binary
if ($LASTEXITCODE -ne 0) {
    throw "dumpbin failed for $Binary."
}
$dependencies | Write-Output
# Includes numbered/debug variants and both regular and delay-load imports.
# UCRT is part of supported Windows versions; VC++ runtime DLLs are not.
if ($dependencies -match '(?i)\b(?:vcruntime|msvcp|concrt|vcomp)\d[^\s]*\.dll\b') {
    throw 'Beacon still imports a VC++ runtime DLL; rebuild with +crt-static before packaging.'
}
