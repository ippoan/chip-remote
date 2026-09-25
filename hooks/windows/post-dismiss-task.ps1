<#
chip-remote: PostToolUse hook for mcp__ccd_session__dismiss_task (local Windows sessions).

Reads the hook JSON from stdin and withdraws the chip on the Worker:
  DELETE <url>/v1/chips/<tool_input.task_id>

Never blocks or disturbs Claude: always exits 0, writes nothing to stdout,
appends problems to %LOCALAPPDATA%\chip-remote\hook.log.

Config: %APPDATA%\chip-remote\config.json (shared with the agent; keys url, token).
        Override the path with $env:CHIP_REMOTE_CONFIG.

This file must stay pure ASCII (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI).
#>
$ErrorActionPreference = 'Stop'

function Write-HookLog {
    param([string]$Message)
    try {
        $dir = Join-Path $env:LOCALAPPDATA 'chip-remote'
        if (-not (Test-Path -LiteralPath $dir)) { [void](New-Item -ItemType Directory -Path $dir -Force) }
        $line = '{0} post-dismiss-task: {1}{2}' -f (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'), $Message, [Environment]::NewLine
        [System.IO.File]::AppendAllText((Join-Path $dir 'hook.log'), $line, (New-Object System.Text.UTF8Encoding($false)))
    } catch {
        Write-Verbose 'log write failed'
    }
}

function Read-StdinUtf8 {
    $stdin = [Console]::OpenStandardInput()
    $ms = New-Object System.IO.MemoryStream
    $stdin.CopyTo($ms)
    $text = (New-Object System.Text.UTF8Encoding($false)).GetString($ms.ToArray())
    return $text.TrimStart([char]0xFEFF)
}

function Get-Prop {
    param($Object, [string]$Name)
    if ($null -ne $Object -and $Object -isnot [string] -and ($Object.PSObject.Properties.Name -contains $Name)) {
        return $Object.$Name
    }
    return $null
}

try {
    $cfgPath = $env:CHIP_REMOTE_CONFIG
    if (-not $cfgPath) { $cfgPath = Join-Path $env:APPDATA 'chip-remote\config.json' }
    if (-not (Test-Path -LiteralPath $cfgPath)) {
        Write-HookLog "config not found: $cfgPath"
        exit 0
    }
    $cfg = Get-Content -LiteralPath $cfgPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $url = [string](Get-Prop $cfg 'url')
    $token = [string](Get-Prop $cfg 'token')
    if (-not $url -or -not $token) {
        Write-HookLog "url / token missing in $cfgPath"
        exit 0
    }

    $hook = Read-StdinUtf8 | ConvertFrom-Json
    $taskId = [string](Get-Prop (Get-Prop $hook 'tool_input') 'task_id')
    # Only a well-formed id may end up in the URL path.
    if ($taskId -cnotmatch '^task_[0-9a-f]+$') {
        $shown = $taskId
        if ($shown.Length -gt 100) { $shown = $shown.Substring(0, 100) }
        Write-HookLog "invalid or missing tool_input.task_id: '$shown'"
        exit 0
    }

    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor
        [Net.SecurityProtocolType]::Tls12
    $target = $url.TrimEnd('/') + '/v1/chips/' + $taskId
    try {
        Invoke-RestMethod -Method Delete -Uri $target -TimeoutSec 5 `
            -Headers @{ Authorization = 'Bearer ' + $token } | Out-Null
    } catch {
        Write-HookLog ('DELETE {0} failed: {1}' -f $target, $_.Exception.Message)
    }
} catch {
    Write-HookLog ('error: ' + $_.Exception.Message)
}
exit 0
