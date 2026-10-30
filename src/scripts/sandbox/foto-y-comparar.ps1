# In the ADMINISTRATOR terminal: takes the picture <Name> and compares it with 1-antes, keeping
# the comparison in C:\sandbox\out\comparar-<Name>.txt as well as showing it.

param([Parameter(Mandatory = $true)][string]$Name)

& 'C:\sandbox\scripts\foto-sistema.ps1' -Name $Name
& 'C:\sandbox\scripts\comparar-fotos.ps1' -Before '1-antes' -After $Name |
    Tee-Object -FilePath "C:\sandbox\out\comparar-$Name.txt"
