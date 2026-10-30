# Step 2 of the sandbox test (ADR-0022), in a NORMAL terminal: makes every change Crisol can
# make in Windows and keeps what each command printed in C:\sandbox\out, which the host reads.
# Windows shows its own dialogs (the certificate, maybe the UAC prompt): answer yes.

$bin = Join-Path $env:USERPROFILE 'crisol-bin'
$out = 'C:\sandbox\out'
$crisol = Join-Path $bin 'crisol.exe'

function Step([string]$file, [string[]]$arguments) {
    Write-Host "crisol $($arguments -join ' ')"
    & $crisol @arguments *> (Join-Path $out $file)
    "exit code: $LASTEXITCODE" | Add-Content (Join-Path $out $file)
}

Step '10-version.txt' @('--version')
Step '11-system-install.txt' @('system', 'install')
Step '12-path-add.txt' @('path', 'add')
Step '13-autostart-on.txt' @('autostart', 'on')
Step '14-trust.txt' @('trust')
Step '15-doctor.txt' @('doctor')
Write-Host "Done. The outputs are in $out (10 to 15)."
