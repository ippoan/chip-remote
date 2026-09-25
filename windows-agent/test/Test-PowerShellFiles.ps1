<#
Static checks for every .ps1 / .psm1 under windows-agent/ and hooks/:
  1. pure ASCII (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI and breaks non-ASCII)
  2. zero parse errors
  3. ChipUia.psm1 imports
  4. PSScriptAnalyzer: zero Error-severity findings (skipped when the module is not installed,
     unless -RequireAnalyzer is given)
Exit code 0 when everything passes.

This file must stay pure ASCII.
#>
[CmdletBinding()]
param(
    [string]$Root,
    [switch]$RequireAnalyzer
)

$ErrorActionPreference = 'Stop'
if (-not $Root) { $Root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent }
$failed = 0

$files = @(foreach ($dir in @('windows-agent', 'hooks')) {
        $p = Join-Path $Root $dir
        if (Test-Path -LiteralPath $p) {
            # -Include is ignored together with -LiteralPath on PS 5.1; filter by extension instead.
            Get-ChildItem -LiteralPath $p -Recurse -File | Where-Object { @('.ps1', '.psm1', '.psd1') -contains $_.Extension.ToLowerInvariant() }
        }
    })
Write-Host ("checking {0} file(s) under {1}" -f $files.Count, $Root)
if ($files.Count -eq 0) { Write-Host 'FAIL: no files found'; exit 1 }

Write-Host '== ASCII'
foreach ($f in $files) {
    $bytes = [System.IO.File]::ReadAllBytes($f.FullName)
    $line = 1
    $bad = @()
    for ($i = 0; $i -lt $bytes.Length; $i++) {
        if ($bytes[$i] -eq 10) { $line++ }
        elseif ($bytes[$i] -gt 127) { $bad += $line; while ($i + 1 -lt $bytes.Length -and $bytes[$i + 1] -gt 127) { $i++ } }
    }
    if ($bad.Count -gt 0) {
        $failed++
        Write-Host ("  FAIL {0}: non-ASCII bytes on line(s) {1}" -f $f.FullName, (($bad | Select-Object -Unique) -join ', '))
    }
}

Write-Host '== parse'
foreach ($f in $files) {
    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors -and $errors.Count -gt 0) {
        $failed++
        foreach ($e in $errors) {
            Write-Host ("  FAIL {0}:{1}: {2}" -f $f.FullName, $e.Extent.StartLineNumber, $e.Message)
        }
    }
}

Write-Host '== import ChipUia.psm1'
try {
    Import-Module (Join-Path $Root 'windows-agent\ChipUia.psm1') -Force
    $labels = Get-ChipLabels
    foreach ($k in @('marker', 'start', 'dismiss')) {
        if (-not $labels[$k]) { throw "default label '$k' is empty" }
    }
    # The code-point defaults must match the UTF-8 example config.
    $example = Get-Content -LiteralPath (Join-Path $Root 'windows-agent\config.example.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($k in @('marker', 'start', 'dismiss')) {
        if ($labels[$k] -cne [string]$example.labels.$k) { throw "default label '$k' differs from config.example.json" }
    }
    Write-Host '  ok'
} catch {
    $failed++
    Write-Host ('  FAIL ' + $_.Exception.Message)
}

Write-Host '== PSScriptAnalyzer (Error)'
if (Get-Module -ListAvailable -Name PSScriptAnalyzer) {
    Import-Module PSScriptAnalyzer
    foreach ($f in $files) {
        $found = @(Invoke-ScriptAnalyzer -Path $f.FullName -Severity Error)
        foreach ($d in $found) {
            $failed++
            Write-Host ("  FAIL {0}:{1}: [{2}] {3}" -f $f.FullName, $d.Line, $d.RuleName, $d.Message)
        }
        $warn = @(Invoke-ScriptAnalyzer -Path $f.FullName -Severity Warning)
        foreach ($d in $warn) {
            Write-Host ("  warn {0}:{1}: [{2}] {3}" -f $f.Name, $d.Line, $d.RuleName, $d.Message)
        }
    }
} elseif ($RequireAnalyzer) {
    $failed++
    Write-Host '  FAIL PSScriptAnalyzer is not installed'
} else {
    Write-Host '  skipped (PSScriptAnalyzer not installed)'
}

Write-Host ''
if ($failed -gt 0) { Write-Host "FAILED: $failed problem(s)"; exit 1 }
Write-Host 'all checks passed'
exit 0
