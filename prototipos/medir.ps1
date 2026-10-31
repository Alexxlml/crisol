# F13.2: measures one prototype. Run it once per executable, with the same services running.
#
#   pwsh prototipos/medir.ps1 -Exe <path to the .exe> -Label slint -Out slint.json
#
# What it does, in order:
#   1. cold start: 5 runs, the time from CreateProcess until the process owns a window;
#   2. memory: the window open, at 30 s and 5 min; then the window closed (it hides in the
#      tray), at 30 s and 5 min. Memory is summed over the whole tree of processes (WebView2
#      included), as the private working set (what Task Manager shows as "Memory") and as the
#      total working set (shared pages counted once per process).
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Label,
    [Parameter(Mandatory)][string]$Out,
    [int]$Runs = 5,
    [int]$ShortSeconds = 30,
    [int]$LongSeconds = 300
)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder t, int max);
    public static string Title(IntPtr h) { var t = new System.Text.StringBuilder(256); GetWindowText(h, t, 256); return t.ToString(); }
    public delegate bool EnumProc(IntPtr h, IntPtr l);
}
"@

function Get-Tree([int]$Root) {
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
    $ids = [System.Collections.Generic.List[int]]::new()
    $ids.Add($Root)
    for ($i = 0; $i -lt $ids.Count; $i++) {
        foreach ($c in ($all | Where-Object ParentProcessId -eq $ids[$i])) {
            if (-not $ids.Contains([int]$c.ProcessId)) { $ids.Add([int]$c.ProcessId) }
        }
    }
    $ids
}

function Get-Memory([int]$Root) {
    $ids = Get-Tree $Root
    $private = 0
    foreach ($id in $ids) {
        $row = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$id" -ErrorAction SilentlyContinue
        if ($row) { $private += $row.WorkingSetPrivate }
    }
    $total = 0
    foreach ($id in $ids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { $total += $p.WorkingSet64 }
    }
    [pscustomobject]@{
        processes = $ids.Count
        private_mb = [math]::Round($private / 1MB, 1)
        working_set_mb = [math]::Round($total / 1MB, 1)
    }
}

# Visible windows titled "Crisol" of the process (not of its children), as handles: the main one.
function Get-VisibleWindows([int]$ProcessId) {
    $found = [System.Collections.Generic.List[IntPtr]]::new()
    $cb = [Win+EnumProc]{
        param($h, $l)
        $owner = 0
        [void][Win]::GetWindowThreadProcessId($h, [ref]$owner)
        if ($owner -eq $ProcessId -and [Win]::IsWindowVisible($h) -and [Win]::Title($h) -eq "Crisol") { $found.Add($h) }
        $true
    }
    [void][Win]::EnumWindows($cb, [IntPtr]::Zero)
    $found.ToArray()
}

# What the close button does: WM_CLOSE to the window titled "Crisol" (the process has other
# windows, hidden ones of the tray and of the single-instance plugin, that are not to be closed).
function Close-Windows([int]$ProcessId) {
    foreach ($h in @(Get-VisibleWindows $ProcessId)) {
        if ([Win]::Title($h) -eq "Crisol") { [void][Win]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
    }
}

function Stop-Tree([int]$Root) {
    foreach ($id in ((Get-Tree $Root) | Sort-Object -Descending)) {
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    }
}

$result = [ordered]@{ label = $Label; exe = $Exe; size_mb = [math]::Round((Get-Item $Exe).Length / 1MB, 2) }

# 1. Cold start (the disk cache is warm after the first run: the first one is kept apart).
$starts = @()
for ($i = 0; $i -lt $Runs; $i++) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $Exe -PassThru
    while ($sw.ElapsedMilliseconds -lt 20000 -and (@(Get-VisibleWindows $p.Id).Count -eq 0)) {
        Start-Sleep -Milliseconds 5
    }
    $starts += $sw.ElapsedMilliseconds
    Start-Sleep -Seconds 2
    Stop-Tree $p.Id
    Start-Sleep -Seconds 2
}
$result.start_ms = $starts
$result.start_mean_ms = [math]::Round(($starts | Measure-Object -Average).Average, 0)
$result.start_mean_without_first_ms = [math]::Round((($starts | Select-Object -Skip 1) | Measure-Object -Average).Average, 0)

# 2. Memory, window open and then hidden in the tray.
$p = Start-Process -FilePath $Exe -PassThru
while ((@(Get-VisibleWindows $p.Id).Count -eq 0)) { Start-Sleep -Milliseconds 50 }
Start-Sleep -Seconds $ShortSeconds
$result.open_30s = Get-Memory $p.Id
Start-Sleep -Seconds ($LongSeconds - $ShortSeconds)
$result.open_5min = Get-Memory $p.Id
Close-Windows $p.Id
Start-Sleep -Seconds 2
$result.close_hides = ((@(Get-VisibleWindows $p.Id).Count -eq 0)) -and -not $p.HasExited
Start-Sleep -Seconds ($ShortSeconds - 2)
$result.hidden_30s = Get-Memory $p.Id
Start-Sleep -Seconds ($LongSeconds - $ShortSeconds)
$result.hidden_5min = Get-Memory $p.Id
Stop-Tree $p.Id

$result | ConvertTo-Json -Depth 4 | Set-Content -Path $Out -Encoding utf8
$result | ConvertTo-Json -Depth 4
