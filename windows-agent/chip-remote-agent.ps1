<#
.SYNOPSIS
chip-remote Windows agent: finds spawn_task chips in Claude desktop via UI Automation and
presses their buttons on request from the chip-remote Worker (WebSocket /v1/agent/ws).

.DESCRIPTION
Resident mode (no switches): connects to the Worker and loops forever, reconnecting with
exponential backoff. Protocol: docs/PROTOCOL.md.

Debug modes (no network):
  -Probe                               list every chip visible right now
  -Invoke start|dismiss -Title "..."   press a button on one chip (optionally -Tldr)

Config: %APPDATA%\chip-remote\config.json (UTF-8)
  { "url": "https://chip-remote.ippoan.org", "token": "...",
    "labels": { "marker": "...", "start": "...", "dismiss": "..." },
    "locateTimeoutSec": 5, "scanIntervalSec": 2, "takeoverBackoffSec": 300,
    "raiseWaitSec": 5, "preventSleep": true }
Log:    %LOCALAPPDATA%\chip-remote\agent.log (rotated at 1 MB)

This file must stay pure ASCII (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI).
#>
[CmdletBinding(DefaultParameterSetName = 'Run')]
param(
    [Parameter(ParameterSetName = 'Probe', Mandatory = $true)]
    [switch]$Probe,

    [Parameter(ParameterSetName = 'Invoke', Mandatory = $true)]
    [ValidateSet('start', 'dismiss')]
    [string]$Invoke,

    [Parameter(ParameterSetName = 'Invoke', Mandatory = $true)]
    [string]$Title,

    [Parameter(ParameterSetName = 'Invoke')]
    [string]$Tldr,

    [string]$ConfigPath
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'ChipUia.psm1') -Force

if (-not $ConfigPath) { $ConfigPath = Get-ChipRemoteConfigPath }

# ---------------------------------------------------------------- logging

$script:LogDir = Join-Path $env:LOCALAPPDATA 'chip-remote'
$script:LogFile = Join-Path $script:LogDir 'agent.log'
$script:LogMaxBytes = 1MB
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Write-AgentLog {
    param([string]$Message, [string]$Level = 'INFO')
    $line = '{0} [{1}] {2}' -f (Get-Date).ToString('yyyy-MM-ddTHH:mm:ss.fffzzz'), $Level, $Message
    try {
        if (-not (Test-Path -LiteralPath $script:LogDir)) {
            [void](New-Item -ItemType Directory -Path $script:LogDir -Force)
        }
        if ((Test-Path -LiteralPath $script:LogFile) -and
            ((Get-Item -LiteralPath $script:LogFile).Length -gt $script:LogMaxBytes)) {
            Move-Item -LiteralPath $script:LogFile -Destination ($script:LogFile + '.1') -Force
        }
        [System.IO.File]::AppendAllText($script:LogFile, $line + [Environment]::NewLine, $script:Utf8NoBom)
    } catch {
        # Logging must never take the agent down.
        Write-Verbose ('log write failed: ' + $_.Exception.Message)
    }
    if ([Environment]::UserInteractive) { Write-Verbose $line }
}

# ---------------------------------------------------------------- config

function Get-ConfigValue {
    param($Config, [string]$Name, $Default)
    if ($Config -and ($Config.PSObject.Properties.Name -contains $Name) -and $null -ne $Config.$Name) {
        return $Config.$Name
    }
    return $Default
}

function Get-WebSocketUri {
    param([string]$BaseUrl)
    $u = $BaseUrl.TrimEnd('/')
    if ($u -match '^https://') { $u = 'wss://' + $u.Substring(8) }
    elseif ($u -match '^http://') { $u = 'ws://' + $u.Substring(7) }
    elseif ($u -notmatch '^wss?://') { throw "unsupported url scheme: $BaseUrl" }
    return [Uri]($u + '/v1/agent/ws')
}

# ---------------------------------------------------------------- debug modes

function Invoke-ProbeMode {
    param([hashtable]$Labels)
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    $window = Get-ClaudeWindow
    if (-not $window) {
        Write-Host 'Claude desktop is not running (no claude.exe with a main window).'
        return 1
    }
    Write-Host ('window: "{0}" hwnd={1}' -f $window.Current.Name, $window.Current.NativeWindowHandle)
    $chips = @(Invoke-WithClaudeRaised -Window $window -ScriptBlock {
            Wait-ChipList -Window $window -Labels $Labels -TimeoutSec $script:RaiseWaitSec
        })
    Write-Host ('chips: {0}' -f $chips.Count)
    $i = 0
    foreach ($c in $chips) {
        $i++
        Write-Host ''
        Write-Host ('[{0}] title:   {1}' -f $i, $c.Title)
        Write-Host ('    tldr:    {0}' -f $c.Tldr)
        Write-Host ('    buttons: {0}' -f ($c.Buttons -join ' | '))
        Write-Host ('    offscreen={0} rect={1}' -f $c.IsOffscreen, $c.Rect)
    }
    if ($chips.Count -eq 0) {
        Write-Host 'Only chips of sessions whose chat pane is currently shown are in the UIA tree.'
    }
    return 0
}

function Invoke-OneShotMode {
    param([hashtable]$Labels, [string]$Action, [string]$ChipTitle, [string]$ChipTldr)
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    $result = Invoke-ChipRequest -Labels $Labels -Action $Action -ChipTitle $ChipTitle -ChipTldr $ChipTldr
    if ($result.ok) {
        Write-Host ('ok: {0} "{1}"' -f $Action, $ChipTitle)
        return 0
    }
    Write-Host ('failed: {0} ({1})' -f $result.error, $result.detail)
    return 1
}

# ---------------------------------------------------------------- UIA request

function Invoke-ChipRequest {
    # Returns @{ok; error; detail} with PROTOCOL error codes.
    param([hashtable]$Labels, [string]$Action, [string]$ChipTitle, [string]$ChipTldr)
    try {
        $window = Get-ClaudeWindow
        if (-not $window) {
            return @{ ok = $false; error = 'claude_not_running'; detail = 'no Claude main window' }
        }
        # The window is usually covered while the user is away, so Chromium has not
        # rendered the chip; un-occlude it for the duration of find + invoke.
        return (Invoke-WithClaudeRaised -Window $window -ScriptBlock {
            # Wait until Chromium renders again (any chip), then briefly for the title itself
            # (other panes' chips may be stale for a moment); if it is behind the pager,
            # Find-ChipPaged pages to it.
            [void](Wait-ChipList -Window $window -Labels $Labels -TimeoutSec $script:RaiseWaitSec)
            [void](Wait-ChipList -Window $window -Labels $Labels -Title $ChipTitle -Tldr $ChipTldr -TimeoutSec 1.5)
            $hit = Find-ChipPaged -Window $window -Labels $Labels -Title $ChipTitle -Tldr $ChipTldr
            if (-not $hit) {
                return @{ ok = $false; error = 'chip_not_found'; detail = 'no chip with that title in the UIA tree' }
            }
            return (Invoke-ChipAction -Chip $hit.Element -Action $Action -Labels $Labels)
        })
    } catch {
        return @{ ok = $false; error = 'invoke_failed'; detail = $_.Exception.Message }
    }
}

# ---------------------------------------------------------------- websocket helpers

function Send-WsJson {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, $Object)
    $json = $Object | ConvertTo-Json -Compress -Depth 6
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $seg = New-Object System.ArraySegment[byte] -ArgumentList @(, $bytes)
    $task = $Socket.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Text, $true,
        [System.Threading.CancellationToken]::None)
    if (-not $task.Wait(10000)) { throw 'websocket send timed out' }
    Write-AgentLog ('> ' + $json)
}

function Close-WsQuietly {
    param([System.Net.WebSockets.ClientWebSocket]$Socket)
    if (-not $Socket) { return }
    try {
        $state = $Socket.State
        if ($state -eq [System.Net.WebSockets.WebSocketState]::Open -or
            $state -eq [System.Net.WebSockets.WebSocketState]::CloseReceived) {
            [void]$Socket.CloseOutputAsync([System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure, 'bye',
                [System.Threading.CancellationToken]::None).Wait(2000)
        }
    } catch {
        Write-Verbose ('close failed: ' + $_.Exception.Message)
    }
    try { $Socket.Dispose() } catch { Write-Verbose 'dispose failed' }
}

# ---------------------------------------------------------------- locate queue

# task_id -> @{ task_id; title; tldr; deadline }
$script:LocateQueue = @{}

# Seconds to wait for Chromium to rebuild its a11y tree after un-occluding the window.
$script:RaiseWaitSec = 5

function Add-LocateItem {
    param($Chip, [int]$TimeoutSec)
    if (-not $Chip) { return }
    $id = [string]$Chip.task_id
    if (-not $id) { return }
    $status = $null
    if ($Chip.PSObject.Properties.Name -contains 'status') { $status = [string]$Chip.status }
    if ($status -and $status -ne 'located_pending') { return }
    if ($script:LocateQueue.ContainsKey($id)) { return }
    $tl = $null
    if ($Chip.PSObject.Properties.Name -contains 'tldr') { $tl = [string]$Chip.tldr }
    $script:LocateQueue[$id] = @{
        task_id  = $id
        title    = [string]$Chip.title
        tldr     = $tl
        deadline = (Get-Date).AddSeconds($TimeoutSec)
    }
    Write-AgentLog ('queued ' + $id)
}

function Invoke-LocateScan {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [hashtable]$Labels)
    if ($script:LocateQueue.Count -eq 0) { return }
    $chips = @()
    try {
        $window = Get-ClaudeWindow
        # Read-only: never move the window here. A chip appears while the user is at the
        # PC, so raising Claude would steal the screen. If the window is covered the chip
        # is simply not found and the phone gets located=false; the raise happens only
        # when an action arrives from the phone (Invoke-ChipRequest).
        if ($window) { $chips = @(Get-ChipList -Window $window -Labels $Labels) }
    } catch {
        Write-AgentLog ('scan failed: ' + $_.Exception.Message) 'WARN'
    }
    $now = Get-Date
    foreach ($id in @($script:LocateQueue.Keys)) {
        $item = $script:LocateQueue[$id]
        $hit = $null
        if ($chips.Count -gt 0) { $hit = Select-Chip -Chips $chips -Title $item.title -Tldr $item.tldr }
        if ($hit) {
            Send-WsJson $Socket ([ordered]@{ type = 'chip.located'; task_id = $id })
            $script:LocateQueue.Remove($id)
        } elseif ($now -ge $item.deadline) {
            Send-WsJson $Socket ([ordered]@{ type = 'chip.not_found'; task_id = $id })
            $script:LocateQueue.Remove($id)
        }
    }
}

# ---------------------------------------------------------------- message handling

function Invoke-WsMessage {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [string]$Text, [hashtable]$Labels, [int]$LocateTimeoutSec)
    Write-AgentLog ('< ' + $Text)
    try {
        $msg = $Text | ConvertFrom-Json
    } catch {
        Write-AgentLog ('bad json: ' + $_.Exception.Message) 'WARN'
        return
    }
    if (-not ($msg.PSObject.Properties.Name -contains 'type')) { return }
    switch ([string]$msg.type) {
        'hello' {
            $script:LocateQueue.Clear()
            if ($msg.PSObject.Properties.Name -contains 'chips') {
                foreach ($c in @($msg.chips)) { Add-LocateItem -Chip $c -TimeoutSec $LocateTimeoutSec }
            }
            $script:HelloReceived = $true
        }
        'chip.new' {
            if ($msg.PSObject.Properties.Name -contains 'chip') {
                Add-LocateItem -Chip $msg.chip -TimeoutSec $LocateTimeoutSec
            }
        }
        'chip.withdrawn' {
            $id = [string]$msg.task_id
            if ($script:LocateQueue.ContainsKey($id)) { $script:LocateQueue.Remove($id) }
        }
        'action' {
            $id = [string]$msg.task_id
            if ($script:LocateQueue.ContainsKey($id)) { $script:LocateQueue.Remove($id) }
            $tl = $null
            if ($msg.PSObject.Properties.Name -contains 'tldr') { $tl = [string]$msg.tldr }
            $action = [string]$msg.action
            if ($action -ne 'start' -and $action -ne 'dismiss') {
                $r = @{ ok = $false; error = 'invoke_failed'; detail = "unknown action '$action'" }
            } else {
                $r = Invoke-ChipRequest -Labels $Labels -Action $action -ChipTitle ([string]$msg.title) -ChipTldr $tl
            }
            if (-not $r.ok) { Write-AgentLog ('action {0} {1} failed: {2} {3}' -f $action, $id, $r.error, $r.detail) 'WARN' }
            Send-WsJson $Socket ([ordered]@{
                    type       = 'action.result'
                    request_id = [string]$msg.request_id
                    task_id    = $id
                    ok         = [bool]$r.ok
                    error      = $r.error
                })
        }
        'ping' {
            Send-WsJson $Socket ([ordered]@{ type = 'pong' })
        }
        default {
            Write-AgentLog ('ignored message type ' + $msg.type)
        }
    }
}

# ---------------------------------------------------------------- session

function Invoke-AgentSession {
    # Runs one WebSocket connection until it closes. Returns the close status code (int) or -1.
    param([Uri]$Uri, [string]$Token, [hashtable]$Labels, [int]$LocateTimeoutSec, [int]$ScanIntervalSec)

    $socket = New-Object System.Net.WebSockets.ClientWebSocket
    $socket.Options.SetRequestHeader('Authorization', 'Bearer ' + $Token)
    $socket.Options.KeepAliveInterval = [TimeSpan]::FromSeconds(30)
    $closeCode = -1
    try {
        Write-AgentLog ('connecting ' + $Uri)
        $connect = $socket.ConnectAsync($Uri, [System.Threading.CancellationToken]::None)
        if (-not $connect.Wait(15000)) { throw 'connect timed out' }
        Write-AgentLog 'connected'

        $buffer = New-Object byte[] 65536
        $message = New-Object System.IO.MemoryStream
        $recv = $null
        $nextScan = Get-Date

        while ($socket.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
            if (-not $recv) {
                $seg = New-Object System.ArraySegment[byte] -ArgumentList @(, $buffer)
                $recv = $socket.ReceiveAsync($seg, [System.Threading.CancellationToken]::None)
            }
            if ($recv.Wait(250)) {
                $r = $recv.Result
                $recv = $null
                if ($r.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
                    if ($null -ne $r.CloseStatus) { $closeCode = [int]$r.CloseStatus }
                    Write-AgentLog ('server closed: {0} {1}' -f $closeCode, $r.CloseStatusDescription)
                    break
                }
                $message.Write($buffer, 0, $r.Count)
                if ($r.EndOfMessage) {
                    $text = [System.Text.Encoding]::UTF8.GetString($message.ToArray())
                    $message.SetLength(0)
                    if ($r.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Text) {
                        Invoke-WsMessage -Socket $socket -Text $text -Labels $Labels -LocateTimeoutSec $LocateTimeoutSec
                    }
                }
            }
            if ((Get-Date) -ge $nextScan) {
                Invoke-LocateScan -Socket $socket -Labels $Labels
                $nextScan = (Get-Date).AddSeconds($ScanIntervalSec)
            }
        }
        if ($closeCode -lt 0 -and $null -ne $socket.CloseStatus) { $closeCode = [int]$socket.CloseStatus }
    } catch {
        $e = $_.Exception
        while ($e.InnerException) { $e = $e.InnerException }
        Write-AgentLog ('session error: ' + $e.Message) 'WARN'
    } finally {
        Close-WsQuietly $socket
    }
    return $closeCode
}

function Invoke-ResidentMode {
    param($Config, [hashtable]$Labels)
    $url = [string](Get-ConfigValue $Config 'url' '')
    $token = [string](Get-ConfigValue $Config 'token' '')
    if (-not $url -or -not $token -or $token -like 'REPLACE_*') {
        Write-AgentLog ("config.url / config.token missing in " + $ConfigPath) 'ERROR'
        return 2
    }
    $uri = Get-WebSocketUri $url
    $locateTimeout = [int](Get-ConfigValue $Config 'locateTimeoutSec' 5)
    $scanInterval = [int](Get-ConfigValue $Config 'scanIntervalSec' 2)
    $takeoverBackoff = [int](Get-ConfigValue $Config 'takeoverBackoffSec' 300)
    if ($scanInterval -lt 1) { $scanInterval = 1 }
    $script:RaiseWaitSec = [double](Get-ConfigValue $Config 'raiseWaitSec' 5)
    if ([bool](Get-ConfigValue $Config 'preventSleep' $true)) {
        if (Enable-KeepAwake) { Write-AgentLog 'keep-awake on (system sleep blocked while the agent runs)' }
        else { Write-AgentLog 'keep-awake request failed' 'WARN' }
    }

    # One agent per desktop session; a second one would keep kicking the first off (close 4000).
    $created = $false
    $mutex = New-Object System.Threading.Mutex($true, 'Local\chip-remote-agent', [ref]$created)
    if (-not $created) {
        Write-AgentLog 'another chip-remote-agent is already running in this session; exiting' 'WARN'
        return 3
    }

    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor
        [Net.SecurityProtocolType]::Tls12

    Write-AgentLog ('agent start pid={0} url={1}' -f $PID, $uri)
    $backoff = 1
    try {
        while ($true) {
            $script:HelloReceived = $false
            $script:LocateQueue.Clear()
            $code = Invoke-AgentSession -Uri $uri -Token $token -Labels $Labels `
                -LocateTimeoutSec $locateTimeout -ScanIntervalSec $scanInterval
            if ($script:HelloReceived) { $backoff = 1 }
            if ($code -eq 4000) {
                $wait = $takeoverBackoff
                Write-AgentLog ('replaced by another agent (4000); retry in {0}s' -f $wait) 'WARN'
            } else {
                $wait = $backoff
                $backoff = [Math]::Min($backoff * 2, 60)
                Write-AgentLog ('disconnected (code {0}); retry in {1}s' -f $code, $wait)
            }
            Start-Sleep -Seconds $wait
        }
    } finally {
        $mutex.ReleaseMutex()
        $mutex.Dispose()
    }
}

# ---------------------------------------------------------------- main

$script:HelloReceived = $false
$config = $null
try {
    $config = Read-ChipRemoteConfig -Path $ConfigPath
} catch {
    Write-AgentLog ('cannot read config {0}: {1}' -f $ConfigPath, $_.Exception.Message) 'ERROR'
    if ($PSCmdlet.ParameterSetName -eq 'Run') { exit 2 }
}
$labels = Get-ChipLabels -Config $config

switch ($PSCmdlet.ParameterSetName) {
    'Probe' { exit (Invoke-ProbeMode -Labels $labels) }
    'Invoke' { exit (Invoke-OneShotMode -Labels $labels -Action $Invoke -ChipTitle $Title -ChipTldr $Tldr) }
    default { exit (Invoke-ResidentMode -Config $config -Labels $labels) }
}
