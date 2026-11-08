# Prueba manual de F13.7, F13.8 y F13.9 en Windows Sandbox

Une las tres verificaciones pendientes de la Fase 13 (acordado el 2026-11-07):

- **Parte A, el ZIP sin winget y todo desde la ventana:** el asistente en una máquina limpia (diciendo «no» y «sí»), el menú Inicio, los botones de *Diagnóstico* con diálogos de Windows, y la desinstalación desde la ventana, que debe dejar Windows como estaba.
- **Parte B, la prueba de aceptación de F13.9 con winget:** `winget install --manifest` → `crisol setup` → la ventana abre desde el menú Inicio → `uninstall --all` → `winget uninstall` → el sistema queda como al inicio.

Todo lo que se guarda va a `C:\sandbox\out` (en tu equipo, `D:\scratch\sandbox-out`). Si el Sandbox se cierra solo, lo hecho no se pierde. Al terminar, avisa en el chat y yo leo esa carpeta.

Convenciones de este documento:

- **[admin]** es la terminal de administrador: clic derecho en Inicio → *Terminal (administrador)* o *Windows PowerShell (administrador)*. Ahí se toman las fotos, porque Defender solo muestra sus exclusiones a un administrador.
- **[usuario]** es una terminal sin elevar: `Win+R` → `powershell` → Enter. Ahí corren Crisol y winget. En el Sandbox tu cuenta es administrador y el UAC está apagado: nada pide elevación, y lo que la necesita (el ayudante) se hace sin preguntar.
- **Anota** quiere decir: escribe en el chat lo que viste (o pega una captura) con el número del paso.

---

## 0. En tu equipo, antes de abrir el Sandbox (Git Bash)

```bash
cd /c/Users/inkis/dev/Crisol/src && cargo build --release
cd /c/Users/inkis/dev/Crisol

# Carpetas que monta f13.wsb. Mueve a otra parte lo que haya en sandbox-out de pruebas viejas.
mkdir -p /d/scratch/sandbox-out /d/scratch/sandbox-pkg
ls /d/scratch/sandbox-out

# El ZIP real. Si no tienes cargo-about, un LICENSES-RUST.md de relleno basta para esta prueba.
printf '# Rust licenses (placeholder for the sandbox test)\n' > /d/scratch/licencias-prueba.md
CRISOL_LICENSES_FILE=/d/scratch/licencias-prueba.md bash src/scripts/package.sh 0.1.0 /d/scratch/sandbox-pkg

# El manifiesto de winget, validado, y con la descarga apuntando al servidor local del Sandbox
# (todavía no hay release publicado en GitHub).
bash src/scripts/winget-manifest.sh 0.1.0 /d/scratch/sandbox-pkg/SHA256SUMS /d/scratch/sandbox-pkg/winget
sed -i 's#https://github.com/Alexxlml/crisol/releases/download/v0.1.0/#http://localhost:8765/#' \
  /d/scratch/sandbox-pkg/winget/0.1.0/Alexxlml.Crisol.installer.yaml
grep InstallerUrl /d/scratch/sandbox-pkg/winget/0.1.0/Alexxlml.Crisol.installer.yaml

# Lo que trae el ZIP: los cinco .exe, licenses/ y los textos.
/c/Windows/System32/tar.exe -tf "$(cygpath -w /d/scratch/sandbox-pkg/crisol-0.1.0-windows-x64.zip)"
```

Esperado: `InstallerUrl: http://localhost:8765/crisol-0.1.0-windows-x64.zip`, y en la lista del ZIP `crisol.exe`, `crisold.exe`, `crisol-shim.exe`, `crisol-elevate.exe`, `crisol-gui.exe`, `licenses/` con 4 archivos, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-NOTICES.md`, `LEEME.txt` y `LICENSES-RUST.md`.

Abre `src\scripts\sandbox\f13.wsb` (doble clic). Tiene red: winget se instala dentro.

---

## 1. Al arrancar el Sandbox

1. Espera a que exista `C:\sandbox\out\1-antes\meta.txt`. `preparar.ps1` corre solo, sin ventana: copia los cinco programas a `%USERPROFILE%\crisol-bin`, espera a Defender (hasta 3 min), añade la exclusión ajena `C:\not-crisol` y toma la foto `1-antes`. También deja este documento en el Escritorio.
2. **[admin]** Comprueba que la foto tiene lo nuevo:

   ```powershell
   Get-ChildItem C:\sandbox\out\1-antes
   Get-Content C:\sandbox\out\00-preparar.txt -Tail 5
   ```

   Deben estar `start-menu.txt`, `local-appdata.txt`, `winget.txt` y `home.txt`, y `meta.txt` debe decir `administrator: True`.

---

## Parte A: el ZIP, todo desde la ventana

### A1. Primer arranque: el asistente se abre solo

`Win+R` → `%USERPROFILE%\crisol-bin\crisol-gui.exe` → Enter.

Esperado:

- La ventana abre (WebView2 del Sandbox) en **Instalación**, sin que la pidas.
- Pasos en este orden: comandos (`shims`), `PATH`, **menú Inicio** (nuevo), CA local, ayudante, inicio al abrir sesión.
- Interruptores: la CA y el ayudante **encendidos**; el inicio al abrir sesión **apagado**. El menú Inicio no tiene interruptor (no es opcional).
- El texto del menú Inicio nombra `...\Start Menu\Programs\Crisol.lnk` y `...\crisol-bin\crisol-gui.exe`.

Al abrirse, la ventana ya creó `C:\Crisol` (guarda que el asistente se abrió). **[admin]** comprueba que la creó privada:

```powershell
icacls C:\Crisol *>&1 | Tee-Object C:\sandbox\out\A1-home-permisos.txt
```

Esperado: **solo tres** líneas de permisos, sin `(I)`: `WDAGUtilityAccount`, `NT AUTHORITY\SYSTEM` y `BUILTIN\Administradores` (o `Administrators`), con `(OI)(CI)(F)`. **No debe** salir `Authenticated Users` ni `Usuarios`/`Users`. Si sale, detente y avisa: es el fallo del primer intento.

**Anota** si algo no cuadra.

### A2. Configurar diciendo «no» al certificado

En el Sandbox tu cuenta es administrador y **el UAC está apagado**: Windows no pregunta nada para instalar el ayudante, lo instala directamente. El «no» al UAC no se puede probar aquí; queda para tu equipo.

Deja los interruptores como vienen y pulsa *Configurar*.

- En el diálogo de Windows del certificado responde **No**. Ese diálogo sí sale: es de Windows para el almacén de certificados del usuario, no del UAC.

Esperado: comandos, `PATH` y menú Inicio **hechos**; CA **rechazada** («no se cambió nada», sin error); ayudante **hecho** sin preguntar; el inicio al abrir sesión no se toca. **No debe quedar** el aviso «Crisol no está del todo configurado» en las demás pantallas (solo falta un paso opcional).

**[admin]**

```powershell
powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\foto-y-comparar.ps1 -Name 2a-asistente-no
```

Esperado como Crisol: `user-path.txt` (la carpeta `shims`), `start-menu.txt` (`Crisol.lnk`), `local-appdata.txt` (`dev.crisol.gui`), `home.txt` (`C:\Crisol present`), y por el ayudante `program-files.txt` y `nrpt.txt`. **Sin cambios** en `root-certs.txt` ni `run-key.txt`.

### A3. El menú Inicio

1. Abre Inicio, escribe `Crisol`. Debe salir **Crisol** con el icono de la aplicación. Ábrelo: con la ventana ya abierta, solo la trae al frente (instancia única).
2. Clic derecho en la entrada → *Abrir ubicación del archivo* → clic derecho en `Crisol.lnk` → *Propiedades*. **Destino:** `C:\Users\WDAGUtilityAccount\crisol-bin\crisol-gui.exe`; **Iniciar en:** esa carpeta.
3. Cierra la app del todo: icono de la bandeja → *Salir* (acepta el aviso). Vuelve a abrirla **desde el menú Inicio**. Esperado: abre en la pantalla de siempre, no en *Instalación* (el asistente se abre solo una vez).
4. **[usuario]** `& "$env:USERPROFILE\crisol-bin\crisol.exe" setup` y responde **n** a todo. Esperado: el menú Inicio sale en lo ya hecho («Crisol is in your Start menu») y no como paso.

**Anota** lo que veas en 2 (destino) y 3.

### A4. Diagnóstico: confiar en la CA y el ayudante, primero «no» y después «sí» (F13.7)

En la ventana, pantalla **Diagnóstico**.

1. Fila de la CA local → *Confiar*. El segundo clic muestra nombre, uso, vigencia, SHA-256 y **SHA-1**. Confirma.
   - En el diálogo de Windows, compara la huella (SHA-1) con la de la pantalla y responde **No**. Esperado: «no se cambió nada», sin error.
   - Repite y responde **Sí**. Esperado: la fila pasa a bien.
2. El ayudante ya quedó instalado en A2, así que su fila debe estar bien y sin botón. El «no» al UAC no se puede probar en el Sandbox (no hay UAC); queda para tu equipo.

**Anota** si la SHA-1 coincide.

### A5. Quitar CA viejas (F13.7)

Para tener una CA vieja confiada, se fuerza una CA nueva:

**[usuario]**

```powershell
& "$env:USERPROFILE\crisol-bin\crisol.exe" stop
Remove-Item C:\Crisol\certs\ca.pem, C:\Crisol\certs\ca.key
```

En la ventana, **Diagnóstico** → *Confiar* en la CA (se crea una nueva) → Sí. Ahora debe salir una fila con la CA anterior y el botón *Quitar las CA viejas* → segundo clic → diálogo de Windows → **Sí**.

Esperado: queda **una** sola `Crisol Local CA` confiada.

**[admin]** `Get-ChildItem Cert:\CurrentUser\Root | Where-Object Subject -match Crisol | Format-List Subject, Thumbprint`

### A6. Añadir Crisol al PATH (F13.7)

**[usuario]** `& "$env:USERPROFILE\crisol-bin\crisol.exe" path remove`

**Diagnóstico** → *Volver a comprobar* → fila `PATH` → *Añadir Crisol al PATH*. Esperado: hecho.

**[usuario]** `& "$env:USERPROFILE\crisol-bin\crisol.exe" path status`: los shims están primero.

### A7. Desactivar el inicio al abrir sesión (F13.7)

Una entrada rota del inicio:

**[usuario]** `reg add HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v Crisol /d "C:\no-existe\crisold.exe" /f`

**Diagnóstico** → *Volver a comprobar* → fila `autostart` → *Desactivar el inicio al abrir sesión*. Esperado: hecho.

**[usuario]** `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v Crisol` debe decir que no encuentra el valor.

### A8. Mostrar en el Explorador (F13.7)

**[usuario]**

```powershell
$toml = 'C:\Crisol\crisol.toml'
$existed = Test-Path $toml
if ($existed) { Copy-Item $toml "$env:TEMP\crisol.toml.bak" }
Add-Content $toml 'esto no es toml ['
```

**Diagnóstico** → *Volver a comprobar* → fila `configuration` → *Mostrar en el Explorador*. Esperado: abre el Explorador en `C:\Crisol` con `crisol.toml` **seleccionado**.

Deja el archivo como estaba (en la misma terminal):

```powershell
if ($existed) { Copy-Item "$env:TEMP\crisol.toml.bak" $toml -Force } else { Remove-Item $toml }
```

### A9. La página del runtime de Visual C++ (F13.7, si sale la fila)

El Sandbox no trae el runtime de Visual C++. Si **Diagnóstico** muestra la fila `Visual C++ runtime`, su botón debe abrir el navegador en la página de Microsoft (`aka.ms`). Si la fila no sale, **anótalo** y sigue.

### A10. Foto con todo instalado

**[admin]**

```powershell
powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\foto-y-comparar.ps1 -Name 2-instalado
```

Esperado como Crisol: `user-path`, `root-certs` (una `Crisol Local CA`), `nrpt`, `program-files` (el ayudante), `start-menu`, `local-appdata` y `home`. `run-key` igual que al inicio.

### A11. Desinstalar desde la ventana (F13.8 y F13.9)

1. Ventana → **Instalación** → *Desinstalar*. Marca **borrar también la carpeta de Crisol** (`--purge`). El botón queda apagado hasta marcar «tengo copia». Márcalo.
2. Antes de confirmar, revisa la lista. Esperado, entre otros pasos:
   - borrar la entrada del menú Inicio (`Crisol.lnk`);
   - borrar lo que guarda la vista web: `C:\Users\WDAGUtilityAccount\AppData\Local\dev.crisol.gui`;
   - borrar los programas: **cinco**, incluido `crisol-gui.exe`.
3. Confirma con el segundo clic. Diálogo del certificado → **Sí** (el ayudante se quita sin UAC).
4. Esperado: todo hecho, y la app **se cierra sola** a los 5 s.
5. Espera **30 s** y en **[admin]**:

   ```powershell
   Get-ChildItem "$env:USERPROFILE\crisol-bin"
   Test-Path "$env:LOCALAPPDATA\dev.crisol.gui"
   Get-Process crisol*, msedgewebview2 -ErrorAction SilentlyContinue | Format-Table Name, Id, Path
   powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\foto-y-comparar.ps1 -Name 3-despues-gui
   ```

   Esperado: `crisol-bin` vacía, `False`, ningún proceso, y al final **`Nothing of Crisol differs between 1-antes and 3-despues-gui.`** (`C:\not-crisol` sigue en `defender.txt`).

   **Si `dev.crisol.gui` sigue ahí,** guarda esto antes de seguir (es el riesgo de tiempos del borrado programado):

   ```powershell
   Get-ChildItem "$env:LOCALAPPDATA\dev.crisol.gui" -Recurse -Force | Select-Object -First 30 FullName |
     Out-File C:\sandbox\out\A11-perfil-que-quedo.txt
   ```

---

## Parte B: winget (aceptación de F13.9)

### B1. Instalar winget en el Sandbox

**[admin]**

```powershell
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'
Install-PackageProvider -Name NuGet -Force | Out-Null
Install-Module -Name Microsoft.WinGet.Client -Force -Repository PSGallery | Out-Null
Repair-WinGetPackageManager -AllUsers
```

Cierra esa terminal y abre otra **[admin]**:

```powershell
winget --version
winget settings --enable LocalManifestFiles
```

### B2. La foto de antes de winget

**[admin]**

```powershell
powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\foto-sistema.ps1 -Name 4-antes-winget
```

### B3. El servidor del ZIP

En una terminal **[usuario]** aparte, que se queda abierta:

```powershell
powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\servir-zip.ps1
```

### B4. Instalar con el manifiesto

En otra **[usuario]**:

```powershell
winget install --manifest C:\sandbox\pkg\winget\0.1.0 --scope user --accept-package-agreements --accept-source-agreements |
  Tee-Object C:\sandbox\out\40-winget-install.txt
```

Esperado: el servidor imprime `200 crisol-0.1.0-windows-x64.zip`, el hash cuadra y winget dice que añadió el alias `crisol`.

**Si winget rechaza la descarga por `http`** o por otra razón, el error queda en `40-winget-install.txt`: detente ahí y avisa.

### B5. Lo que dejó winget

Abre una terminal **[usuario] nueva** (para que tome el `PATH`):

```powershell
& {
  crisol --version
  (Get-Command crisol).Source
  Get-Item "$env:LOCALAPPDATA\Microsoft\WinGet\Links\crisol.exe" | Format-List FullName, LinkType, Target
  Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Filter 'Alexxlml.Crisol*' |
    ForEach-Object { $_.FullName; Get-ChildItem $_.FullName -Recurse | Select-Object -ExpandProperty Name }
} *>&1 | Tee-Object C:\sandbox\out\41-winget-carpeta.txt
```

Esperado: `crisol 0.1.0`, el enlace en `WinGet\Links`, y en la carpeta del paquete los **cinco** `.exe` juntos, sin versión en la ruta (es lo que la ADR-0024 tiene como «sin verificar»).

### B6. `crisol setup` desde la terminal

**[usuario]**

```powershell
crisol setup
```

Responde: CA → **s** (diálogo de Windows → Sí), ayudante → **s** (sin UAC en el Sandbox), inicio al abrir sesión → **n**.

Esperado: el paso del menú Inicio apunta a `...\WinGet\Packages\Alexxlml.Crisol_...\crisol-gui.exe` y sale hecho. Después:

```powershell
crisol setup *>&1 | Tee-Object C:\sandbox\out\42-setup-otra-vez.txt
```

Esperado: nada que hacer salvo el inicio al abrir sesión, y «Crisol is in your Start menu» entre lo hecho.

### B7. La ventana desde el menú Inicio

Inicio → `Crisol` → abre la ventana. Comprueba en las propiedades del acceso directo que el destino es el `crisol-gui.exe` de la carpeta de winget. La pantalla de servicios debe conectar con el daemon. No debe salir el aviso «no está del todo configurado».

### B8. Desinstalar con la ventana abierta en la bandeja (hallazgo 1 de la revisión, corregido)

Deja la ventana abierta (o en la bandeja). **[usuario]**

```powershell
crisol --json uninstall --all --purge | Out-File C:\sandbox\out\43-lista.json
crisol uninstall --all --purge --yes *>&1 | Tee-Object C:\sandbox\out\44-uninstall-gui-abierta.txt
```

Diálogo del certificado → Sí (el ayudante se quita sin UAC). Espera **30 s**:

```powershell
& {
  "perfil: $(Test-Path "$env:LOCALAPPDATA\dev.crisol.gui")"
  "home: $(Test-Path C:\Crisol)"
  Get-Process crisol* -ErrorAction SilentlyContinue | Format-Table Name, Id, Path
} *>&1 | Tee-Object C:\sandbox\out\45-tras-uninstall.txt
```

Esperado:

- en `43-lista.json`, el primer paso es `"step": "window"` (*Close the Crisol window…*);
- en `44-…`, la primera línea del resultado dice *Closed the Crisol window*, y el icono desaparece de la bandeja antes de que Windows pregunte por el certificado;
- en `45-…`, `perfil: False`, `home: False` y ningún `crisol-gui` en la lista (los programas siguen: son de winget).

Si el perfil sigue ahí, guarda qué procesos de la vista web quedaban:

```powershell
Get-Process msedgewebview2 -ErrorAction SilentlyContinue | Format-Table Id, Path *>&1 | Tee-Object C:\sandbox\out\46-perfil-sigue.txt
```

### B9. `winget uninstall` y la foto final

Cierra la terminal del servidor (Ctrl+C). **[usuario]**

```powershell
winget uninstall Alexxlml.Crisol *>&1 | Tee-Object C:\sandbox\out\47-winget-uninstall.txt
```

**[admin]**

```powershell
powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\foto-sistema.ps1 -Name 5-despues-winget
powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\comparar-fotos.ps1 -Before 4-antes-winget -After 5-despues-winget |
  Tee-Object C:\sandbox\out\48-comparar-winget.txt
powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\comparar-fotos.ps1 -Before 1-antes -After 5-despues-winget |
  Tee-Object C:\sandbox\out\49-comparar-todo.txt
```

Esperado en los dos: **`Nothing of Crisol differs ...`**. Las diferencias marcadas `windows` (certificados raíz que Windows descargó, el `PATH` de `WinGet\Links`) no cuentan; **anótalas** de todos modos.

---

## Al terminar

Avisa en el chat. Leo `D:\scratch\sandbox-out` y lo que anotaste paso por paso.
