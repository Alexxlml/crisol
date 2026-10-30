# Step 4 of the sandbox test (ADR-0022), in a NORMAL terminal: `uninstall --all --purge`, with
# what it prints kept in C:\sandbox\out. The list it shows before asking is saved first (the
# question itself and the "yes" are covered by the automated tests), then it runs with --yes so
# its output can be kept. Windows asks about each certificate and maybe shows the UAC prompt:
# answer yes. Last, it runs again: there must be nothing left to undo.

$crisol = Join-Path $env:USERPROFILE 'crisol-bin\crisol.exe'
$out = 'C:\sandbox\out'

function Step([string]$file, [string[]]$arguments) {
    Write-Host "crisol $($arguments -join ' ')"
    & $crisol @arguments *> (Join-Path $out $file)
    "exit code: $LASTEXITCODE" | Add-Content (Join-Path $out $file)
}

Step '20-lista.json' @('--json', 'uninstall', '--all', '--purge')
Step '21-uninstall.txt' @('uninstall', '--all', '--purge', '--yes')
Step '22-otra-vez.json' @('--json', 'uninstall', '--all')
Write-Host "Done. The outputs are in $out (20 to 22)."
