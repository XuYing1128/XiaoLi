[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,

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
$executablePath = (Resolve-Path -LiteralPath $Executable).Path
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$stageRoot = Join-Path $outputRoot 'windows-portable-stage'
$packageRoot = Join-Path $stageRoot 'XiaoLi'
$archivePath = Join-Path $outputRoot "XiaoLi-v$Version-Windows-x64-portable.zip"

New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
if (Test-Path -LiteralPath $stageRoot) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null

Copy-Item -LiteralPath $executablePath -Destination (Join-Path $packageRoot 'XiaoLi.exe')
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'plugin/xiaoli-model-monitor') `
    -Destination (Join-Path $packageRoot 'plugin/xiaoli-model-monitor') -Recurse -Force
foreach ($emptyPluginDirectory in @('scripts', 'tools')) {
    $candidate = Join-Path $packageRoot "plugin/xiaoli-model-monitor/$emptyPluginDirectory"
    if ((Test-Path -LiteralPath $candidate) -and
        -not (Get-ChildItem -LiteralPath $candidate -Force | Select-Object -First 1)) {
        Remove-Item -LiteralPath $candidate -Force
    }
}
$rootDocuments = @(
    'ASSET_PROVENANCE.md',
    'CHANGELOG.md',
    'CONTRIBUTING.md',
    'DESIGN.md',
    'LICENSE',
    'README.md',
    'README.en.md',
    'SECURITY.md',
    'THIRD_PARTY_NOTICES.md'
)
foreach ($document in $rootDocuments) {
    Copy-Item -LiteralPath (Join-Path $repositoryRoot $document) -Destination $packageRoot
}
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs') `
    -Destination (Join-Path $packageRoot 'docs') -Recurse
$assetDirectory = Join-Path $packageRoot 'src/assets'
New-Item -ItemType Directory -Path $assetDirectory -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'src/assets/mochi-app-icon.png') `
    -Destination $assetDirectory

& node (Join-Path $PSScriptRoot 'generate-third-party-licenses.mjs') `
    (Join-Path $packageRoot 'THIRD_PARTY_LICENSES.html')
if ($LASTEXITCODE -ne 0) {
    throw 'Third-party notice generation failed'
}

if (Test-Path -LiteralPath $archivePath) {
    Remove-Item -LiteralPath $archivePath -Force
}
Compress-Archive -LiteralPath $packageRoot -DestinationPath $archivePath -CompressionLevel Optimal
if ((Get-Item -LiteralPath $archivePath).Length -le 0) {
    throw 'Portable ZIP is empty'
}
Remove-Item -LiteralPath $stageRoot -Recurse -Force
Write-Host $archivePath
