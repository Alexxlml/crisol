# ADR-0024 — Empaquetado: Crisol completo sin GUI, por winget

- **Estado:** aceptada por el propietario (2026-10-30).
- **Fecha:** 2026-10-30

## Contexto

El backend está completo (fases 0 a 12), pero Crisol solo se usa compilándolo. El propietario quiere que **Crisol funcione y se instale con o sin GUI**: el daemon y el CLI son el producto, y la GUI será una capa ligera encima que se añade sin cambiar lo demás.

Lo que ya existe y condiciona el diseño:

- **Cuatro programas** que se buscan entre sí en la misma carpeta: `crisol.exe`, `crisold.exe`, `crisol-shim.exe` y `crisol-elevate.exe` (`main.rs` usa `current_exe().parent()`). Llevan el runtime de C dentro (ADR-0023), y el catálogo va incrustado.
- **`require_trusted_program`** (revisión de la Fase 8, hallazgo 3): `crisold.exe` y `crisol-elevate.exe` no se registran ni se elevan si otra cuenta puede escribir en el archivo o en su carpeta.
- **Una sola entrada en el PATH** (ADR-0002): `CRISOL_HOME\shims`. Hoy **`crisol` mismo no está en el PATH**.
- **`uninstall --all`** (ADR-0022) quita lo que Crisol cambia y borra los programas, con el patrón «plan → acuerdo → ejecutar» (`teardown::run_if_agreed`).
- **La home** (`C:\Crisol`) es de datos y privada (ADR-0008); los programas no van en ella.

## Decisión propuesta

### 1. Un solo canal: winget (propietario)

- Crisol se publica **solo en winget**, como paquete «portable» dentro de un ZIP: el manifiesto usa `NestedInstallerType: portable`, con `crisol.exe` como `PortableCommandAlias`.
- **Lo que esto resuelve:**
  - winget descarga, comprueba el SHA-256 del manifiesto, descomprime en su carpeta de paquetes (`%LOCALAPPDATA%\Microsoft\WinGet\Packages\…`, dentro del perfil: privada, así que `require_trusted_program` la acepta) y pone `crisol` en su carpeta de enlaces, que winget mantiene en el PATH. **Crisol no añade una segunda entrada al PATH** y la ADR-0002 no cambia.
  - Actualizar es `winget upgrade`, y desinstalar los programas, `winget uninstall`. Crisol no se copia ni se borra a sí mismo cuando lo instaló winget.
- **El ZIP sigue publicado** en GitHub (es lo que descarga winget). Quien no use winget puede descomprimirlo dentro de su perfil y usar la ruta completa o añadir la carpeta al PATH él mismo; es un uso avanzado, sin ayuda de `setup`.

### 2. Lo que se publica

- `crisol-<versión>-windows-x64.zip` con los cuatro programas, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-NOTICES.md` y un `LEEME.txt` corto.
- Junto al ZIP, `SHA256SUMS` y su **firma minisign**, con la misma herramienta con que Crisol verifica sus descargas (ADR-0005).
- Lo construye GitHub Actions con cada etiqueta `v*`: `check.sh`, `cargo build --release` en `windows-latest`, y el release queda como borrador para revisarlo antes de publicarlo. Después se abre el PR del manifiesto en `microsoft/winget-pkgs`.

### 3. `crisol setup`: los pasos del usuario

Después de `winget install`, `crisol setup` hace lo que winget no puede, con el mismo patrón que `uninstall --all` y la lógica en `crisol-core`, para que la GUI llame a lo mismo:

1. **Plan** (solo lee), **acuerdo** (`--yes` no pregunta) y **ejecución** con el resultado de cada paso.
2. Pasos: `crisol shims install` y `crisol path add` (los shims); y, **cada uno con su pregunta**, `crisol trust` (Windows pide confirmación), `crisol system install` (un UAC) y `crisol autostart on`.
3. Antes de nada comprueba que la carpeta de los programas es privada (`require_trusted_program`), para que un ZIP descomprimido en un sitio abierto se detecte al principio y no al registrar el daemon.

### 4. Lo que hay que cuidar con winget

- **Los enlaces de winget:** `crisol` del PATH puede ser un enlace simbólico a `crisol.exe` real. Si `current_exe()` devuelve la ruta del enlace, `crisol` buscaría a `crisold.exe` en la carpeta de enlaces y no lo encontraría. La búsqueda de los programas hermanos debe resolver el enlace (`canonicalize`). Se comprueba en la prueba de aceptación.
- **Actualizar con el daemon corriendo:** Windows no deja reemplazar un `.exe` en uso, así que `winget upgrade` fallaría con `crisold.exe` abierto. La guía dice `crisol shutdown` antes; `doctor` avisa si el daemon que corre no es de la versión instalada.
- **Desinstalar:** primero `crisol uninstall --all` (quita lo que Crisol cambió en Windows) y después `winget uninstall`. Cuando los programas están dentro de la carpeta de paquetes de winget, `uninstall --all` **no los borra** (los borraría por debajo de winget) y termina diciendo el comando de winget.
- **La entrada de autoarranque** apunta a la ruta de `crisold.exe`. Si una actualización de winget cambia esa ruta, `crisol autostart status` ya lo detecta como «otro crisold» y `autostart on` lo corrige; `setup` lo ofrece.
- Sin verificar todavía, a comprobar en la prueba de aceptación: si la carpeta del paquete lleva la versión en su nombre, y qué hace winget si no puede crear el enlace simbólico.

### 5. Firma: sin firma por ahora (propietario)

- Las primeras versiones salen **sin firmar**. Windows puede mostrar SmartScreen («editor desconocido») y el UAC de `system install` dirá «editor desconocido». La guía de instalación lo explica.
- **Camino para firmar sin pagar:** la [SignPath Foundation](https://signpath.org/) firma gratis proyectos de código abierto. Pide licencia de código abierto (Crisol es MIT o Apache-2.0), repositorio público, compilar en CI desde ese repositorio (el punto 2 ya lo hace) y MFA en las cuentas. El editor que mostraría Windows sería «SignPath Foundation». Si llegan donaciones, la otra opción es un certificado propio.
- Cuando haya firma: P.1, tal como está en el plan.

### 6. La GUI, después

La GUI será un quinto programa en el mismo paquete (o un paquete de winget que dependa del de CLI). Su primer arranque llamará al mismo plan de `setup`. Quien no la quiera no la instala.

## Tareas propuestas (para `docs/plan.md` cuando se acepte)

- **P.2** Flujo de publicación: etiqueta `v*` → `check.sh` → `cargo build --release` → ZIP, `SHA256SUMS` y firma minisign como borrador de release.
- **P.3** `crisol setup` en `crisol-core` y en el CLI. Programas hermanos encontrados a través de enlaces simbólicos. `uninstall --all` reconoce una instalación de winget, y `doctor` avisa de un daemon de otra versión.
  - *Aceptación:* en Windows Sandbox (con winget instalado o el manifiesto local con `winget install --manifest`), instalar → `setup` → usar → `uninstall --all` → `winget uninstall` deja el sistema como al inicio, como en la ADR-0022.
- **P.4** Manifiesto de winget, PR a `microsoft/winget-pkgs` y guía de instalación en `docs/`.
- **P.1** (ya en el plan) Firma Authenticode, cuando haya cómo (SignPath Foundation o certificado propio).

## Implementación de P.3 (2026-10-30)

- `crisol setup [--yes]` vive en `crisol-core::onboarding`; el CLI solo parsea, imprime y pregunta. Pasos, en orden: **shims**, **PATH**, y, **cada uno con su pregunta**, **CA**, **ayudante** y **autoarranque**. Solo se listan los que faltan; con todo hecho no pregunta nada, así que volver a ejecutarlo es seguro (y es lo que corrige un autoarranque que apunta a otra ruta tras una actualización).
- Antes de todo comprueba que la carpeta de los programas es privada (`require_trusted_program` sobre `crisol-shim.exe` y `crisold.exe`, que comparten carpeta: se dice una vez, con el `icacls` que la cierra). Mientras haya un problema no se cambia nada.
- Una negativa del usuario (a una pregunta, al diálogo de Windows de la CA o al aviso de administrador) es un resultado, no un fallo: el código de salida es 0 salvo que un paso falle de verdad.
- `--yes` no pregunta, pero Windows sigue preguntando por sí mismo (CA y UAC). `crisol trust` sigue mostrando las huellas **antes** del diálogo; `setup` las menciona en el resultado y recomienda `crisol trust` a quien quiera compararlas primero.
- **Enlaces:** `crisol-platform::real_current_exe` resuelve el enlace (`canonicalize` y quita el prefijo `\\?\`), y de ahí salen los programas hermanos, el autoarranque y el ayudante.
- **winget:** si la carpeta de los programas está dentro de `%LOCALAPPDATA%\Microsoft\WinGet\Packages` o `%ProgramFiles%\WinGet\Packages`, `uninstall --all` no los borra y termina diciendo `winget uninstall Alexxlml.Crisol`. El identificador del paquete está en `crisol_core::programs::WINGET_ID` y debe coincidir con el manifiesto de P.4.
- **`doctor`:** compara la versión del daemon que contesta (`hello`) con la del programa y avisa, con `crisol shutdown` como arreglo.
- **Sin comprobar todavía** (prueba de aceptación, con el manifiesto de P.4): si la carpeta del paquete lleva la versión, y qué hace winget si no puede crear el enlace simbólico.

## Consecuencias

- Crisol se instala, se actualiza y se quita con una herramienta que Windows ya trae, sin administrador. El administrador solo se pide para los nombres `.test` (`system install`), como hasta ahora.
- El PATH del usuario sigue con una sola entrada de Crisol (los shims); `crisol` llega por la carpeta de enlaces de winget.
- Depende de winget: sin él, el ZIP es para quien sepa ponerlo a mano.
- Sin firma, la primera impresión puede incluir avisos de Windows, hasta conseguir una.
