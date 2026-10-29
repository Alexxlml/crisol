# ADR-0019 — `crisol shell`: una terminal donde solo manda Crisol

- **Estado:** aceptada (el propietario respondió el diseño el 2026-10-29); sin implementar
- **Fecha:** 2026-10-29

## Contexto

Crisol debe funcionar como un *sandbox*: lo que se ejecuta con Crisol no lo interrumpe lo de fuera, y Crisol no interrumpe a nadie. Se espera que no corra a la vez que otra suite (los puertos chocarían, ADR-0007), pero **otra suite instalada no debe poder alterar su funcionamiento**. Eso incluye el PATH de máquina, el de usuario, los programas globales y las variables de entorno.

Hoy hay tres caminos, y ninguno basta:

- **Los shims en el PATH de usuario** (ADR-0002) pierden ante el PATH de máquina. En este equipo, el Node de `C:\Program Files\nodejs` gana.
- **`crisol env`** (ADR-0018 §5) solo antepone los shims. No limpia variables como `PHPRC` de otra suite, y depende de detectar bien la terminal (revisión de la Fase 10, hallazgo 1).
- **Devaron** (`analisis/devaron/fa5-sistema.md`) abre una terminal desde su GUI con un `PATH` temporal y `REVOLTZ_ROOT`, sin perfil propio y sin limpiar nada.

## Decisión

`crisol shell` abre, **en la misma ventana**, una terminal hija con un entorno armado por Crisol. `exit` devuelve a la terminal de antes. Fuera de esa terminal no cambia nada: no se escribe en el registro, en el PATH ni en los perfiles del usuario.

### 1. El `PATH`: los shims delante de todo

El `PATH` del hijo es el actual, con delante, por este orden:

1. `shims/`;
2. las carpetas de los **clientes de las bases** instaladas con Crisol, solo de las versiones que use la configuración, para que esos nombres sean los de Crisol y se conecten a sus servicios. Son los clientes que trae cada instalación: `mysql`, `mariadb`, `psql`, `redis-cli`… MongoDB 8 ya no incluye `mongosh`; ofrecerlo sería una entrada nueva del catálogo, que se decide aparte.

No se quita nada del resto del `PATH`: lo que no es de Crisol (Git, VS Code…) sigue disponible.

### 2. Variables de otras herramientas: se quitan

Antes de lanzar la terminal se quitan del entorno del hijo las variables que cambian cómo se comportan PHP, Node o npm: `PHPRC`, `PHP_INI_SCAN_DIR`, `NODE_PATH`, `NODE_OPTIONS`, `npm_config_*` (en cualquier combinación de mayúsculas y minúsculas) y las propias marcas de los shims (`CRISOL_PHPRC`, `CRISOL_NPM_PREFIX`). La lista exacta se fija al implementar y se prueba. No se avisa al abrir: la lista va en la documentación del usuario.

### 3. Perfil de Crisol, después del tuyo

Crisol genera un perfil por terminal en `etc/shell/` y lo ejecuta **después** del perfil del usuario, antes del primer comando:

| Terminal | Cómo se lanza | Perfil del usuario |
|---|---|---|
| PowerShell 7 (`pwsh`) | `-NoExit -Command ". '<etc>\shell\crisol.ps1'"` | `$PROFILE` se carga antes |
| Windows PowerShell 5.1 | igual, con `powershell.exe`; además se corrige `PSModulePath` si se heredó de la otra edición | `$PROFILE` se carga antes |
| cmd | `cmd /k "<etc>\shell\crisol.cmd"` | `AutoRun` corre antes |
| Git Bash | `bin\bash.exe --rcfile <etc>/shell/crisol.bashrc -i`, archivo que primero carga `~/.bashrc` | lo carga el archivo de Crisol |

El perfil de Crisol vuelve a poner sus carpetas primero (por si el del usuario metió nvm, Laragon…), quita otra vez las variables del punto 2, pone el prefijo **`(crisol)`** en el prompt y titula la ventana **«Crisol»**. `crisol shell --clean` abre sin el perfil del usuario (`-NoProfile`, `/d`, `--norc`).

### 4. Qué terminal se abre

La misma desde la que se llama, detectada por el proceso padre (como `crisol env`). `--shell pwsh|powershell|cmd|bash` la fuerza. Si el padre no es una de las cuatro soportadas, se usa pwsh, luego powershell y luego cmd, y se dice cuál se abrió. Git Bash se busca por la instalación de Git (`git.exe` en el `PATH` → `..\bin\bash.exe`), nunca por `git-bash.exe`, que abre otra ventana.

**Terminales soportadas:** cmd, Windows PowerShell 5.1, PowerShell 7 y Git Bash. Otra cualquiera (nushell, fish, WSL) recibe un error que lo dice y propone `--shell`.

### 5. Dónde va cada cosa

- `crisol-core`: el entorno del hijo (`PATH` y variables que se quitan), el texto de cada perfil y la elección de terminal a partir del nombre del padre. Es puro y se prueba sin lanzar nada.
- `crisol-platform`: buscar los ejecutables de cada terminal y lanzarla esperando a que termine. El código de salida de la terminal se devuelve tal cual.
- `crisol-cli`: solo el comando y sus banderas.

## Alternativas descartadas

- **PATH reducido (solo shims y Windows):** más aislado, pero rompe Git, VS Code y cualquier herramienta del usuario dentro de la terminal. El propietario prefirió los shims delante de todo.
- **Solo `crisol env`:** no limpia variables, no tiene perfil y depende de adivinar la terminal.
- **Abrir una ventana nueva:** rompe el uso desde VS Code y desde terminales integradas.
- **Sin el perfil del usuario:** totalmente predecible, pero pierde el prompt y los alias. Queda como `--clean`.

## Consecuencias

- Dentro de `crisol shell`, los programas de otras suites que Crisol no reemplaza siguen existiendo detrás. Lo que Crisol sí ofrece (PHP, Composer, Node y sus gestores, clientes de bases) gana siempre.
- Un perfil del usuario que *reescriba* el `PATH` entero queda corregido por el de Crisol, que corre después. Uno que lance procesos al abrir sí los lanza (salvo con `--clean`).
- Un programa que lea el `PATH` del registro en lugar del de su proceso escapa al aislamiento (es raro).
