<#
chip-remote: PostToolUse hook for mcp__ccd_session__spawn_task (local Windows sessions).

Reads the hook JSON from stdin and reports the new chip to the Worker:
  POST <url>/v1/chips  {task_id, title, tldr, cwd, host, session_id}

Never blocks or disturbs Claude: always exits 0, writes nothing to stdout,
appends problems to %LOCALAPPDATA%\chip-remote\hook.log.

Config: %APPDATA%\chip-remote\config.json (shared with the agent; keys url, token, optional host).
        Override the path with $env:CHIP_REMOTE_CONFIG.

This file must stay pure ASCII (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI).
#>
$ErrorActionPreference = 'Stop'

function Write-HookLog {
    param([string]$Message)
    try {
        $dir = Join-Path $env:LOCALAPPDATA 'chip-remote'
        if (-not (Test-Path -LiteralPath $dir)) { [void](New-Item -ItemType Directory -Path $dir -Force) }
        $line = '{0} post-spawn-task: {1}{2}' -f (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'), $Message, [Environment]::NewLine
        [System.IO.File]::AppendAllText((Join-Path $dir 'hook.log'), $line, (New-Object System.Text.UTF8Encoding($false)))
    } catch {
        Write-Verbose 'log write failed'
    }
}

function Read-StdinUtf8 {
    # Read raw bytes: [Console]::In would decode with the OEM code page and break Japanese.
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

    $raw = Read-StdinUtf8
    $hook = $raw | ConvertFrom-Json
    $toolInput = Get-Prop $hook 'tool_input'
    $response = Get-Prop $hook 'tool_response'

    # tool_response may be a string, {content:[{type,text}]} or an array of blocks.
    if ($response -is [string]) { $respText = $response }
    else { $respText = ConvertTo-Json -InputObject $response -Depth 20 -Compress }
    $m = [regex]::Match([string]$respText, 'task_[0-9a-f]+')
    if (-not $m.Success) {
        $snippet = [string]$respText
        if ($snippet.Length -gt 300) { $snippet = $snippet.Substring(0, 300) }
        Write-HookLog "task_id not found in tool_response: $snippet"
        exit 0
    }

    $title = [string](Get-Prop $toolInput 'title')
    $tldr = [string](Get-Prop $toolInput 'tldr')
    $cwd = Get-Prop $toolInput 'cwd'
    if (-not $cwd) { $cwd = Get-Prop $hook 'cwd' }
    $hostName = [string](Get-Prop $cfg 'host')
    if (-not $hostName) { $hostName = $env:COMPUTERNAME }

    $body = [ordered]@{
        task_id    = $m.Value
        title      = $title
        tldr       = $tldr
        cwd        = $cwd
        host       = $hostName
        session_id = (Get-Prop $hook 'session_id')
    }
    $json = ConvertTo-Json -InputObject $body -Compress

    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor
        [Net.SecurityProtocolType]::Tls12
    $target = $url.TrimEnd('/') + '/v1/chips'
    try {
        Invoke-RestMethod -Method Post -Uri $target -TimeoutSec 5 `
            -Headers @{ Authorization = 'Bearer ' + $token } `
            -ContentType 'application/json; charset=utf-8' `
            -Body ([System.Text.Encoding]::UTF8.GetBytes($json)) | Out-Null
    } catch {
        Write-HookLog ('POST {0} for {1} failed: {2}' -f $target, $m.Value, $_.Exception.Message)
    }
} catch {
    Write-HookLog ('error: ' + $_.Exception.Message)
}
exit 0
