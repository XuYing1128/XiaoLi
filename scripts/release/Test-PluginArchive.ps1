[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Archive,

    [Parameter(Mandatory = $true)]
    [string]$ScratchRoot
)

$ErrorActionPreference = 'Stop'
$archivePath = (Resolve-Path -LiteralPath $Archive).Path
$scratchPath = [System.IO.Path]::GetFullPath($ScratchRoot)
New-Item -ItemType Directory -Path $scratchPath -Force | Out-Null
$extractRoot = Join-Path $scratchPath ("plugin-archive-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $extractRoot | Out-Null

Expand-Archive -LiteralPath $archivePath -DestinationPath $extractRoot
$pluginRoot = Join-Path $extractRoot 'xiaoli-model-monitor'
if (-not (Test-Path -LiteralPath $pluginRoot -PathType Container)) {
    throw "Plugin archive is missing its xiaoli-model-monitor root: $archivePath"
}

& node (Join-Path $PSScriptRoot 'validate-plugin.mjs') $pluginRoot
if ($LASTEXITCODE -ne 0) {
    throw "Extracted plugin validation failed: $archivePath"
}

Write-Host "Plugin archive smoke passed: $archivePath"
