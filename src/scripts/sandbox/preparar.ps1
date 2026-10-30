# Runs once when the sandbox starts (crisol.wsb): copies the Crisol programs to a folder only
# this user can write (the mapped folder is read-only, and Crisol refuses to run programs other
# accounts could replace), adds a Defender exclusion that is not Crisol's (it must survive the
# uninstall), and takes the "before" picture of the system.

$ErrorActionPreference = 'Stop'
$bin = Join-Path $env:USERPROFILE 'crisol-bin'
New-Item -ItemType Directory -Force $bin | Out-Null
foreach ($name in 'crisol.exe', 'crisold.exe', 'crisol-shim.exe', 'crisol-elevate.exe') {
    Copy-Item (Join-Path 'C:\sandbox\bin' $name) $bin -Force
}

# An exclusion of someone else: uninstalling Crisol must leave it alone.
try {
    New-Item -ItemType Directory -Force 'C:\not-crisol' | Out-Null
    Add-MpPreference -ExclusionPath 'C:\not-crisol'
} catch {
    Write-Host "Could not add the foreign Defender exclusion: $_"
}

& 'C:\sandbox\scripts\foto-sistema.ps1' -Name '1-antes'
Write-Host "Crisol is in $bin. The 'before' picture is in C:\sandbox\out\1-antes."
