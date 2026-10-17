# ADR-0018 — Shims de Node: scripts directos, `pnpm` y `yarn` propios, prefijo global por versión

- **Estado:** aceptada (decidida en la Fase 10; el propietario puede revisarla)
- **Fecha:** 2026-10-17

## Contexto

La Fase 10 pide que `node`, `npm`, `npx` y `corepack` sigan la versión del proyecto como `php` (ADR-0002, ADR-0010), y que los paquetes globales de npm no se mezclen entre versiones ni ensucien `%APPDATA%\npm`. Lo medido está en `analisis/node-shims.md`:

- `npm.cmd` y `npx.cmd` (los archivos del ZIP de Node) abren `cmd.exe` y, `npm`, un segundo `node` solo para averiguar el prefijo: 172 ms contra 85 ms de `node npm-cli.js`. Rust (1.99) pasa intactos `%`, `^`, `"` y espacios a un `.cmd`, y rechaza solo los saltos de línea («batch file arguments are invalid»).
- `corepack enable` escribe `pnpm`, `yarn` y los demás en la carpeta de **una** instalación de Node, con rutas relativas a esa instalación: no sirve con varias versiones.
- Corepack descarga en `%LOCALAPPDATA%\node\corepack`, y npm guarda su prefijo global en `%APPDATA%\npm` y su caché en `%LOCALAPPDATA%\npm-cache`.

## Decisión

### 1. Los shims ejecutan el script, no el `.cmd`

`npm`, `npx`, `corepack`, `pnpm`, `pnpx`, `yarn` y `yarnpkg` se lanzan como `node.exe <script> <argumentos>` con el Node de la versión elegida (`crisol_core::nodetool`). Los argumentos llegan intactos (también un salto de línea). Una copia de npm instalada con `npm install -g npm` gana sobre la del ZIP, que es lo que hacía `npm.cmd` con `npm-prefix.js`.

### 2. `pnpm` y `yarn` son shims de Crisol, `corepack enable` no hace nada

Hay shims `pnpm`, `pnpx`, `yarn` y `yarnpkg` que ejecutan los puntos de entrada de Corepack de esa versión de Node. `corepack enable` y `corepack disable` (sin `--install-directory`) no escriben nada y lo explican; el resto de Corepack pasa. Corepack descarga en `CRISOL_HOME\cache\corepack` (`COREPACK_HOME`, salvo que el usuario ponga el suyo). Node 25 y posteriores no traen Corepack: los shims lo dicen y proponen una versión que sí lo tiene o `npm install --global corepack` (que entonces se usa).

### 3. Prefijo global por versión y caché propia, por variables del proceso hijo

Para `node`, `npm` y `npx` el shim fija, solo en el proceso hijo:

- `npm_config_prefix = data/node/<versión>/global`;
- `npm_config_cache = cache/npm` (una caché para todas las versiones: es direccionada por contenido).

No se escribe ningún `.npmrc` ni se toca `%APPDATA%\npm`. Si el usuario ya definió la variable, o su `.npmrc` (el de `NPM_CONFIG_USERCONFIG` o `~/.npmrc`, que solo se lee) tiene una línea `prefix` o `cache`, esa se respeta y Crisol no fija la suya (el prefijo, entonces, no entra en el `PATH` del hijo).

Además, el `PATH` del hijo lleva delante la carpeta global de esa versión y la carpeta del propio Node, para que un script, `npx` o un programa global que diga `node` obtenga la versión elegida y no el primer `node` que encuentre la terminal.

Desinstalar una versión de Node **no** borra su carpeta global (son datos del usuario).

### 4. Programas globales: un shim por programa, en `shims/`

`npm install -g typescript` deja `tsc.cmd` en la carpeta global de esa versión. Para que `tsc` funcione en una terminal sin añadir carpetas al `PATH` (ADR-0002: una sola entrada), Crisol hace una **copia del shim con el nombre del programa** (`shims/tsc.exe`):

- la hace el propio shim de `npm` cuando termina bien un `install`, `uninstall`, `update`, `link`… con `-g`/`--global`/`--location=global`, y `crisol shims install|sync`;
- los nombres se anotan en `shims/.crisol-globals`: un archivo con ese nombre que no esté en la lista no se sobrescribe ni se borra;
- se ofrece un comando por cada programa de **cualquier** versión de Node instalada, y desaparece cuando ninguna lo tiene;
- ejecutado como `tsc`, el shim busca la versión de Node del directorio, el `tsc.cmd` de su carpeta global y lo ejecuta (con el Node de esa versión primero en el `PATH`); si el programa solo está en otra versión, lo dice y dice cuál (`crisol use node 24.22.0` o instalarlo aquí);
- un nombre que no es un comando de Crisol y no está en la lista no hace nada.

Los nombres de Crisol (`php`, `node`, `npm`, `npx`, `composer`, `corepack`, `pnpm`, `pnpx`, `yarn`, `yarnpkg`) nunca son programas globales.

### 5. El Node de la máquina y `crisol env`

Con un Node en el `PATH` de máquina (`C:\Program Files\nodejs`), Windows lo consulta antes que el `PATH` de usuario, y Crisol no toca el de máquina. `crisol doctor` y `crisol path` lo dicen (qué programa gana, de dónde, por qué Crisol no lo cambia) y proponen `crisol env`: imprime las líneas que ponen los shims primero **solo en esa terminal** (PowerShell, `cmd` o Bash), sin escribir nada.

### 6. Comandos del usuario

`crisol shims install|remove|sync|status`, `crisol path add|remove|status` y `crisol env [--shell …]`. Cada cosa ocurre solo con el comando; ninguno se ejecuta al instalar Node ni al arrancar el daemon. `shims install` se niega a copiar un `crisol-shim` que otras cuentas puedan reemplazar (como `autostart on` con `crisold`).

## Alternativas descartadas

- **Lanzar `npm.cmd`:** el doble de lento, pierde los saltos de línea y deja el aviso «¿Desea terminar el trabajo por lotes?» al pulsar Ctrl+C.
- **`corepack enable --install-directory shims\`:** los archivos que escribe fijan la ruta de una instalación concreta.
- **Añadir la carpeta global de la versión activa al `PATH` del usuario:** cambia con la versión y rompe la regla de una sola entrada.
- **Un solo shim «genérico» que resuelva cualquier nombre sin tener un archivo por programa:** Windows solo encuentra programas que existen como archivo en una carpeta del `PATH`.
- **Prefijo global compartido entre versiones (`data/node/global`):** los paquetes con partes nativas se compilan para una versión de Node.

## Consecuencias y límites

- Los programas globales instalados con `pnpm add -g` o `yarn global add` no tienen comando (sus carpetas globales son suyas).
- Los programas globales se ejecutan por su `.cmd` (npm no deja otra cosa): un argumento con salto de línea no se puede pasar (el error lo explica) y Ctrl+C puede mostrar el aviso de `cmd.exe`.
- Un script que llame a `%APPDATA%\npm` por ruta fija no encontrará lo instalado con Crisol.
- Con un `npm_config_prefix` en el entorno (o `prefix` en el `.npmrc` del usuario), todo vuelve a ser como en cualquier instalación de Node: los programas globales van a donde el usuario dijo.
- El shim de `npm` hace un trabajo extra tras `npm i -g` (leer las carpetas globales y copiar un ejecutable por programa); no ocurre en ningún otro caso.
