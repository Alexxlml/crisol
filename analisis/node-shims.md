# Shims de Node: lo que se midió y por qué se lanza el script y no el `.cmd`

Fecha: 2026-10-17 (F10.2). Rust 1.99.0, Windows 11, Node 20.20.2 y 24.22.0 oficiales.

## 1. Los archivos por lotes (`npm.cmd`, `npx.cmd`) y el endurecimiento de Rust

Pendiente desde la Fase 4: qué hace la biblioteca estándar de Rust con los argumentos cuando el programa es un archivo por lotes. Desde Rust 1.77.2 (`CVE-2024-24576`) cita los argumentos con las reglas de `cmd.exe` y rechaza los que no puede hacer seguros.

Prueba: `crisol-shim/tests/batch_args.rs`. Un `.cmd` de prueba (`@"echo_args.exe" %*`) recibe un argumento por vez; `echo_args.exe` imprime lo que le llegó.

| Argumento | Resultado |
|---|---|
| `plain`, `with space`, `trailing\` | llega igual |
| `50%`, `%PATH%`, `%%` | llega **sin expandir** (Rust rompe la expansión de `%`) |
| `a^b`, `^` | llega igual |
| `say "hi"`, `q"` | llega igual |
| `a&b`, `a|b`, `<x>`, `!bang!` | llega igual |
| un salto de línea (`\n`) o un retorno de carro (`\r`) | **Rust no lanza el proceso**: `io::ErrorKind::InvalidInput`, «batch file arguments are invalid» |

Conclusión: con Rust actual, `%`, `^`, `"` y espacios son seguros incluso por un `.cmd`. Lo único que se rechaza es el salto de línea, y el error de Rust («batch file arguments are invalid») no dice qué argumento ni por qué.

## 2. Por qué el shim no pasa por el `.cmd`

`npm.cmd` hace tres cosas: abre `cmd.exe`, ejecuta `node npm-prefix.js` (otro `node`, solo para preguntar dónde está la carpeta global) y por fin `node npm-cli.js %*`. Medido (mediana de 10 ejecuciones de `npm --version`, Node 24.22.0, sin el shim):

| Cómo se lanza | Tiempo |
|---|---|
| `npm.cmd --version` | 172 ms |
| `node.exe npm-cli.js --version` | 85 ms |

Y con el `.cmd` el salto de línea no pasa. Por eso `crisol_core::nodetool` arma `node.exe <script> <argumentos>` para `npm`, `npx`, `corepack`, `pnpm`, `pnpx`, `yarn` y `yarnpkg`, sin `cmd.exe`. Con los Node oficiales reales (`real_node_runs_through_the_shims`): `%`, `^`, `"`, espacios, `&`, `|`, una cadena vacía y un salto de línea llegan intactos a `node`, y el código de salida vuelve.

**Lo que se pierde al no usar `npm.cmd`:** `npm-prefix.js` permite que `npm install -g npm` (una copia nueva de npm en el prefijo global) gane sobre el npm del ZIP. El shim lo reproduce en F10.3 mirando `<prefijo>/node_modules/npm` (ver `docs/decisiones/0018-shims-de-node.md`).

**Ctrl+C:** el shim sigue sobreviviendo a Ctrl+C y deja que lo reciba el hijo (F4.2). Sin `cmd.exe` de por medio no sale el «¿Desea terminar el trabajo por lotes (S/N)?».

## 3. Corepack, `pnpm` y `yarn`

`corepack enable` escribe `pnpm`, `pnpx`, `yarn` y `yarnpkg` (`.cmd`, `.ps1` y sin extensión) en la carpeta del `node.exe` que lo ejecuta, o en `--install-directory`. Se probó con Node 24.22.0 y `--install-directory`: cada archivo apunta al `corepack` **de esa instalación** con una ruta relativa fija (`%~dp0\..\nodereal\node-v24.22.0-win-x64\node_modules\corepack\dist\pnpm.js`) y ejecuta `node` del PATH. Al cambiar de versión de Node (o desinstalar la que escribió el archivo) quedan apuntando a una versión que ya no es la del proyecto.

Decisión: Crisol trae **sus propios** shims `pnpm`, `pnpx`, `yarn` y `yarnpkg`, que ejecutan `node.exe <instalación>\node_modules\corepack\dist\<nombre>.js` con la versión de Node del proyecto. `corepack enable` y `corepack disable` (sin `--install-directory`) no escriben nada y explican por qué. Lo demás de Corepack pasa tal cual.

**Dónde descarga Corepack:** por defecto en `%LOCALAPPDATA%\node\corepack`. El shim fija `COREPACK_HOME=CRISOL_HOME\cache\corepack` (si el usuario no puso la suya). Comprobado con la red (`real_pnpm_goes_through_corepack_and_downloads_only_into_the_home`): `pnpm --version` con `packageManager: pnpm@9.15.9` descarga pnpm a `cache\corepack` y no cambia nada en `%LOCALAPPDATA%\node\corepack`. Corepack avisa antes de descargar salvo que `COREPACK_ENABLE_DOWNLOAD_PROMPT=0`; su `corepack.js` ya lo fija a `0`.

**Node 25 y posteriores no traen Corepack** (los ZIP de 20, 22 y 24 sí). Con esas versiones `pnpm`/`yarn` dicen que esa versión no incluye Corepack y qué hacer (`crisol use node 24`, o `npm install --global corepack` para esa versión).

## 4. Qué hace `crisol path add` frente al Node de la máquina

En este equipo hay un Node de máquina (`C:\Program Files\nodejs`) en el PATH de máquina, que Windows consulta **antes** que el PATH de usuario. Crisol nunca cambia el PATH de máquina (ADR-0002), así que `node` en una terminal normal seguirá siendo el de Programas aunque `crisol path add` ponga los shims primero en el PATH de usuario. `crisol path` y `crisol doctor` lo dicen con el nombre del programa y la carpeta, y proponen `crisol env`: imprime las líneas que ponen los shims primero **solo en esa terminal** (`crisol env | Invoke-Expression` en PowerShell; `cmd` y `bash` tienen su forma). No escribe nada en ningún sitio. Un `crisol shell` que lance una terminal así queda como pregunta al propietario (ver `claude/ESTADO.md`).
