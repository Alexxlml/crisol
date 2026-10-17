# ADR-0015 — Daemon, IPC y CLI

- **Estado:** aceptada (implementada en la Fase 8)
- **Fecha:** 2026-10-16

## Contexto

La Fase 8 une todo lo anterior en un proceso residente (`crisold`), un contrato para hablarle que las GUIs futuras puedan usar (`crisol-protocol`) y la línea de comandos (`crisol`). Hay que decidir el transporte, el modelo de concurrencia, qué hace el CLI cuando no hay daemon y qué se instala aparte con administrador.

## Decisión

1. **Protocolo.** JSON-RPC 2.0, un objeto JSON por línea, versionado por `PROTOCOL_VERSION`. `crisol-protocol` no hace E/S: define `Call` (20 operaciones), `Reply`, `RpcError` con códigos estables, `Event` y un `Client` síncrono sobre cualquier par de flujos. Añadir un campo a una respuesta o evento es compatible; cualquier otro cambio sube la versión. El cliente se presenta con `hello` y el daemon rechaza una versión que no entiende.
2. **Transporte.** Una *named pipe* por home (`\\.\pipe\crisol-<hash de la ruta>`), con un descriptor de seguridad que solo deja entrar al usuario actual y a SYSTEM (ni siquiera a otros administradores), sin clientes remotos y con `first_pipe_instance` para que un segundo daemon del mismo home no arranque. **El cliente comprueba quién está al otro lado** (`GetNamedPipeServerProcessId` y el SID de ese proceso) antes de mandar nada: los nombres de pipe son globales y otra cuenta podría haber creado antes uno con ese nombre.
3. **Quién hace qué.** La lógica está en `crisol-core`: el `Engine` responde las operaciones que solo cambian archivos (`install`, `use`, `park`, `link`, `secure`, `list`, `sites`, `logs`, `doctor`…) y el `Daemon` (una máquina de estados sin E/S ni hilos propios) es dueño del `WebStack` y responde las de servicios en marcha. El CLI solo traduce palabras a `Call` y respuestas a texto. Por eso un cliente sin daemon corre exactamente el mismo código del `Engine` y obtiene la misma respuesta.
4. **Concurrencia.** `tokio` (la convención del proyecto) solo en `crisol-daemon`: acepta conexiones, las atiende por líneas y reenvía eventos. Los servicios viven en un **hilo-actor** ordinario que es el único dueño del `Daemon`, así que no hay candados; el actor avanza los servicios cada 150 ms y retiene la respuesta de `start`, `stop`, `restart` y `shutdown` hasta que el daemon se estabiliza (`settled`), de modo que `crisol start` vuelve cuando Nginx está arriba o falló, y dice por qué. El núcleo sigue siendo código síncrono.
5. **El daemon vigila los archivos del usuario** (`crisol.toml`, `sites.toml`, las carpetas con `park` y los *pins* de cada sitio) cada dos segundos y recarga Nginx sin que nadie lo pida. Un cambio de puertos o de sufijo no se adivina: se avisa que hace falta `crisol restart`.
6. **Qué hace el CLI sin daemon.** Las operaciones de archivos, `status` (todo `stopped`) y `doctor` van directo al núcleo. `start` y `restart` **arrancan el daemon** (`crisold` junto a `crisol`, separado de la terminal y de su *job*). `stop` sin daemon dice que no había nada. `crisol stop` detiene **solo los servicios** (Nginx y PHP): el daemon sigue, vigila los archivos y responde el DNS de los `.test`. Cerrarlo es explícito: `crisol stop --daemon` o `crisol shutdown` (`shutdown`), y solo entonces se avisa de que, con la regla NRPT instalada, los `.test` no resuelven mientras el daemon no corre (hallazgo 4 de la Fase 7). (Corregido en la revisión de la Fase 8: antes `stop` cerraba también el daemon y existía `--keep-daemon`.)
7. **Un daemon por home**, sin servicio de Windows ni administrador. El respondedor DNS de ADR-0014 vive en el daemon (hilo propio, `127.77.0.53:53`); si no puede abrir el puerto lo registra y sigue.
8. **Arranque al iniciar sesión (F8.4).** Un valor en `HKCU\...\Run` con la ruta de `crisold` entre comillas y nada más; `crisol autostart on|off|status`. No es una tarea programada con privilegios.
9. **El ayudante privilegiado (F8.5, F8.6).** `crisol system install` lanza **una vez** `crisol-elevate.exe install-self <sufijo>` con UAC; ese proceso se copia a `%ProgramFiles%\Crisol\`, comprueba otra vez de forma estricta que la carpeta solo la pueden escribir los administradores, escribe el *manifest* (versión de Crisol y SHA-256 de lo copiado) y crea la regla NRPT: un solo aviso. **Quien llama conoce el ayudante sin ejecutarlo** (el *manifest* `requireAdministrator` hace que cualquier ejecución pida UAC): `HelperLocation::verify` lee el *manifest* de esa carpeta de administradores y lo compara con el hash del programa y con la versión de este Crisol; si no coincide, se niega y pide instalar de nuevo (W1 de la verificación de la Fase 7). `crisol system uninstall` quita la regla (solo si es de Crisol), el bloque de `hosts` y la carpeta; solo corre desde la copia instalada. El proceso elevado no puede escribir resultados en ninguna ruta que el lado sin privilegios le indique, así que informa con **códigos de salida** (`SetupFailure`).
   - **Qué se confía en la instalación.** El proceso que se instala copia el `crisol-elevate.exe` desde el que arrancó: quien ejecuta `crisol system install` elige darle administrador al que vino con su `crisol.exe`, como con cualquier instalador que descargue. Todo lo posterior (se ejecuta solo desde Program Files, con su hash y los permisos de la carpeta comprobados) no depende de eso.

## Consecuencias

- Los mensajes de error de la home abierta (ADR-0008) llegan intactos al cliente: la frase en `message` y el comando exacto en `fix`; el CLI los muestra en dos líneas y `--json` los entrega como objeto.
- El protocolo tiene ya los puntos de extensión para las Fases 9 a 11 (servicios de bases de datos entran en `Status`/`Start` sin cambiar la forma de nada; `Event::Log` y `Event::Progress` ya existen).
- Pendiente a propósito (no están en el plan de la Fase 8): comandos de `trust`/`untrust` de la CA, instalar los *shims* y agregarlos al PATH desde el CLI, `repair`, y que `start`/`stop` de un servicio suelto tengan sentido cuando haya más de uno (hoy Nginx y los pools son una unidad). Las comprobaciones de `doctor` ya lo reflejan sin dar comandos que no existen.
- Verificación manual con administrador pendiente: `crisol system install` y `uninstall` reales (UAC, `Program Files`, NRPT, `hosts`), el respondedor DNS en `127.77.0.53:53` dentro del daemon real, y `crisol start` con Nginx y PHP reales.

## Corrección (2026-10-16, revisión de la Fase 8)

Decidida por el propietario a partir de `claude/revisiones/2026-10-16-opus-fase8.md`. Pendiente de implementar.

- **Punto 6:** `crisol stop` detiene **solo los servicios**. El daemon sigue corriendo y cerrarlo es explícito (`crisol stop --daemon` o `crisol shutdown`). Desaparece `--keep-daemon`.
- **Punto 9, qué se confía:** además de lo dicho, antes del UAC de `system install`, de `autostart on` y de lanzar `crisold`, se comprueba que el ejecutable y su carpeta no los pueden escribir otras cuentas. El proceso elevado repite la comprobación y lee el programa de origen una sola vez. Las versiones publicadas llevan firma Authenticode, que se comprueba antes del UAC. No se fija un hash al compilar.
- **Registro:** el daemon usa `tracing` (convención del proyecto) con hora legible, desde antes de la Fase 9.
- **Robustez del daemon:**
  - un error al aceptar una conexión no lo cierra;
  - las operaciones largas del `Engine` corren fuera del hilo-actor, de modo que los servicios se siguen supervisando y `status` responde durante una instalación;
  - el nombre del pipe incluye el SID del usuario.
