[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,

    [Parameter(Mandatory = $true)]
    [string]$ScratchRoot,

    [ValidateRange(1, 300)]
    [int]$ProcessTimeoutSeconds = 30
)

$ErrorActionPreference = 'Stop'
$executablePath = (Resolve-Path -LiteralPath $Executable).Path
$scratchPath = [System.IO.Path]::GetFullPath($ScratchRoot)
$sessionsPath = Join-Path $scratchPath 'sessions'
$statePath = Join-Path $scratchPath 'state'
$indexPath = Join-Path $scratchPath 'session-index.jsonl'

New-Item -ItemType Directory -Path $sessionsPath -Force | Out-Null
New-Item -ItemType Directory -Path $statePath -Force | Out-Null

# Force this smoke run onto an endpoint that cannot collide with a real XiaoLi
# instance owned by the same user. Production intentionally uses one per-user
# endpoint; an isolated test needs its own descriptor to prove offline behavior.
$endpointNonce = [guid]::NewGuid().ToString('N')
if ($env:OS -eq 'Windows_NT') {
    $endpointDescriptor = @{
        transport = 'windows-named-pipe'
        pipeName = "OpenAI.Codex.ModelMonitor.PortableSmoke.$endpointNonce"
    }
} else {
    $endpointDescriptor = @{
        transport = 'unix-domain-socket'
        path = (Join-Path $statePath "offline-$endpointNonce.sock")
    }
}
[System.IO.File]::WriteAllText(
    (Join-Path $statePath 'ipc-endpoint.json'),
    ($endpointDescriptor | ConvertTo-Json -Compress),
    [System.Text.UTF8Encoding]::new($false)
)

function Invoke-PortableProcess {
    param(
        [string[]]$Arguments,
        [AllowNull()]
        [string]$InputText,
        [string]$Name
    )
    $stdoutPath = Join-Path $scratchPath "$Name.stdout"
    $stderrPath = Join-Path $scratchPath "$Name.stderr"
    $inputPath = Join-Path $scratchPath "$Name.stdin"
    $parameters = @{
        FilePath = $executablePath
        ArgumentList = $Arguments
        Environment = @{ XIAOLI_STATE_DIR = $statePath }
        RedirectStandardOutput = $stdoutPath
        RedirectStandardError = $stderrPath
        PassThru = $true
    }
    if ($null -ne $InputText) {
        [System.IO.File]::WriteAllText($inputPath, $InputText, [System.Text.UTF8Encoding]::new($false))
        $parameters.RedirectStandardInput = $inputPath
    }
    $process = Start-Process @parameters
    $finished = $process.WaitForExit($ProcessTimeoutSeconds * 1000)
    if (-not $finished) {
        # This Process object was created from the verified executable path
        # above. Kill only that process tree; never search by a broad name.
        try {
            $process.Kill($true)
        } catch {
            if (-not $process.HasExited) {
                Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            }
        }
        [void]$process.WaitForExit(5000)
        throw "Portable $Name process exceeded the $ProcessTimeoutSeconds second timeout (PID $($process.Id))"
    }
    # Flush redirected asynchronous stream buffers before reading their files.
    $process.WaitForExit()
    [pscustomobject]@{
        ExitCode = $process.ExitCode
        Stdout = [System.IO.File]::ReadAllText($stdoutPath)
        Stderr = [System.IO.File]::ReadAllText($stderrPath)
    }
}

$probeResult = Invoke-PortableProcess -Name 'probe' -InputText $null -Arguments @(
    '--probe-once',
    '--sessions-root', $sessionsPath,
    '--session-index', $indexPath,
    '--state-root', $statePath
)
if ($probeResult.ExitCode -ne 0) {
    throw "Probe failed with exit code $($probeResult.ExitCode): $($probeResult.Stderr)"
}
$probe = $probeResult.Stdout | ConvertFrom-Json
if ($probe.schemaVersion -lt 5) {
    throw "Expected snapshot schema 5 or newer, received $($probe.schemaVersion)"
}
if ($null -eq $probe.collectorHealth -or $null -eq $probe.conversations) {
    throw 'Probe snapshot is missing required fields'
}

$hookPayload = @{
    hook_event_name = 'UserPromptSubmit'
    session_id = '00000000-0000-0000-0000-000000000001'
    turn_id = '00000000-0000-0000-0000-000000000002'
    model = 'gpt-5.6-sol'
    effort = 'ultra'
} | ConvertTo-Json -Compress
$hookResult = Invoke-PortableProcess -Name 'hook' -InputText $hookPayload `
    -Arguments @('--hook-capture', $statePath)
if ($hookResult.ExitCode -ne 0) {
    throw "Hook capture failed with exit code $($hookResult.ExitCode): $($hookResult.Stderr)"
}
$hook = $hookResult.Stdout | ConvertFrom-Json
if ($hook.continue -ne $true -or $hook.suppressOutput -ne $true) {
    throw 'Hook capture did not return the fail-open response'
}
$isolatedHookState = Join-Path $statePath 'hook-latest.json'
if (-not (Test-Path -LiteralPath $isolatedHookState -PathType Leaf)) {
    throw "Hook capture did not write to isolated XIAOLI_STATE_DIR: $statePath"
}

# A stopped GUI must never let MCP present a disk snapshot as current. Seed an
# unmistakably stale cache canary and require the read-only tool to fail closed.
$staleSnapshotCanary = 'stale-model-must-not-be-current'
[System.IO.File]::WriteAllText(
    (Join-Path $statePath 'latest-snapshot.json'),
    ('{"schemaVersion":5,"checkedAt":"2000-01-01T00:00:00Z","conversations":[{"activeRequest":{"model":"' + $staleSnapshotCanary + '"}}]}'),
    [System.Text.UTF8Encoding]::new($false)
)

$mcpInput = @(
    @{ jsonrpc = '2.0'; id = 1; method = 'initialize'; params = @{} },
    @{ jsonrpc = '2.0'; id = 2; method = 'tools/list'; params = @{} },
    @{ jsonrpc = '2.0'; id = 3; method = 'tools/call'; params = @{ name = 'get_monitor_summary'; arguments = @{} } }
) | ForEach-Object { $_ | ConvertTo-Json -Compress }
$mcpResult = Invoke-PortableProcess -Name 'mcp' `
    -InputText (($mcpInput -join [Environment]::NewLine) + [Environment]::NewLine) `
    -Arguments @('--mcp-server')
if ($mcpResult.ExitCode -ne 0) {
    throw "MCP self-test failed with exit code $($mcpResult.ExitCode): $($mcpResult.Stderr)"
}
$responses = @($mcpResult.Stdout -split '\r?\n' | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json })
$toolsResponse = $responses | Where-Object { $_.id -eq 2 } | Select-Object -First 1
if ($null -eq $toolsResponse) {
    throw 'MCP tools/list did not return a response'
}
$toolNames = @($toolsResponse.result.tools | ForEach-Object { $_.name })
$requiredTools = @(
    'get_monitor_summary',
    'get_session_detail',
    'render_monitor_card',
    'get_connection_origin',
    'list_relay_audits',
    'get_relay_audit'
)
$missingTools = @($requiredTools | Where-Object { $_ -notin $toolNames })
if ($missingTools.Count -gt 0) {
    throw "MCP tools/list is missing required read-only tools: $($missingTools -join ', ')"
}
$offlineResponse = $responses | Where-Object { $_.id -eq 3 } | Select-Object -First 1
$offlineText = @($offlineResponse.result.content | ForEach-Object { $_.text }) -join "`n"
if ($null -eq $offlineResponse -or $offlineResponse.result.isError -ne $true -or
    $offlineText -notmatch '(?i)offline' -or $offlineText.Contains($staleSnapshotCanary)) {
    throw 'MCP did not fail closed while the live XiaoLi monitor was offline'
}

Write-Host "Portable smoke passed: $executablePath"
