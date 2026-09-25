# ChipUia.psm1 - locate and operate spawn_task chips in Claude desktop via UI Automation.
#
# This file must stay pure ASCII (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI).
# The Japanese UI labels come from config.json (UTF-8) or from the code-point defaults below.
#
# Measured chip structure (Claude desktop 2.7032, see docs/PROTOCOL.md):
#   StatusBar                       <- the chip
#     Text   <marker>               (labels.marker)
#     Text   <title>
#     Group
#       Text <tldr>
#     Button <dismiss>
#     Group  <start>
#       Button <start>              (InvokePattern)
#       Button <more options>

Set-StrictMode -Version 2.0

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

# Default labels as UTF-16 code points so this file stays ASCII.
function ConvertFrom-CodePoint {
    param([int[]]$Codes)
    return (-join ($Codes | ForEach-Object { [char]$_ }))
}
$script:DefaultLabels = @{
    marker  = (ConvertFrom-CodePoint @(0x63A8, 0x5968, 0x30BF, 0x30B9, 0x30AF))
    start   = (ConvertFrom-CodePoint @(0x30EF, 0x30FC, 0x30AF, 0x30C4, 0x30EA, 0x30FC, 0x3067, 0x958B, 0x59CB))
    dismiss = (ConvertFrom-CodePoint @(0x63D0, 0x6848, 0x3092, 0x975E, 0x8868, 0x793A))
}

# Chromium builds its accessibility tree lazily: the first query after a period without
# UIA clients returns only the title bar. Remember when each window was last queried.
$script:LastQuery = @{}
$script:WarmupDelayMs = 1500
$script:WarmupIdleSec = 30

function Get-ChipRemoteConfigPath {
    [CmdletBinding()]
    param()
    return (Join-Path $env:APPDATA 'chip-remote\config.json')
}

function Read-ChipRemoteConfig {
    <#
    .SYNOPSIS
    Reads config.json (UTF-8). Returns $null when the file does not exist.
    #>
    [CmdletBinding()]
    param([string]$Path = (Get-ChipRemoteConfigPath))
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $raw = Get-Content -LiteralPath $Path -Raw -Encoding UTF8
    return ($raw | ConvertFrom-Json)
}

function Get-ChipLabels {
    <#
    .SYNOPSIS
    Returns @{marker; start; dismiss}: built-in defaults overridden by config.labels.
    #>
    [CmdletBinding()]
    param(
        [object]$Config,
        [string]$ConfigPath
    )
    $labels = @{
        marker  = $script:DefaultLabels.marker
        start   = $script:DefaultLabels.start
        dismiss = $script:DefaultLabels.dismiss
    }
    if (-not $Config -and $ConfigPath) { $Config = Read-ChipRemoteConfig -Path $ConfigPath }
    if ($Config -and ($Config.PSObject.Properties.Name -contains 'labels') -and $Config.labels) {
        foreach ($k in @('marker', 'start', 'dismiss')) {
            if ($Config.labels.PSObject.Properties.Name -contains $k) {
                $v = [string]$Config.labels.$k
                if ($v) { $labels[$k] = $v }
            }
        }
    }
    return $labels
}

function Get-ClaudeWindow {
    <#
    .SYNOPSIS
    Returns the AutomationElement of Claude desktop's main window, or $null.
    The Claude Code CLI is also named claude.exe but has no main window.
    #>
    [CmdletBinding()]
    param([string]$ProcessName = 'claude')
    $procs = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne [IntPtr]::Zero })
    if ($procs.Count -eq 0) { return $null }
    $preferred = @($procs | Where-Object { $_.MainWindowTitle -eq 'Claude' })
    if ($preferred.Count -gt 0) { $procs = $preferred }
    foreach ($p in $procs) {
        try {
            $el = [System.Windows.Automation.AutomationElement]::FromHandle($p.MainWindowHandle)
            if ($el) { return $el }
        } catch {
            Write-Verbose ("FromHandle failed for pid {0}: {1}" -f $p.Id, $_.Exception.Message)
        }
    }
    return $null
}

function ConvertTo-NormalizedText {
    [CmdletBinding()]
    param([AllowNull()][string]$Text)
    if ($null -eq $Text) { return '' }
    return ([regex]::Replace($Text, '\s+', ' ')).Trim()
}

function Format-UiaRect {
    # BoundingRectangle is Rect.Empty (Infinity/-Infinity) for offscreen elements; never cast blindly.
    [CmdletBinding()]
    param($Rect)
    try {
        if ($null -eq $Rect -or $Rect.IsEmpty) { return 'empty' }
        foreach ($v in @($Rect.X, $Rect.Y, $Rect.Width, $Rect.Height)) {
            if ([double]::IsInfinity($v) -or [double]::IsNaN($v)) { return 'empty' }
        }
        return ('{0},{1} {2}x{3}' -f [math]::Round($Rect.X), [math]::Round($Rect.Y), [math]::Round($Rect.Width), [math]::Round($Rect.Height))
    } catch {
        return 'unknown'
    }
}

function Get-TypeCondition {
    param([System.Windows.Automation.ControlType]$Type)
    return New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
}

function Get-ChipButtonElements {
    # The chip's buttons are NOT inside its StatusBar: Claude desktop renders them as the
    # StatusBar's following siblings (Button "dismiss", then Group "start" holding the
    # start split-button). Collect descendants of the StatusBar (in case a future layout
    # nests them) plus following siblings while they are a Button or the start Group.
    param(
        [System.Windows.Automation.AutomationElement]$Element,
        [hashtable]$Labels
    )
    $ButtonType = [System.Windows.Automation.ControlType]::Button
    $GroupType = [System.Windows.Automation.ControlType]::Group
    $list = New-Object System.Collections.ArrayList
    foreach ($b in $Element.FindAll([System.Windows.Automation.TreeScope]::Descendants, (Get-TypeCondition $ButtonType))) {
        [void]$list.Add($b)
    }
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $sib = $walker.GetNextSibling($Element)
    while ($sib) {
        $ct = $sib.Current.ControlType
        if ($ct -eq $ButtonType) {
            [void]$list.Add($sib)
        } elseif ($ct -eq $GroupType -and [string]$sib.Current.Name -eq $Labels.start) {
            foreach ($b in $sib.FindAll([System.Windows.Automation.TreeScope]::Descendants, (Get-TypeCondition $ButtonType))) {
                [void]$list.Add($b)
            }
        } else {
            break
        }
        $sib = $walker.GetNextSibling($sib)
    }
    return $list.ToArray()
}

function Read-ChipInfo {
    # Returns a chip object for a StatusBar element, or $null when it is not a chip.
    param(
        [System.Windows.Automation.AutomationElement]$Element,
        [hashtable]$Labels
    )
    $TextType = [System.Windows.Automation.ControlType]::Text
    $GroupType = [System.Windows.Automation.ControlType]::Group
    $ButtonType = [System.Windows.Automation.ControlType]::Button
    $children = $Element.FindAll([System.Windows.Automation.TreeScope]::Children,
        [System.Windows.Automation.Condition]::TrueCondition)

    $seenMarker = $false
    $title = $null
    $tldr = $null
    foreach ($c in $children) {
        $ct = $c.Current.ControlType
        $name = [string]$c.Current.Name
        if ($ct -eq $TextType) {
            if (-not $seenMarker) {
                if ($name -eq $Labels.marker) { $seenMarker = $true }
            } elseif ($null -eq $title -and $name) {
                $title = $name
            }
        } elseif ($ct -eq $GroupType -and $seenMarker -and $null -eq $tldr) {
            # The tldr group holds text only; the start split-button group holds buttons.
            $btn = $c.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (Get-TypeCondition $ButtonType))
            if (-not $btn) {
                $texts = $c.FindAll([System.Windows.Automation.TreeScope]::Descendants, (Get-TypeCondition $TextType))
                $parts = @(foreach ($t in $texts) { [string]$t.Current.Name })
                if ($parts.Count -eq 0 -and $c.Current.Name) { $parts = @([string]$c.Current.Name) }
                $tldr = ($parts -join '')
            }
        }
    }
    if (-not $seenMarker) { return $null }

    $buttons = @(Get-ChipButtonElements -Element $Element -Labels $Labels)
    $buttonNames = @(foreach ($b in $buttons) { [string]$b.Current.Name })

    return [pscustomobject]@{
        Element    = $Element
        Title      = [string]$title
        Tldr       = [string]$tldr
        Buttons    = $buttonNames
        IsOffscreen = [bool]$Element.Current.IsOffscreen
        Rect       = (Format-UiaRect $Element.Current.BoundingRectangle)
    }
}

function Read-ChipListCore {
    param(
        [System.Windows.Automation.AutomationElement]$Window,
        [hashtable]$Labels
    )
    $bars = $Window.FindAll([System.Windows.Automation.TreeScope]::Descendants,
        (Get-TypeCondition ([System.Windows.Automation.ControlType]::StatusBar)))
    $list = New-Object System.Collections.ArrayList
    foreach ($bar in $bars) {
        try {
            $info = Read-ChipInfo -Element $bar -Labels $Labels
            if ($info) { [void]$list.Add($info) }
        } catch {
            # Element vanished while we were reading it (ElementNotAvailableException).
            Write-Verbose ("skip status bar: {0}" -f $_.Exception.Message)
        }
    }
    return $list.ToArray()
}

function Get-ChipList {
    <#
    .SYNOPSIS
    Lists every chip currently in Claude desktop's UIA tree.
    Returns objects {Element, Title, Tldr, Buttons, IsOffscreen, Rect}.
    #>
    [CmdletBinding()]
    param(
        [System.Windows.Automation.AutomationElement]$Window,
        [hashtable]$Labels,
        [switch]$NoWarmup
    )
    if (-not $Labels) { $Labels = Get-ChipLabels }
    if (-not $Window) { $Window = Get-ClaudeWindow }
    if (-not $Window) { return }

    $key = 0
    try { $key = [int64]$Window.Current.NativeWindowHandle } catch { $key = 0 }
    $now = Get-Date
    $cold = $true
    if ($script:LastQuery.ContainsKey($key)) {
        $cold = (($now - $script:LastQuery[$key]).TotalSeconds -gt $script:WarmupIdleSec)
    }

    $chips = @(Read-ChipListCore -Window $Window -Labels $Labels)
    if ($chips.Count -eq 0 -and $cold -and -not $NoWarmup) {
        # First query only wakes Chromium's a11y tree up; ask again.
        Start-Sleep -Milliseconds $script:WarmupDelayMs
        $chips = @(Read-ChipListCore -Window $Window -Labels $Labels)
    }
    $script:LastQuery[$key] = Get-Date
    return $chips
}

function Select-Chip {
    <#
    .SYNOPSIS
    Picks the chip matching Title (exact, then whitespace-normalized); Tldr breaks ties.
    Returns the chip object or $null.
    #>
    [CmdletBinding()]
    param(
        [object[]]$Chips,
        [Parameter(Mandatory = $true)][string]$Title,
        [string]$Tldr
    )
    $Chips = @($Chips | Where-Object { $null -ne $_ })
    $cands = @($Chips | Where-Object { $_.Title -ceq $Title })
    if ($cands.Count -eq 0) {
        $nt = ConvertTo-NormalizedText $Title
        $cands = @($Chips | Where-Object { (ConvertTo-NormalizedText $_.Title) -ceq $nt })
    }
    if ($cands.Count -eq 0) { return $null }
    if ($cands.Count -gt 1 -and $Tldr) {
        $exact = @($cands | Where-Object { $_.Tldr -ceq $Tldr })
        if ($exact.Count -gt 0) { return $exact[0] }
        # The tldr group may be split into several text runs; compare without whitespace.
        $squash = [regex]::Replace($Tldr, '\s+', '')
        $loose = @($cands | Where-Object { [regex]::Replace($_.Tldr, '\s+', '') -ceq $squash })
        if ($loose.Count -gt 0) { return $loose[0] }
    }
    return $cands[0]
}

function Find-Chip {
    <#
    .SYNOPSIS
    Returns the chip's StatusBar AutomationElement, or $null (Claude not running or no match).
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Title,
        [string]$Tldr,
        [System.Windows.Automation.AutomationElement]$Window,
        [hashtable]$Labels
    )
    if (-not $Window) { $Window = Get-ClaudeWindow }
    if (-not $Window) { return $null }
    $chips = @(Get-ChipList -Window $Window -Labels $Labels)
    $hit = Select-Chip -Chips $chips -Title $Title -Tldr $Tldr
    if ($hit) { return $hit.Element }
    return $null
}

function Invoke-ChipAction {
    <#
    .SYNOPSIS
    Presses the start or dismiss button inside a chip via InvokePattern.
    Returns @{ok=[bool]; error=<code or $null>; detail=<string or $null>}.
    Error codes: chip_not_found (element went stale), button_not_found, invoke_failed.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][System.Windows.Automation.AutomationElement]$Chip,
        [Parameter(Mandatory = $true)][ValidateSet('start', 'dismiss')][string]$Action,
        [hashtable]$Labels
    )
    if (-not $Labels) { $Labels = Get-ChipLabels }
    $name = $Labels[$Action]
    try {
        $btn = @(Get-ChipButtonElements -Element $Chip -Labels $Labels) |
            Where-Object { [string]$_.Current.Name -eq $name } | Select-Object -First 1
    } catch {
        return @{ ok = $false; error = 'chip_not_found'; detail = $_.Exception.Message }
    }
    if (-not $btn) {
        return @{ ok = $false; error = 'button_not_found'; detail = ('no button named "{0}" in chip' -f $Action) }
    }
    try {
        if (-not $btn.Current.IsEnabled) {
            return @{ ok = $false; error = 'invoke_failed'; detail = 'button is disabled' }
        }
        $pattern = $null
        $ok = $btn.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)
        if (-not $ok -or -not $pattern) {
            return @{ ok = $false; error = 'invoke_failed'; detail = 'InvokePattern not supported' }
        }
        $pattern.Invoke()
    } catch {
        return @{ ok = $false; error = 'invoke_failed'; detail = $_.Exception.Message }
    }
    return @{ ok = $true; error = $null; detail = $null }
}

Export-ModuleMember -Function Get-ChipRemoteConfigPath, Read-ChipRemoteConfig, Get-ChipLabels,
    Get-ClaudeWindow, Get-ChipList, Select-Chip, Find-Chip, Invoke-ChipAction, Format-UiaRect,
    ConvertTo-NormalizedText
