<#
.SYNOPSIS
Removes the "chip-remote-agent" Scheduled Task and stops a running agent.

.PARAMETER Purge
Also delete %APPDATA%\chip-remote (config.json, shared with the Windows hooks) and
%LOCALAPPDATA%\chip-remote (logs). Without it, config and logs are kept.

This file must stay pure ASCII (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI).
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [switch]$Purge
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$taskName = 'chip-remote-agent'

$task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($task) {
    if ($PSCmdlet.ShouldProcess($taskName, 'Stop and unregister scheduled task')) {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
        Write-Host "unregistered scheduled task '$taskName'"
    }
} else {
    Write-Host "scheduled task '$taskName' not found"
}

# An agent started by hand (not through the task) keeps running otherwise.
$procs = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" |
    Where-Object { $_.CommandLine -and $_.CommandLine -like '*chip-remote-agent.ps1*' -and
        $_.CommandLine -notlike '*-Probe*' -and $_.CommandLine -notlike '*-Invoke*' -and
        $_.ProcessId -ne $PID })
foreach ($p in $procs) {
    if ($PSCmdlet.ShouldProcess("pid $($p.ProcessId)", 'Stop chip-remote agent process')) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
        Write-Host "stopped agent pid $($p.ProcessId)"
    }
}

if ($Purge) {
    foreach ($dir in @((Join-Path $env:APPDATA 'chip-remote'), (Join-Path $env:LOCALAPPDATA 'chip-remote'))) {
        if ((Test-Path -LiteralPath $dir) -and $PSCmdlet.ShouldProcess($dir, 'Remove directory')) {
            Remove-Item -LiteralPath $dir -Recurse -Force
            Write-Host "removed $dir"
        }
    }
} else {
    Write-Host ("kept config {0} (use -Purge to delete)" -f (Join-Path $env:APPDATA 'chip-remote\config.json'))
}
