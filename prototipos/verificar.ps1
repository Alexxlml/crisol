# F13.2: checks what a prototype does against a real crisold (CRISOL_HOME must point at a test
# home where the daemon runs and nginx and php are installed). It drives the window through
# UI Automation, as a person would, and asks `crisol status` what really happened.
#
#   pwsh prototipos/verificar.ps1 -Exe <.exe> -Crisol <crisol.exe>
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Crisol
)

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win2 {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder t, int max);
    public static string Title(IntPtr h) { var t = new System.Text.StringBuilder(256); GetWindowText(h, t, 256); return t.ToString(); }
    public delegate bool EnumProc(IntPtr h, IntPtr l);
}
"@

function Get-Windows([int]$ProcessId, [switch]$Main) {
    $found = [System.Collections.Generic.List[IntPtr]]::new()
    $cb = [Win2+EnumProc]{
        param($h, $l)
        $owner = 0
        [void][Win2]::GetWindowThreadProcessId($h, [ref]$owner)
        if ($owner -eq $ProcessId -and [Win2]::IsWindowVisible($h) -and (-not $Main -or [Win2]::Title($h) -eq "Crisol")) { $found.Add($h) }
        $true
    }
    [void][Win2]::EnumWindows($cb, [IntPtr]::Zero)
    $found.ToArray()
}

function Wait-Until([scriptblock]$Test, [int]$Seconds = 15) {
    $end = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $end) { if (& $Test) { return $true }; Start-Sleep -Milliseconds 200 }
    $false
}

function Get-Status { (& $Crisol status --json 2>$null | ConvertFrom-Json).services | ForEach-Object { "$($_.name)=$($_.state)" } }

function Get-WindowElement([int]$ProcessId) {
    $handles = @(Get-Windows $ProcessId -Main)
    if ($handles.Count -eq 0) { return $null }
    [System.Windows.Automation.AutomationElement]::FromHandle($handles[0])
}

function Get-Names($Element) {
    $all = $Element.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($e in $all) { $e.Current.Name }
}

function Invoke-Button($Element, [string]$Name) {
    $cond = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $b = $Element.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    if (-not $b) { return $false }
    $b.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    $true
}

$report = [ordered]@{}
$report.status_before = (Get-Status) -join ", "
$p = Start-Process -FilePath $Exe -PassThru
$report.window_opens = Wait-Until { @(Get-Windows $p.Id -Main).Count -gt 0 }
# The accessibility tree of a web view is built lazily: wait for the list to show up.
$null = Wait-Until { $w = Get-WindowElement $p.Id; @(Get-Names $w) -contains "nginx" } 15
$win = Get-WindowElement $p.Id
$names = @(Get-Names $win | Where-Object { $_ })
$report.ui_texts = ($names | Select-Object -Unique) -join " | "
$report.list_shows_services = ($names -contains "nginx") -and ($names -contains "php-8.4.27")
$report.shows_daemon = [bool]($names | Where-Object { $_ -like "crisold *" })

# Stop all, then start all, from the window.
$report.clicked_stop_all = Invoke-Button $win "Stop all"
$report.services_stopped = Wait-Until { -not ((Get-Status) -match "running") }
Start-Sleep -Seconds 1
$win = Get-WindowElement $p.Id
$report.button_became_start_all = [bool](Wait-Until { $w = Get-WindowElement $p.Id; (Get-Names $w) -contains "Start all" } 8)
$report.window_shows_stopped = (@(Get-Names $win) -contains "stopped")
$win = Get-WindowElement $p.Id
$report.clicked_start_all = Invoke-Button $win "Start all"
$report.services_running_again = Wait-Until { ((Get-Status) -match "running").Count -ge 2 } 30
$report.status_after = (Get-Status) -join ", "

# Close hides; a second run shows the window again and ends.
foreach ($h in @(Get-Windows $p.Id -Main)) { if ([Win2]::Title($h) -eq "Crisol") { [void][Win2]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) } }
$report.close_hides = Wait-Until { (@(Get-Windows $p.Id -Main).Count -eq 0) } 5
$p.Refresh()
$report.still_running_after_close = -not $p.HasExited
$second = Start-Process -FilePath $Exe -PassThru
$report.second_exits = Wait-Until { $second.Refresh(); $second.HasExited } 10
$report.second_woke_window = Wait-Until { @(Get-Windows $p.Id -Main).Count -gt 0 } 10
$others = @(Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($Exe)) -ErrorAction SilentlyContinue)
$report.instances_running = $others.Count

# Stop the daemon with the tray's last entry is checked by hand; here the process just ends.
$p.CloseMainWindow() | Out-Null
foreach ($proc in (Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($Exe)) -ErrorAction SilentlyContinue)) { Stop-Process -Id $proc.Id -Force }
[pscustomobject]$report | ConvertTo-Json
