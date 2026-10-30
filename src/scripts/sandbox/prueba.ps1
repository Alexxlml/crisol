# The whole sandbox test of ADR-0022 in one go, in an ADMINISTRATOR terminal (the pictures need
# it: Defender only shows its exclusions to an administrator). Everything it prints is kept in
# C:\sandbox\out, which the host reads, so a sandbox that closes by itself loses nothing that was
# done. Windows shows its dialogs on the way: the certificate when it is added and when it is
# removed (answer yes to both).

$crisol = Join-Path $env:USERPROFILE 'crisol-bin\crisol.exe'
$out = 'C:\sandbox\out'
$scripts = 'C:\sandbox\scripts'

function Step([string]$file, [string[]]$arguments) {
    Write-Host ("crisol " + ($arguments -join ' '))
    & $crisol @arguments *> (Join-Path $out $file)
    "exit code: $LASTEXITCODE" | Add-Content (Join-Path $out $file)
}

if (-not (Test-Path (Join-Path $out '1-antes\meta.txt'))) {
    Write-Host 'The before picture is not there yet: wait for preparar.ps1 (00-preparar.txt).'
    exit 1
}

# 1. Every change Crisol can make.
Step '10-version.txt' @('--version')
Step '11-system-install.txt' @('system', 'install')
Step '12-path-add.txt' @('path', 'add')
Step '13-autostart-on.txt' @('autostart', 'on')
Step '14-trust.txt' @('trust')
try {
    Add-MpPreference -ExclusionPath 'C:\Crisol\data' -ErrorAction Stop
    'added C:\Crisol\data' | Set-Content (Join-Path $out '15-defender.txt')
} catch {
    "could not add C:\Crisol\data: $_" | Set-Content (Join-Path $out '15-defender.txt')
}
Step '16-doctor.txt' @('doctor')

# 2. The picture with all of it.
& (Join-Path $scripts 'foto-sistema.ps1') -Name '2-instalado'
& (Join-Path $scripts 'comparar-fotos.ps1') -Before '1-antes' -After '2-instalado' *> (Join-Path $out '17-comparar-instalado.txt')

# 3. Taking it all out. The list first (the question and its "yes" are covered by the
# automated tests), then the removal itself, then the list again, which must be empty.
Step '20-lista.json' @('--json', 'uninstall', '--all', '--purge')
Step '21-uninstall.txt' @('uninstall', '--all', '--purge', '--yes')
Step '22-otra-vez.json' @('--json', 'uninstall', '--all')

# 4. The picture after, against the one before.
& (Join-Path $scripts 'foto-sistema.ps1') -Name '3-despues'
& (Join-Path $scripts 'comparar-fotos.ps1') -Before '1-antes' -After '3-despues' *> (Join-Path $out '23-comparar-despues.txt')
Get-Content (Join-Path $out '23-comparar-despues.txt')
Write-Host "`nDone. Everything is in $out."
