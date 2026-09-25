<#
Tests for hooks/windows/*.ps1 under Windows PowerShell 5.1.

Runs each hook as a separate powershell.exe with a fixture on stdin, against a local
HttpListener that records the request. Uses the same fixtures as run.sh.
Exit code: 0 when all assertions pass.

This file must stay pure ASCII.
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$here = $PSScriptRoot
$hooks = Join-Path (Split-Path $here -Parent) 'windows'
$fixtures = Join-Path $here 'fixtures'
$work = Join-Path ([System.IO.Path]::GetTempPath()) ('chip-remote-hooktest-' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $work)

$script:Passed = 0
$script:Failed = 0
$script:Case = ''

function Assert-Equal {
    param($Expected, $Actual, [string]$What)
    if ($Expected -ceq $Actual) { $script:Passed++ }
    else {
        $script:Failed++
        Write-Host ("  FAIL [{0}] {1}: expected '{2}', got '{3}'" -f $script:Case, $What, $Expected, $Actual)
    }
}

function Assert-True {
    param([bool]$Condition, [string]$What)
    if ($Condition) { $script:Passed++ }
    else {
        $script:Failed++
        Write-Host ("  FAIL [{0}] {1}" -f $script:Case, $What)
    }
}

function Get-FreePort {
    $l = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 0)
    $l.Start()
    $p = $l.LocalEndpoint.Port
    $l.Stop()
    return $p
}

function Read-Fixture {
    param([string]$Name)
    return (Get-Content -LiteralPath (Join-Path $fixtures $Name) -Raw -Encoding UTF8 | ConvertFrom-Json)
}

function Invoke-Hook {
    <#
    Runs a hook with a fixture on stdin. When $Port is set, serves one request with $Status.
    Returns @{ exit; stdout; request = $null | @{method; path; auth; contentType; body}; log }
    #>
    param([string]$Script, [string]$Fixture, [int]$Port, [int]$Status = 201, [string]$ConfigJson)

    $caseDir = Join-Path $work ([guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $caseDir)
    $cfgPath = Join-Path $caseDir 'config.json'
    if ($ConfigJson) { [System.IO.File]::WriteAllText($cfgPath, $ConfigJson) }
    $env:CHIP_REMOTE_CONFIG = $cfgPath
    $env:LOCALAPPDATA = $caseDir
    $stdoutPath = Join-Path $caseDir 'stdout.txt'

    $listener = $null
    $ctxTask = $null
    if ($Port -gt 0) {
        $listener = New-Object System.Net.HttpListener
        $listener.Prefixes.Add("http://localhost:$Port/")
        $listener.Start()
        $ctxTask = $listener.GetContextAsync()
    }
    try {
        $proc = Start-Process -FilePath 'powershell.exe' -PassThru -NoNewWindow `
            -RedirectStandardInput (Join-Path $fixtures $Fixture) `
            -RedirectStandardOutput $stdoutPath `
            -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $hooks $Script))
        # Touch Handle now, otherwise ExitCode is $null after the process exits (known .NET quirk).
        [void]$proc.Handle
        $request = $null
        if ($ctxTask) {
            if ($ctxTask.Wait(20000)) {
                $ctx = $ctxTask.Result
                $reader = New-Object System.IO.StreamReader($ctx.Request.InputStream, (New-Object System.Text.UTF8Encoding($false)))
                $request = @{
                    method      = $ctx.Request.HttpMethod
                    path        = $ctx.Request.Url.AbsolutePath
                    auth        = $ctx.Request.Headers['Authorization']
                    contentType = $ctx.Request.ContentType
                    body        = $reader.ReadToEnd()
                }
                $ctx.Response.StatusCode = $Status
                $ctx.Response.Close()
            }
        }
        if (-not $proc.WaitForExit(30000)) {
            $proc.Kill()
            throw "hook $Script did not exit"
        }
        $proc.WaitForExit()
        $logPath = Join-Path $caseDir 'chip-remote\hook.log'
        $log = ''
        if (Test-Path -LiteralPath $logPath) { $log = Get-Content -LiteralPath $logPath -Raw -Encoding UTF8 }
        $stdout = ''
        if (Test-Path -LiteralPath $stdoutPath) { $stdout = [string](Get-Content -LiteralPath $stdoutPath -Raw) }
        return @{ exit = $proc.ExitCode; stdout = $stdout; request = $request; log = [string]$log }
    } finally {
        if ($listener) { $listener.Close() }
    }
}

function Test-CleanExit {
    param($Result)
    Assert-Equal 0 $Result.exit 'exit code'
    Assert-Equal '' ([string]$Result.stdout).Trim() 'stdout must be empty'
}

$port = Get-FreePort
$config = '{"url":"http://localhost:' + $port + '/","token":"tok_secret_123","host":"test-host"}'

Write-Host '== post-spawn-task.ps1'
$cases = @(
    @{ fixture = 'spawn-string.json'; task = 'task_12d25b98'; cwd = '/home/claude/work/chip-remote'; session = '3f1c2b9e-0d7a-4e51-9a60-1b2c3d4e5f60' },
    @{ fixture = 'spawn-content.json'; task = 'task_0a1b2c3d'; cwd = '/home/claude/work/other-repo'; session = '3f1c2b9e-0d7a-4e51-9a60-1b2c3d4e5f60' },
    @{ fixture = 'spawn-array.json'; task = 'task_ffee0011'; cwd = '/home/claude/work/chip-remote'; session = $null }
)
foreach ($c in $cases) {
    $script:Case = 'spawn ' + $c.fixture
    $fx = Read-Fixture $c.fixture
    $r = Invoke-Hook -Script 'post-spawn-task.ps1' -Fixture $c.fixture -Port $port -ConfigJson $config
    Test-CleanExit $r
    if (-not $r.request) {
        Assert-True $false ('no request received; log: ' + $r.log)
        continue
    }
    Assert-Equal 'POST' $r.request.method 'method'
    Assert-Equal '/v1/chips' $r.request.path 'path'
    Assert-Equal 'Bearer tok_secret_123' $r.request.auth 'authorization'
    Assert-Equal 'application/json; charset=utf-8' $r.request.contentType 'content-type'
    $body = $r.request.body | ConvertFrom-Json
    Assert-Equal $c.task $body.task_id 'body.task_id'
    Assert-Equal $fx.tool_input.title $body.title 'body.title (Japanese preserved)'
    Assert-Equal $fx.tool_input.tldr $body.tldr 'body.tldr'
    Assert-Equal $c.cwd $body.cwd 'body.cwd'
    Assert-Equal 'test-host' $body.host 'body.host'
    Assert-Equal $c.session $body.session_id 'body.session_id'
    Assert-Equal 'task_id,title,tldr,cwd,host,session_id' (($body.PSObject.Properties | ForEach-Object Name) -join ',') 'body keys'
    Assert-Equal '' $r.log 'log must be empty'
}

$script:Case = 'spawn without task_id'
$r = Invoke-Hook -Script 'post-spawn-task.ps1' -Fixture 'spawn-no-id.json' -ConfigJson $config
Test-CleanExit $r
Assert-True ($r.log -like '*task_id not found*') ('log entry; got: ' + $r.log)

$script:Case = 'spawn without config'
$r = Invoke-Hook -Script 'post-spawn-task.ps1' -Fixture 'spawn-string.json'
Test-CleanExit $r
Assert-True ($r.log -like '*config not found*') ('log entry; got: ' + $r.log)

$script:Case = 'spawn with server down'
$deadPort = Get-FreePort
$r = Invoke-Hook -Script 'post-spawn-task.ps1' -Fixture 'spawn-string.json' `
    -ConfigJson ('{"url":"http://localhost:' + $deadPort + '","token":"t"}')
Test-CleanExit $r
Assert-True ($r.log -like '*POST http://localhost:*failed*') ('log entry; got: ' + $r.log)

$script:Case = 'spawn with http 401'
$r = Invoke-Hook -Script 'post-spawn-task.ps1' -Fixture 'spawn-string.json' -Port $port -Status 401 -ConfigJson $config
Test-CleanExit $r
Assert-True ($r.log -like '*failed*401*') ('log entry; got: ' + $r.log)

Write-Host '== post-dismiss-task.ps1'
$script:Case = 'dismiss'
$r = Invoke-Hook -Script 'post-dismiss-task.ps1' -Fixture 'dismiss.json' -Port $port -Status 204 -ConfigJson $config
Test-CleanExit $r
if ($r.request) {
    Assert-Equal 'DELETE' $r.request.method 'method'
    Assert-Equal '/v1/chips/task_12d25b98' $r.request.path 'path'
    Assert-Equal 'Bearer tok_secret_123' $r.request.auth 'authorization'
    Assert-Equal '' $r.log 'log must be empty'
} else {
    Assert-True $false ('no request received; log: ' + $r.log)
}

$script:Case = 'dismiss with malformed task_id'
$r = Invoke-Hook -Script 'post-dismiss-task.ps1' -Fixture 'dismiss-bad-id.json' -ConfigJson $config
Test-CleanExit $r
Assert-True ($r.log -like '*invalid or missing*') ('log entry; got: ' + $r.log)

Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
Write-Host ''
Write-Host ("passed: {0}, failed: {1}" -f $script:Passed, $script:Failed)
if ($script:Failed -gt 0) { exit 1 }
exit 0
