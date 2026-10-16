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

## Fase 11 — Salud del sistema y limpieza

- **F11.1** `crisol doctor`: puertos en conflicto, otros PHP/Node en el PATH, CA confiada o no, DNS de `.test`, servicios caídos, permisos. Cada problema con su solución sugerida.
  - → nota: incluir en `doctor` la medición del arranque lento de BD por Defender (con opción de excluir solo `data/`), colisiones de dominios en `hosts` y un diagnóstico de errores de arranque (runtime VC++ ausente, DLL faltante).
- **F11.2** `crisol uninstall --all`: revierte PATH, CA, DNS/hosts, autoarranque. Lista exactamente qué cambió antes de hacerlo.
  - *Aceptación:* en una VM o sandbox de Windows, instalar → usar → desinstalar deja el registro, el PATH, el hosts y el almacén de certificados idénticos al inicio (comparar exportaciones).
  - → nota: todo lo que Crisol cambie en el sistema debe poder revertirse: bloque en `hosts`, exclusiones de Defender, entrada de arranque.

## Fase 12 — Contrato para la GUI

- **F12.1** Documentar el protocolo en `docs/protocolo.md` (cada método, parámetros, errores, eventos) generado o verificado desde `crisol-protocol`.
- **F12.2** Cliente de ejemplo mínimo que se suscribe a eventos y muestra el estado en consola: es la prueba de que cualquier carcasa (Slint, Tauri, web) puede montarse encima.

---

## Fuera de alcance de este plan

GUI, soporte Linux/macOS (solo se garantiza que el diseño lo permita), túneles/compartir sitios, bases de datos remotas, actualizaciones automáticas de Crisol. Se planifican después de la Fase 12.
