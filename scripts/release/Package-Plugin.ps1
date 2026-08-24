[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,

    [Parameter(Mandatory = $true)]
    [string]$Version
)

$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$') {
    throw "Invalid version: $Version"
}
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$pluginRoot = Join-Path $repositoryRoot 'plugin/xiaoli-model-monitor'
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$archivePath = Join-Path $outputRoot "xiaoli-codex-plugin-v$Version.zip"
$stageRoot = Join-Path $outputRoot 'plugin-stage'
$stagePlugin = Join-Path $stageRoot 'xiaoli-model-monitor'
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
if (Test-Path -LiteralPath $stageRoot) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $stageRoot -Force | Out-Null
Copy-Item -LiteralPath $pluginRoot -Destination $stagePlugin -Recurse -Force
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE') -Destination $stagePlugin
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'ASSET_PROVENANCE.md') -Destination $stagePlugin
foreach ($emptyPluginDirectory in @('scripts', 'tools')) {
    $candidate = Join-Path $stagePlugin $emptyPluginDirectory
    if ((Test-Path -LiteralPath $candidate) -and
        -not (Get-ChildItem -LiteralPath $candidate -Force | Select-Object -First 1)) {
        Remove-Item -LiteralPath $candidate -Force
    }
}
if (Test-Path -LiteralPath $archivePath) {
    Remove-Item -LiteralPath $archivePath -Force
}
# Compress-Archive omits dotfiles on Unix, which would silently drop both
# `.codex-plugin/plugin.json` and `.mcp.json`. Zip the stage root through .NET
# so the same plugin payload is produced on Windows, macOS and Linux.
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $stageRoot,
    $archivePath,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $false
)
if ((Get-Item -LiteralPath $archivePath).Length -le 0) {
    throw 'Plugin ZIP is empty'
}
Remove-Item -LiteralPath $stageRoot -Recurse -Force
Write-Host $archivePath
