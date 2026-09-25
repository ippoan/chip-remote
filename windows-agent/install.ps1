<#
.SYNOPSIS
Registers the chip-remote agent as a Scheduled Task that starts at logon of the current user.

.DESCRIPTION
- Creates %APPDATA%\chip-remote\config.json from config.example.json when it is missing
  (byte copy, the file stays UTF-8). Edit url / token afterwards.
- Registers task "chip-remote-agent": trigger At logon (this user), Interactive logon type
  (UIA only works inside the user's desktop session), RunLevel Limited, no time limit,
  restart on failure.
- Does not need an elevated prompt.

.PARAMETER Start
Start the task right away after registering it.

This file must stay pure ASCII (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI).
#>
[CmdletBinding()]
param(
    [switch]$Start
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$taskName = 'chip-remote-agent'
$agentPath = Join-Path $PSScriptRoot 'chip-remote-agent.ps1'
$examplePath = Join-Path $PSScriptRoot 'config.example.json'
$configDir = Join-Path $env:APPDATA 'chip-remote'
$configPath = Join-Path $configDir 'config.json'

if (-not (Test-Path -LiteralPath $agentPath)) { throw "agent script not found: $agentPath" }

# 1. config.json
if (-not (Test-Path -LiteralPath $configPath)) {
    if (-not (Test-Path -LiteralPath $configDir)) { [void](New-Item -ItemType Directory -Path $configDir -Force) }
    [System.IO.File]::Copy($examplePath, $configPath)
    Write-Host "created $configPath -- set url and token in it."
} else {
    Write-Host "config exists: $configPath"
}

# 2. scheduled task
$user = '{0}\{1}' -f $env:USERDOMAIN, $env:USERNAME
$argument = '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "{0}"' -f $agentPath
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $argument -WorkingDirectory $PSScriptRoot
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1)

$task = New-ScheduledTask -Action $action -Trigger $trigger -Principal $principal -Settings $settings `
    -Description 'chip-remote: operate Claude desktop spawn_task chips from the phone (UI Automation agent).'
[void](Register-ScheduledTask -TaskName $taskName -InputObject $task -Force)
Write-Host "registered scheduled task '$taskName' (at logon of $user)"
Write-Host "  powershell.exe $argument"

if ($Start) {
    Start-ScheduledTask -TaskName $taskName
    Write-Host "started. log: $(Join-Path $env:LOCALAPPDATA 'chip-remote\agent.log')"
} else {
    Write-Host "start now with: Start-ScheduledTask -TaskName $taskName   (or re-run with -Start)"
}
