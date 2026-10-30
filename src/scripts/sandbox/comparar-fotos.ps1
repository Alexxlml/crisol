# Compares two pictures taken by foto-sistema.ps1 and says, file by file, whether they are the
# same. Exit code 0 when everything is identical (meta.txt is not compared), 1 otherwise.

param(
    [Parameter(Mandatory = $true)][string]$Before,
    [Parameter(Mandatory = $true)][string]$After,
    [string]$Out = 'C:\sandbox\out'
)

$ErrorActionPreference = 'Stop'
$a = Join-Path $Out $Before
$b = Join-Path $Out $After
$different = 0
foreach ($file in Get-ChildItem $a -Filter '*.txt' | Where-Object Name -ne 'meta.txt') {
    $other = Join-Path $b $file.Name
    if (-not (Test-Path $other)) {
        Write-Host "MISSING   $($file.Name) (not in $After)"
        $different++
        continue
    }
    $diff = Compare-Object (Get-Content $file.FullName) (Get-Content $other)
    if ($diff) {
        Write-Host "DIFFERENT $($file.Name)"
        foreach ($line in $diff) {
            $side = if ($line.SideIndicator -eq '<=') { "  only in $Before :" } else { "  only in $After :" }
            Write-Host "$side $($line.InputObject)"
        }
        $different++
    } else {
        Write-Host "same      $($file.Name)"
    }
}
if ($different -eq 0) {
    Write-Host "`n$Before and $After are identical."
    exit 0
}
Write-Host "`n$different file(s) differ between $Before and $After."
exit 1
