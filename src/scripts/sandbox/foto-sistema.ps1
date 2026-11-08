# Takes a picture of everything Crisol can change outside its home (ADR-0022), one text file
# per thing, in C:\sandbox\out\<Name>. Two pictures are compared with comparar-fotos.ps1.
# Defender only shows its exclusions to an administrator: meta.txt says whether this ran as one.

param(
    [Parameter(Mandatory = $true)][string]$Name,
    [string]$Out = 'C:\sandbox\out'
)

$ErrorActionPreference = 'Stop'
$dir = Join-Path $Out $Name
New-Item -ItemType Directory -Force $dir | Out-Null

function Save([string]$file, $lines) {
    @($lines) | ForEach-Object { "$_" } | Set-Content -Encoding utf8 (Join-Path $dir $file)
}

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
Save 'meta.txt' @("taken $(Get-Date -Format o)", "administrator: $admin")

# The user PATH as stored, without expanding %VARIABLES%.
$environment = Get-Item 'HKCU:\Environment'
Save 'user-path.txt' $environment.GetValue('Path', '', 'DoNotExpandEnvironmentNames')

$run = Get-Item 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Save 'run-key.txt' ($run.GetValueNames() | Sort-Object | ForEach-Object { "$_ = $($run.GetValue($_))" })

Save 'root-certs.txt' (Get-ChildItem Cert:\CurrentUser\Root | Sort-Object Thumbprint |
    ForEach-Object { "$($_.Thumbprint) $($_.Subject)" })

Save 'hosts.txt' (Get-Content (Join-Path $env:SystemRoot 'System32\drivers\etc\hosts'))

Save 'nrpt.txt' (Get-DnsClientNrptRule | Sort-Object { "$($_.Namespace)" } |
    ForEach-Object { "$($_.Namespace) -> $($_.NameServers) [$($_.Comment)]" })

try {
    $mp = Get-MpPreference
    Save 'defender.txt' (@($mp.ExclusionPath | Where-Object { $_ } | ForEach-Object { "path $_" }) +
        @($mp.ExclusionProcess | Where-Object { $_ } | ForEach-Object { "process $_" }) | Sort-Object)
} catch {
    Save 'defender.txt' "unavailable: $_"
}

$helper = Join-Path $env:ProgramFiles 'Crisol'
if (Test-Path $helper) {
    Save 'program-files.txt' (Get-ChildItem $helper | ForEach-Object { $_.Name })
} else {
    Save 'program-files.txt' 'absent'
}

# The window (F13.9): its Start menu entry, the profile of its web view, and what winget keeps.
$programs = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
Save 'start-menu.txt' (Get-ChildItem $programs -Filter '*.lnk' -ErrorAction SilentlyContinue |
    Sort-Object Name | ForEach-Object { $_.Name })
Save 'local-appdata.txt' (Get-ChildItem $env:LOCALAPPDATA -Force -ErrorAction SilentlyContinue |
    Where-Object Name -match 'crisol' | ForEach-Object { $_.Name })
$winget = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet'
Save 'winget.txt' (@('Packages', 'Links') | ForEach-Object {
        $kind = $_
        Get-ChildItem (Join-Path $winget $kind) -Force -ErrorAction SilentlyContinue |
            Sort-Object Name | ForEach-Object { "$kind $($_.Name)" }
    })

# The home: a purge must leave it absent.
if (Test-Path 'C:\Crisol') { Save 'home.txt' 'C:\Crisol present' } else { Save 'home.txt' 'C:\Crisol absent' }

Write-Host "Picture '$Name' saved in $dir"
