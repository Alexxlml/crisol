# Runs once when the sandbox starts (crisol.wsb), with no window: copies the Crisol programs to
# a folder only this user can write (the mapped folder is read-only, and Crisol refuses to run
# programs other accounts could replace), waits for Defender (it starts minutes after the
# desktop), adds a Defender exclusion that is not Crisol's (it must survive the uninstall), and
# takes the "before" picture. Its log is C:\sandbox\out\00-preparar.txt.

Start-Transcript -Path 'C:\sandbox\out\00-preparar.txt' -Force | Out-Null
$bin = Join-Path $env:USERPROFILE 'crisol-bin'
New-Item -ItemType Directory -Force $bin | Out-Null
foreach ($name in 'crisol.exe', 'crisold.exe', 'crisol-shim.exe', 'crisol-elevate.exe') {
    Copy-Item (Join-Path 'C:\sandbox\bin' $name) $bin -Force
}
# The steps, where they can be opened and copied from.
Copy-Item 'C:\sandbox\scripts\COMANDOS.txt' ([Environment]::GetFolderPath('Desktop')) -Force

$defender = $false
foreach ($attempt in 1..36) {
    try {
        Get-MpPreference -ErrorAction Stop | Out-Null
        $defender = $true
        break
    } catch {
        Start-Sleep -Seconds 5
    }
}
Write-Host "Defender answers: $defender"

# An exclusion of someone else: uninstalling Crisol must leave it alone.
if ($defender) {
    try {
        New-Item -ItemType Directory -Force 'C:\not-crisol' | Out-Null
        Add-MpPreference -ExclusionPath 'C:\not-crisol' -ErrorAction Stop
        Write-Host 'Added the exclusion C:\not-crisol'
    } catch {
        Write-Host "Could not add the foreign Defender exclusion: $_"
    }
}

& 'C:\sandbox\scripts\foto-sistema.ps1' -Name '1-antes'
Write-Host "Crisol is in $bin. Ready for prueba.ps1."
Stop-Transcript | Out-Null
