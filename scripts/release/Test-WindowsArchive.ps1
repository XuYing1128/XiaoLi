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
$extractRoot = Join-Path $scratchPath ("windows-archive-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $extractRoot | Out-Null

Expand-Archive -LiteralPath $archivePath -DestinationPath $extractRoot
$executable = Join-Path $extractRoot 'XiaoLi/XiaoLi.exe'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Portable archive is missing XiaoLi/XiaoLi.exe: $archivePath"
}
$licenseCatalog = Join-Path $extractRoot 'XiaoLi/THIRD_PARTY_LICENSES.html'
if (-not (Test-Path -LiteralPath $licenseCatalog -PathType Leaf) -or
    -not (Select-String -LiteralPath $licenseCatalog `
        -SimpleMatch 'data-complete-license-catalog="true"' -Quiet)) {
    throw "Portable archive is missing the complete offline license catalog: $archivePath"
}
$pluginRoot = Join-Path $extractRoot 'XiaoLi/plugin/xiaoli-model-monitor'
& node (Join-Path $PSScriptRoot 'validate-plugin.mjs') $pluginRoot
if ($LASTEXITCODE -ne 0) {
    throw "Portable archive contains an invalid plugin payload: $archivePath"
}

& (Join-Path $PSScriptRoot 'Test-Portable.ps1') `
    -Executable $executable `
    -ScratchRoot (Join-Path $extractRoot 'smoke-state')

Write-Host "Windows archive smoke passed: $archivePath"
