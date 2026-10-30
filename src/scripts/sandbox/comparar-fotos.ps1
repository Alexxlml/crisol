# Compares two pictures taken by foto-sistema.ps1, file by file (meta.txt is not compared).
#
# Windows changes some of the same things by itself while the sandbox runs (a sign-in entry of
# Edge, root certificates it downloads, Defender that starts answering minutes after the
# desktop), so every difference is shown, and split in two: the lines that are Crisol's (they
# name Crisol: its folder, its CA, its NRPT comment, its helper, its sign-in entry) and the rest.
# Exit code 0 when no difference is Crisol's, 1 otherwise.

param(
    [Parameter(Mandatory = $true)][string]$Before,
    [Parameter(Mandatory = $true)][string]$After,
    [string]$Out = 'C:\sandbox\out'
)

$ErrorActionPreference = 'Stop'
$a = Join-Path $Out $Before
$b = Join-Path $Out $After
$ours = 0
$theirs = 0
foreach ($file in Get-ChildItem $a -Filter '*.txt' | Where-Object Name -ne 'meta.txt') {
    $other = Join-Path $b $file.Name
    if (-not (Test-Path $other)) {
        Write-Host "MISSING   $($file.Name) (not in $After)"
        $ours++
        continue
    }
    $diff = @(Compare-Object @(Get-Content $file.FullName) @(Get-Content $other))
    if ($diff.Count -eq 0) {
        Write-Host "same      $($file.Name)"
        continue
    }
    Write-Host "DIFFERENT $($file.Name)"
    foreach ($line in $diff) {
        $side = if ($line.SideIndicator -eq '<=') { "only in $Before" } else { "only in $After" }
        if ("$($line.InputObject)" -match 'crisol') {
            Write-Host "  CRISOL   ${side}: $($line.InputObject)"
            $ours++
        } else {
            Write-Host "  windows  ${side}: $($line.InputObject)"
            $theirs++
        }
    }
}
Write-Host ''
if ($theirs -gt 0) {
    Write-Host "$theirs difference(s) are not Crisol's: Windows changed them by itself."
}
if ($ours -eq 0) {
    Write-Host "Nothing of Crisol differs between $Before and $After."
    exit 0
}
Write-Host "$ours difference(s) are Crisol's between $Before and $After."
exit 1
