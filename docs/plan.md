# Plan de construcción — Backend de Crisol

Cada fase tiene tareas con un ID (`F1.3`). Haz una tarea a la vez, en orden. Una tarea está terminada solo cuando cumple su **criterio de aceptación** y pasan `cargo test --workspace`, `cargo clippy --workspace -- -D warnings` y `cargo fmt --check`.

Leyenda: 🔒 requiere admin (operación aislada y opcional) · 🌐 requiere red · 🪟 específico de Windows.

---

## Fase 0 — Esqueleto

**Objetivo:** workspace compilando, con CI local y convenciones fijadas.

- **F0.1** Crear el workspace de Cargo en `src/` con los crates `crisol-core`, `crisol-platform`, `crisol-protocol`, `crisol-daemon`, `crisol-cli`, `crisol-shim` (ver `docs/arquitectura.md`). Edición 2021 o la más reciente estable; `rust-toolchain.toml` fijando el canal estable.
  - *Aceptación:* `cargo build --workspace` compila; cada binario imprime su versión con `--version`.
- **F0.2** Configurar `rustfmt.toml`, `clippy` estricto en `[workspace.lints]`, `.gitignore`, y un script `src/scripts/check.sh` que corra fmt, clippy y tests.
  - *Aceptación:* el script pasa en limpio.
- **F0.3** Inicializar git en la raíz del proyecto (no en `src/`) y hacer el primer commit.

## Fase 1 — Rutas y configuración

**Objetivo:** saber dónde vive todo y leer la configuración sin destruirla nunca.

- **F1.1** `CrisolPaths`: resuelve `CRISOL_HOME` (variable o default por SO) y expone cada subdirectorio de la arquitectura. Función `ensure_layout()` que crea lo que falte.
  - *Aceptación:* pruebas con `tempfile` verifican la estructura; ninguna ruta se arma concatenando strings.
- **F1.2** Modelo de `crisol.toml` (serde) con defaults: versión por defecto de PHP/Node, servicios habilitados, puertos, TLD (`test`), carpetas parked.
  - *Aceptación:* un archivo vacío produce una config válida con defaults; campos desconocidos generan advertencia, no error.
  - → nota: no guardar secretos (contraseñas de BD, tokens) en `crisol.toml`; zona horaria de PHP del sistema o de la config, no fija; una sola fuente de verdad para puertos.
- **F1.3** Escritura segura: toda modificación de la config del usuario pasa por `toml_edit` y conserva comentarios y orden. Escritura atómica (archivo temporal + rename).
  - *Aceptación:* prueba con un TOML lleno de comentarios: después de cambiar un valor, el resto del archivo queda idéntico byte a byte.
  - → nota: la config del usuario manda; un archivo generado nunca gana sobre ella.
- **F1.4** Modelo de `.crisol.toml` por proyecto y la búsqueda hacia arriba desde un directorio.
  - *Aceptación:* pruebas con árboles de carpetas anidadas.

## Fase 2 — Supervisión de procesos

**Objetivo:** arrancar y detener procesos sin dejar huérfanos jamás.

- **F2.1** Trait `ProcessSupervisor` en core + implementación falsa para pruebas.
- **F2.2** 🪟 Implementación Windows con **Job Objects** (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`): cada servicio en su job; matar el job mata todo el árbol. Sin ventana de consola (`CREATE_NO_WINDOW`).
  - *Aceptación:* prueba de integración que lanza un proceso que a su vez lanza hijos; tras `stop`, ningún proceso del árbol sigue vivo.
  - → nota: un Job Object por proceso con `KILL_ON_JOB_CLOSE`; parada ordenada por servicio con un tiempo de gracia y `Kill` del árbol como respaldo.
- **F2.3** Redirección de stdout/stderr a `logs/<servicio>/` con rotación por tamaño.
  - → nota: redirigir siempre la salida de los servicios a archivo, también al lanzarlos sin privilegios.
- **F2.4** Trait `PortProbe` + implementación Windows: si un puerto está ocupado, devolver PID y nombre del proceso.
  - *Aceptación:* prueba que ocupa un puerto del rango 19000–19999 y verifica la detección.
  - → nota: nunca liberar un puerto matando al proceso que lo usa. Sí es útil consultar los rangos reservados de Windows (`netsh int ipv4 show excludedportrange`).
- **F2.5** Ciclo de vida del servicio: `Stopped → Starting → Running → Stopping`, más `Failed(motivo)`. Chequeo de salud por servicio (puerto escuchando o comando de prueba) con timeout.
  - → nota: parámetros de referencia del watchdog (≈3 caídas en 60 s, 1,5 s de espera, sondeo de arranque cada 500 ms con tope ≈20 s); hacerlos configurables.

## Fase 3 — Catálogo y descarga de versiones 🌐

**Objetivo:** instalar cualquier versión de cualquier servicio de forma verificable.

- **F3.1** Formato del manifiesto `catalog.toml` (servicio, versión, SO/arquitectura, URL, sha256, formato, ruta del ejecutable). Incrustado en el binario con `include_str!`.
- **F3.2** Investigar y llenar el catálogo inicial para Windows x64 **solo con fuentes oficiales**: PHP (windows.php.net, builds NTS x64), Nginx (nginx.org), Composer (getcomposer.org), Node (nodejs.org). Documentar cada fuente en `analisis/fuentes-binarios.md`.
  - *Aceptación:* cada entrada tiene hash verificado contra lo publicado por el proveedor.
- **F3.3** Descargador: progreso reportable (para la GUI futura), verificación SHA-256, extracción a carpeta temporal y movimiento atómico a `bin/<servicio>/<versión>/`. Si algo falla, no queda nada a medias.
  - → nota: la verificación SHA-256 de lo que Crisol descarga es un requisito, no un extra.
- **F3.4** Comandos de núcleo: `install`, `uninstall`, `list --installed`, `list --available`.
  - *Aceptación:* instalar y desinstalar PHP deja `bin/` exactamente como estaba antes.

## Fase 4 — PHP y shims

**Objetivo:** `php` en cualquier terminal usa la versión correcta del proyecto, con una sola entrada en el PATH.

- **F4.1** Generación de `php.ini` por versión a partir de `php.ini-development` + ajustes de Crisol: `extension_dir` correcto, extensiones comunes habilitadas (curl, fileinfo, gd, intl, mbstring, openssl, pdo_mysql, pdo_pgsql, pdo_sqlite, sqlite3, zip…), y luego `overrides/`. (Esto resuelve lo que tuviste que parchear a mano en Devaron.)
  - *Aceptación:* `php -m` en cada versión instalada muestra las extensiones esperadas; pruebas de snapshot con `insta`.
  - → nota: no fijar `date.timezone`; preferir plantillas completas más `overrides/` a parchear con expresiones regulares.
- **F4.2** `crisol-shim`: detecta con qué nombre fue invocado (`php`, `composer`, `node`, `npm`, `npx`), resuelve la versión según `docs/arquitectura.md` y ejecuta el binario real pasando argumentos, stdin/stdout y código de salida. Sin tokio. Debe ser muy rápido.
  - *Aceptación:* benchmark simple: sobrecarga del shim menor a ~20 ms; prueba de propagación de código de salida y de argumentos con espacios y comillas.
- **F4.3** Composer como shim que ejecuta `composer.phar` con el PHP resuelto.
- **F4.4** Trait `UserPath` + implementación Windows: agrega/quita **solo** `CRISOL_HOME\shims` del PATH **de usuario** (no el de máquina) y notifica el cambio (`WM_SETTINGCHANGE`). Detecta y advierte si hay otros `php.exe` antes en el PATH (Herd, Laragon, XAMPP) sin tocarlos.
  - *Aceptación:* `crisol path add` y `crisol path remove` dejan el valor del registro idéntico al original; probado contra una clave simulada, no la real.
  - → nota: ofrecer terminales con un PATH temporal: `crisol shell`/`crisol env` como camino sin cambios persistentes y los shims como opción.
- **F4.5** `crisol use php 8.3` (global) y `crisol use php 8.4 --local` (escribe `.crisol.toml`).

## Fase 5 — Nginx y sitios

**Objetivo:** cualquier carpeta en `www/` o enlazada se sirve en `nombre.test` con su PHP.

- **F5.1** Modelo de sitios: carpetas *parked* (cada subcarpeta = sitio) y sitios *linked* (ruta arbitraria + nombre). Detección de document root (`public/` para Laravel, raíz para WordPress, etc.).
- **F5.2** Generación de `nginx.conf` + un server block por sitio a partir de plantillas, con upstream al pool de PHP de su versión. Incluir `overrides/` por sitio.
  - *Aceptación:* snapshots de la config generada; `nginx -t` pasa sobre la config generada.
  - → nota: generar todo desde plantillas, sin parchear `nginx.conf` con expresiones regulares. En Apache, preferir variables a rutas absolutas (mejor para mover la carpeta).
- **F5.3** Pool de `php-cgi.exe` por versión en uso (tamaño configurable, reinicio automático, reciclaje por peticiones).
  - → nota: varios workers en puertos consecutivos desde un puerto base y un `upstream` por pool, para que la caída de algunos workers no tumbe el servicio.
- **F5.4** Recarga en caliente: cambiar sitios regenera la config y hace `nginx -s reload` sin cortar los demás sitios.
  - *Aceptación:* prueba E2E en puertos 19xxx: crear una carpeta con `index.php`, hacer una petición HTTP y recibir la salida de `phpinfo()` con la versión correcta.

## Fase 6 — HTTPS local

- **F6.1** CA local con `rcgen` (se genera una vez, la clave privada nunca sale de `certs/`).
  - → ver ADR-0013: la CA lleva *name constraints* (`test`, `localhost`, el TLD configurado, `127.0.0.0/8`, `::1`), `pathlen = 0`, ECDSA P-256 y 10 años; nunca se reemplaza sola.
  - → nota: CA propia con hojas por sitio, no un único certificado autofirmado.
- **F6.2** Certificado por sitio (o comodín `*.test`) firmado por la CA; integrar en el server block.
  - → ver ADR-0013 (y su corrección del 2026-10-16: sin comodín, el certificado lista los hosts asegurados porque `rustls-webpki` rechaza `*.test`): un solo certificado de 1 año con renovación automática, `secure = [...]` en `sites.toml`, redirección HTTP→HTTPS. Hecho.
  - → nota: evitar `.local` en los SAN. Vigencia corta en las hojas.
- **F6.3** 🪟 Trait `TrustStore`: instalar la CA en el almacén **CurrentUser\Root** (no requiere admin, Windows muestra un diálogo de confirmación). Desinstalar la quita.
  - *Aceptación:* `crisol secure sitio` y la petición HTTPS valida sin `-k`. `crisol trust remove` deja el almacén como estaba.
  - → nota: instalar la CA en CurrentUser\Root (sin admin) evita el aviso del navegador.
  - → ver ADR-0013: las pruebas automáticas usan un almacén privado; la instalación real la prueba el propietario a mano (`cargo run -p crisol-platform --example trust_manual`). La CA también va en `etc/php/cacert.pem` y en `NODE_EXTRA_CA_CERTS`. Hecho; falta solo la prueba manual.

## Fase 7 — Resolución de `.test`

**Objetivo:** dominios `.test` sin editar el archivo hosts a mano.

- **F7.1** Investigar y documentar en `analisis/dns-windows.md` las opciones: (a) servidor DNS local + regla NRPT para el sufijo `.test` 🔒; (b) escritura gestionada del archivo hosts dentro de un bloque delimitado `# BEGIN CRISOL` / `# END CRISOL` 🔒. Registrar la decisión como ADR.
  - → nota: Herd y Laragon ya usan `.test` en `hosts`: detectar colisiones y considerar un sufijo propio.
- **F7.2** Implementar la opción elegida, aislando la parte con admin en un ejecutable/operación mínima que solo hace eso.
- **F7.3** Implementar la otra como respaldo.
  - *Aceptación:* al desinstalar, el archivo hosts y las reglas DNS quedan como antes (probado contra copias, no contra el sistema real).

## Fase 8 — Daemon, IPC y CLI completa

- **F8.1** `crisol-protocol`: tipos JSON-RPC versionados para todas las operaciones de las fases anteriores + eventos (estado de servicio, logs, progreso de descarga).
- **F8.2** `crisold`: servidor IPC, estado en memoria, autoarranque opcional de servicios, apagado limpio (detiene todo lo que inició).
- **F8.3** `crisol` CLI con `clap`: `start/stop/restart/status [servicio]`, `install/uninstall/list`, `use`, `park/link/unlink/sites`, `secure/unsecure`, `logs -f`, `doctor`. Salida legible y opción `--json` en todo.
  - *Aceptación:* cada comando tiene prueba de integración contra un daemon en un `CRISOL_HOME` temporal.
  - Hecho además (2026-10-16): `crisol autostart on|off|status` (F8.4) y `crisol system install|uninstall` (F8.5, F8.6). Ver ADR-0015.
  - → `crisol stop` debe avisar que, con la regla NRPT instalada, los `.test` no resuelven (y esperan el tiempo de espera del DNS) mientras el daemon está parado (ADR-0014, revisión de la Fase 7, hallazgo 4).
- **F8.4** Registro opcional del daemon al iniciar sesión (Programador de tareas o clave Run del usuario), sin servicio de Windows ni admin.
  - → nota: arranque al iniciar sesión con `Run` del usuario o una tarea sin nivel alto, nunca con privilegios de administrador.
- **F8.5** 🔒 🪟 Instalar `crisol-elevate.exe` en `%ProgramFiles%\Crisol\` con la ACL por defecto de esa carpeta (solo Administradores, SYSTEM y TrustedInstaller escriben), desde un instalador que corre **una vez** con administrador (ADR-0014, revisión de la Fase 7, hallazgo 1). Aislada y opcional: sin ella no hay `.test` automático, pero nada más deja de funcionar. Después de copiarlo, comprobar con `HelperLocation::verify` y, en el mismo paso, crear la regla NRPT.
  - **Versión del ayudante (W1 de la verificación de la Fase 7):** quien lo llama debe conocer la versión del ayudante **sin ejecutarlo**, porque el *manifest* `requireAdministrator` hace que cualquier ejecución, incluso un `--version`, pida UAC. Por ejemplo, un recurso de versión que se lee del archivo, o un hash esperado del ayudante incrustado al compilar el binario principal (el instalador copia exactamente ese binario). Si no coincide, se niega y pide reinstalar. El instalador debe usar una compilación *release* con versión; la que hay hoy en `C:\Program Files\Crisol\` es de prueba y no corresponde a ningún commit.
  - *Aceptación:* tras instalar, `HelperLocation::installed().verify()` pasa; el ayudante nunca se ejecuta desde la home.
- **F8.6** 🔒 🪟 Desinstalar: quitar la regla NRPT y el bloque de `hosts` (con el ayudante), y después borrar `%ProgramFiles%\Crisol\`. `hosts` y la NRPT quedan como antes (probado contra copias, F7).

## Fase 9 — Bases de datos y servicios extra

Para cada uno: entrada de catálogo, plantilla de config, inicialización del directorio de datos, chequeo de salud, prueba E2E.

- **F9.1** MySQL y MariaDB (`--initialize-insecure` / `mariadb-install-db`; root sin contraseña solo en localhost, configurable). No pueden compartir el puerto 3306 a la vez: validar conflicto.
  - → nota: evitar puertos por defecto repetidos (MySQL y MariaDB en 3306); decidir aquí el reparto de puertos.
- **F9.2** PostgreSQL (`initdb`, `pg_ctl`).
- **F9.3** Redis — ⚠️ no hay builds oficiales para Windows; investigar opciones (port de tporadowski, Memurai, Garnet de Microsoft) y decidirlo en una ADR.
- **F9.4** Mailpit, Meilisearch, Memcached, MongoDB.
- **F9.5** Datos separados por versión mayor (`data/mysql/8.4/`) para poder tener dos versiones sin corromper datos.

## Fase 10 — Node

- **F10.1** Catálogo de Node (nodejs.org publica `SHASUMS256.txt`) e instalación por versión.
- **F10.2** Shims de `node`, `npm`, `npx` (y `corepack` para pnpm/yarn) con la misma resolución por proyecto.
- **F10.3** Prefijo global de npm por versión dentro de `CRISOL_HOME`, para que los paquetes globales no se mezclen entre versiones ni ensucien `%APPDATA%\npm`.
  - *Hecho (2026-10-17):* ver ADR-0018 y `analisis/node-shims.md`. Catálogo con Node 20/22/24 y `[recommended] node = "24"` (F10.1); shims de Node y `crisol shims|path|env` (F10.2); prefijo por versión, caché propia y un comando por programa global (F10.3).

## Fase 11 — Salud del sistema y limpieza

- **F11.1** `crisol doctor`: puertos en conflicto, otros PHP/Node en el PATH, CA confiada o no, DNS de `.test`, servicios caídos, permisos. Cada problema con su solución sugerida.
  - → nota: incluir en `doctor` la medición del arranque lento de BD por Defender (con opción de excluir solo `data/`), colisiones de dominios en `hosts` y un diagnóstico de errores de arranque (runtime VC++ ausente, DLL faltante).
- **F11.2** `crisol uninstall --all`: revierte PATH, CA, DNS/hosts, autoarranque. Lista exactamente qué cambió antes de hacerlo.
  - *Aceptación:* en una VM o sandbox de Windows, instalar → usar → desinstalar deja el registro, el PATH, el hosts y el almacén de certificados idénticos al inicio (comparar exportaciones).
  - → nota: todo lo que Crisol cambie en el sistema debe poder revertirse: bloque en `hosts`, exclusiones de Defender, entrada de arranque.

## Fase 12 — Contrato para la GUI

- **F12.1** Documentar el protocolo en `docs/protocolo.md` (cada método, parámetros, errores, eventos) generado o verificado desde `crisol-protocol`.
- **F12.2** Cliente de ejemplo mínimo que se suscribe a eventos y muestra el estado en consola: es la prueba de que cualquier carcasa (Slint, Tauri, web) puede montarse encima.
  - *Hecho (2026-10-30):* `crisol-platform/examples/watch.rs` (`cargo run -p crisol-platform --example watch`). Usa solo `crisol-protocol` y la conexión por tubería; sus pruebas corren con `cargo test --workspace`.

## Empaquetado (cuando haya versiones publicadas)

Decidido en la ADR-0024: un solo canal, winget (paquete portable dentro de un ZIP), y sin firma por ahora.

- **P.2** Flujo de publicación: etiqueta `v*` → `check.sh` → `cargo build --release` → ZIP, `SHA256SUMS` y firma minisign como borrador de release.
  - *Hecho (2026-10-30):* `.github/workflows/release.yml`, `src/scripts/package.sh` y `sign-release.sh`, y `docs/publicar.md` (secretos que debe crear el propietario). El flujo no se ha ejecutado en GitHub; el empaquetado y la firma sí se probaron en local.
- **P.3 (hecha 2026-10-30, sin la prueba en Windows Sandbox)** `crisol setup` en `crisol-core` y en el CLI (plan, acuerdo y ejecución: shims y, preguntando cada uno, `trust`, `system install` y `autostart`). Programas hermanos encontrados a través de enlaces simbólicos. `uninstall --all` reconoce una instalación de winget y no borra sus programas, y `doctor` avisa de un daemon de otra versión.
  - *Aceptación:* en Windows Sandbox, instalar con winget (o `winget install --manifest`) → `setup` → usar → `uninstall --all` → `winget uninstall` deja el sistema como al inicio, como en la ADR-0022. Comprobar si la carpeta del paquete lleva la versión y qué hace winget si no puede crear el enlace.
  - *Hecho:* `crisol-core::onboarding` (plan, acuerdo y ejecución; pasos shims, PATH y, preguntando cada uno, CA, ayudante y autoarranque; antes de todo comprueba que la carpeta de los programas es privada), `crisol setup [--yes]`, `real_current_exe` para resolver el enlace de winget, `uninstall --all` que no borra los programas de winget y `doctor` que avisa de un daemon de otra versión. **Falta** la prueba de aceptación en Windows Sandbox con winget (`winget install --manifest`), que necesita el manifiesto de P.4.
- **P.4** Manifiesto de winget, PR a `microsoft/winget-pkgs` y guía de instalación en `docs/`.

- **P.1** 🪟 Firma Authenticode de `crisol.exe`, `crisold.exe`, `crisol-shim.exe` y `crisol-elevate.exe` en las versiones publicadas, y comprobación de la firma del `crisol-elevate.exe` de origen **antes** del UAC de `crisol system install` (así el aviso de Windows muestra un editor conocido y no «desconocido»). Es el complemento de la comprobación de permisos que ya existe (`crisol_core::programs::require_trusted_program`, revisión de la Fase 8, hallazgo 3) y **no se implementa todavía**: necesita un certificado de firma de código. No se fija un hash del ayudante al compilar (decisión del propietario, 2026-10-16).

## Fase 13 — GUI

**Objetivo:** una GUI de bandeja y ventana encima del daemon, sin lógica propia (ADR-0001 y ADR-0026). La lógica va en `crisol-client` y se prueba sin interfaz. La GUI dibuja y traduce clics en llamadas.

Reglas de toda la fase:

- **Nada de lógica en la GUI.** Si una pantalla necesita decidir algo (qué mostrar, qué falta, qué hacer después), eso va en `crisol-client` o en `crisol-core`, con pruebas.
- **No se cambia lo que hace el CLI.** `src/crisol-cli/tests/commands.rs` debe pasar sin tocar sus expectativas, salvo los cambios que una tarea diga explícitamente.
- **Textos de la GUI en inglés y español** desde la primera pantalla, con el mecanismo de traducción del framework. Los mensajes del núcleo se muestran tal cual (ADR-0026 §4).
- **Cada tarea con pantalla** deja en su bitácora una lista de verificación manual (qué abrir, qué pulsar, qué debe verse) y capturas en `claude/capturas/` (no van al repositorio). Las pruebas automáticas cubren `crisol-client`.
- Las pruebas a mano se hacen con `CRISOL_HOME` en una carpeta de prueba y con puertos que no choquen con Herd ni con Devaron (`docs/arquitectura.md`).

- **F13.1** Crate `crisol-client`, que usan el CLI y la GUI (ADR-0026 §5). La tarea tiene tres partes:
  - **Mover sin cambiar conducta:** `Context` y sus tipos, `Link`, `Remote`, `Local`, la conexión con arranque del daemon (`link`, `start_daemon`), el `hello` con la versión del daemon, y el montaje de Windows de `crisol-cli/src/main.rs` como una función (`Context::windows()` o equivalente). El CLI queda con `args`, `render` y el `Session` que imprime.
  - **Añadir la vista en vivo** (`LiveView` o nombre equivalente). Abre su propia conexión de suscripción, hace `hello` → `subscribe` → `status` y aplica los eventos con la regla de `seq` de la ADR-0025: guarda los que llegan durante `status` y vuelve a pedir el estado si falta un número. Se reconecta cuando el daemon se va (`shutting_down` o la tubería cerrada) y cuando vuelve. Expone un estado que se puede copiar y comparar: servicios, «recargando», `blocked` y si hay conexión o no. Las llamadas van por otra conexión, porque el protocolo atiende una petición a la vez.
  - **Llevar `examples/watch.rs`** a usar `LiveView`.
  - *Aceptación:*
    - las pruebas del CLI pasan sin cambiar sus expectativas;
    - `LiveView` tiene pruebas con un daemon falso para: un evento más nuevo que la foto antes de la respuesta, un salto de `seq` que vuelve a pedir `status`, `shutting_down` y reconexión, y un daemon de otra versión;
    - `crisol-cli` no depende de nada que `crisol-client` no exponga;
    - la ADR-0026 o `docs/arquitectura.md` dicen dónde vive cada cosa.
- **F13.2** Prototipos de Slint y Tauri 2 y la decisión del framework (ADR-0026 §1).
  - **Dónde:** `prototipos/gui-slint/` y `prototipos/gui-tauri/`, fuera del workspace (que `cargo test --workspace` no los compile). Usan `crisol-client` como dependencia por ruta.
  - **Qué hacen:** la misma pantalla en los dos. Muestra la lista de servicios en vivo (`LiveView`), con botones de start y stop, y un icono en la bandeja con el menú de la ADR-0026 §2 (iniciar/detener todo, abrir, salir, detener todo y salir). Cerrar la ventana la oculta, y una segunda ejecución muestra la ventana existente. Si un framework no puede hacer algo de esto, se dice y se mide lo que cuesta resolverlo (por ejemplo, el crate de bandeja que haría falta con Slint).
  - **Medición:** con `--release`, en este equipo y con los mismos servicios activos. Se miden:
    - la RAM en reposo de todo el árbol de procesos (incluidos los de WebView2), con la ventana abierta y oculta en la bandeja, a los 30 s y a los 5 min;
    - el arranque en frío hasta la primera ventana (media de 5);
    - el tamaño de lo que se distribuiría (con el runtime que haga falta);
    - las líneas de la pantalla;
    - lo que añade al build y al flujo de publicación (Node y npm, herramientas);
    - la licencia, comprobada en la web oficial (Slint: GPLv3 o su licencia royalty-free y lo que exige; Tauri: MIT/Apache).
  - Antes de agregar cada dependencia, se comprueba su versión actual en crates.io o npm y se justifica en la bitácora.
  - **Entrega:** `analisis/gui-prototipos.md`, con la tabla, el método y una recomendación argumentada. **No se elige**: el propietario decide y se anota en la ADR-0026. Los prototipos se borran después de decidir.
  - *Aceptación:* los dos prototipos funcionan contra un `crisold` real con una home de prueba; la tabla está completa y el informe dice cómo repetir cada medida.
- **F13.3** Esqueleto de la GUI con el framework elegido. Va en el crate `crisol-gui` (binario `crisol-gui.exe`), dentro del workspace (si es Tauri, la parte web va en su carpeta y el build lo documenta `docs/publicar.md`). Incluye:
  - la bandeja con el estado general (todo corre, algo falló, parado, sin daemon) y el menú de la ADR-0026 §2: «Iniciar todo» o «Detener todo» según el estado (`start`/`stop`), «Abrir Crisol», «Salir» (solo la GUI) y «Detener todo y salir» (`shutdown`), con el aviso de una sola vez al salir con servicios corriendo;
  - la ventana que se oculta al cerrarse;
  - la instancia única por usuario, detrás de un trait de `crisol-platform`;
  - la traducción (inglés y español, según el idioma de Windows);
  - la conexión mediante `LiveView`;
  - un aviso con su arreglo cuando el daemon es de otra versión;
  - la pantalla vacía de cada sección de la ADR-0026 §3, con la navegación.

  **Pregunta para el propietario al empezar:** si la bandeja arranca al iniciar sesión (ADR-0026, «Preguntas abiertas»).
  - *Aceptación:* `check.sh` pasa con el crate nuevo; si el framework lo permite, `check.sh` compila la GUI. Verificación manual: abrir, cerrar a la bandeja, volver a abrir, segunda instancia, «Salir» con servicios corriendo (el aviso sale la primera vez y no la segunda; los sitios siguen respondiendo), «Detener todo» (el daemon sigue), «Detener todo y salir» (no queda `crisold`), y el paso de daemon parado a corriendo y al revés.
- **F13.4** Servicios y sitios. Incluye:
  - la lista de servicios con su estado en vivo, start/stop/restart y enable/disable;
  - el indicador de «recargando» entre `reload_started` y su cierre;
  - el aviso de `blocked` con su comando;
  - la lista de sitios (`sites`) con park, unpark, link, unlink, secure y unsecure, y «abrir en el navegador».

  Elegir carpeta usa el diálogo del sistema. Los errores muestran `message` y `fix` tal cual.
  - *Aceptación:* lo que decide qué botón se ofrece en cada estado está en `crisol-client`, con pruebas; verificación manual de cada acción contra un `crisold` real.
- **F13.5** Versiones: lo instalado y lo disponible (`list`), instalar con la barra de progreso (evento `progress`, que llega solo a la conexión que pidió `install`), desinstalar, `use` y los avisos de vulnerabilidad y fin de soporte que ya dan `install` y `list`. `install --from` queda para el CLI en esta versión.
  - *Aceptación:* progreso visible de principio a fin con una descarga real 🌐; cancelar la ventana durante una instalación no deja nada a medias (lo garantiza el núcleo; se comprueba).
- **F13.6** Logs y credenciales.
  - **Logs:** un visor por servicio, con las últimas líneas y el seguimiento con `from`, como `logs -f`, o el evento `log` si ya hay suscripción. Debe aguantar logs grandes sin cargar el archivo entero.
  - **Credenciales:** las líneas `.env` de cada base, con un botón para copiar y `reset_password` con confirmación (y el aviso de Garnet y Meilisearch si corren).
  - *Aceptación:* verificación manual; la lógica de seguimiento está en `crisol-client` y tiene pruebas.
- **F13.7** `doctor` con acciones. Primero se diseña: una ampliación de la ADR-0021 con el campo opcional `action` en `Check`, que es compatible (se añade un campo, sin cambiar la versión del protocolo). Las acciones tienen este alcance:
  - una llamada del protocolo;
  - una operación local del núcleo (`trust`, `setup`, `untrust --stale`, `autostart off`);
  - un enlace.

  Los comandos de administrador ajenos a Crisol (Defender) se muestran para copiar y **no se ejecutan** (ADR-0026 §6). El texto que ve el CLI no cambia. **El diseño lo aprueba el propietario antes de implementarlo.**
  - *Aceptación:* `docs/protocolo.md` y su prueba lo documentan; cada `Check` con acción tiene una prueba que la comprueba; en la GUI, el botón muestra lo que va a hacer antes de hacerlo.
- **F13.8** Primer arranque y desinstalación.
  - **El asistente:** con el `Plan` de `onboarding` muestra los pasos, lo hecho, los problemas y las notas, y pregunta cada paso opcional. Se abre solo cuando el plan no está vacío.
  - **La pantalla de `uninstall --all`:** con el `Plan` de `teardown`, muestra todo lo que el CLI lista y pide la confirmación, con la pregunta de los respaldos cuando se borra la home. Cierra la GUI al terminar.
  - Los dos usan `run_if_agreed`, en el proceso de la GUI (no en el daemon), para que los diálogos de Windows y el UAC salgan delante.
  - *Aceptación:* verificación manual completa en Windows Sandbox: el asistente en una máquina limpia, y la desinstalación que deja el sistema como al inicio (scripts de `src/scripts/sandbox/`, como la ADR-0022).
- **F13.9** Empaquetado de la GUI (ADR-0024 §6 y ADR-0026 §6). Incluye:
  - `crisol-gui.exe` en el ZIP (`package.sh`) y en el flujo de publicación (y su build web si es Tauri);
  - el manifiesto de winget, sin alias de comando para la GUI;
  - un paso de `setup` que crea el acceso directo en el menú Inicio del usuario (`.lnk`, detrás de un trait de `crisol-platform`), y su contraparte en `uninstall --all`;
  - `real_exe` y `is_winget_package` revisados para el quinto programa;
  - `docs/instalacion.md` actualizado.
  - *Aceptación:* `winget install --manifest` en Windows Sandbox → `crisol setup` crea el acceso directo → la GUI abre desde el menú Inicio → `uninstall --all` lo quita → el sistema queda como al inicio.
- **Revisión de la Fase 13** (Opus), con el resumen para la revisión en `claude/revisiones/`, como en las fases anteriores. Se puede pedir una revisión intermedia después de F13.3.

---

## Fuera de alcance de este plan

Soporte Linux/macOS (solo se garantiza que el diseño lo permita), túneles/compartir sitios, bases de datos remotas, actualizaciones automáticas de Crisol y la traducción de los mensajes del núcleo. Se planifican después de la Fase 13.
