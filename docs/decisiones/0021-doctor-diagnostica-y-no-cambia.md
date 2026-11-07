# ADR-0021 — `doctor` diagnostica y no cambia nada

- **Estado:** aceptada (2026-10-29, F11.1). El propietario confirmó la sección 3 (Defender) el mismo día y pidió `crisol trust` de una vez (sección 4). **Sección 5 (acciones para la GUI, F13.7): aceptada por el propietario el 2026-11-06.**
- **Fecha:** 2026-10-29

## Contexto

F11.1 pide que `crisol doctor` detecte, cada uno con su solución:

- puertos en conflicto;
- otros PHP/Node en el PATH;
- si la CA está confiada;
- el DNS de `.test`;
- servicios caídos;
- permisos.

La nota del plan añade:

- medir el arranque lento de las bases por Defender, «con opción de excluir solo `data/`»;
- colisiones de dominios en `hosts`;
- un diagnóstico de errores de arranque (runtime VC++ ausente, DLL faltante).

Tres cosas no son obvias:

1. **Quién sabe qué.** El motor (`Engine`) solo ve archivos y corre en el hilo de trabajo del daemon. El estado de los servicios lo tiene el actor (`Daemon`). Y los códigos NTSTATUS, el registro y Defender son de Windows.
2. **Qué es una colisión en `hosts`.** En este equipo, Herd y Laragon definen 46 nombres bajo `.test`, casi todos con `127.0.0.1`.
3. **La exclusión de Defender necesita administrador.** F11.1 no está marcada como «requiere admin».

## Decisión

### 1. Tres capas

- **El motor** comprueba lo que se ve en archivos:
  - versiones sin soporte, incluida una 7.4.x instalada con `--from`;
  - restos de `runtime.log.rotating*`;
  - **todas** las carpetas abiertas de la home, no solo la primera;
  - sitios que `hosts` manda a otra dirección.
- **El daemon** añade, con `Daemon::complete_doctor`, lo que solo él sabe. El actor lo llama cuando el hilo de trabajo termina un `doctor`, y `Daemon::handle` lo llama en el modo sin daemon del CLI. Añade:
  - `services`: los que fallaron, con su causa y el significado del código de salida;
  - `ports`: los puertos de los servicios activados que **no** están corriendo y que otro programa ocupa, con un puerto libre sugerido. Los puertos de los servicios que corren son suyos.
  - `database startup`: cuánto tardó cada base en estar lista.
- **La plataforma**, detrás de `SystemChecks`, aporta:
  - `explain_exit` (códigos NTSTATUS);
  - `hosts_content` (solo lee el archivo);
  - `slow_start_hint`;
  - comprobaciones nuevas: runtime VC++ por el registro (solo lectura) o por las DLL de `System32`, y `::1`;
  - la regla NRPT, que ahora se comprueba resolviendo `crisol-doctor.<tld>` con el resolvedor de Windows cuando el daemon corre. Si no corre, avisa de que los nombres no resuelven.

### 2. Colisiones en `hosts`

- Solo es un problema un nombre que **es un sitio de Crisol** (o un subdominio de uno), que está **fuera** del bloque de Crisol y que apunta a una dirección que **no** es de loopback. Windows lee `hosts` antes que cualquier DNS, así que ese sitio no llega a Crisol.
- Una línea con `127.0.0.1` de otra suite no estorba a la resolución: si otra suite tiene el puerto, eso lo dice `ports`.
- La línea `hosts block` cuenta los nombres ajenos bajo el sufijo, sin listarlos.

### 3. Defender: medir y sugerir, nunca ejecutar

- `DbStack` mide el tiempo desde el lanzamiento hasta `Running`.
  - Solo cuenta los arranques en los que los datos ya existían: el primero también los crea (`initdb`, `mysqld --initialize`) y su duración no dice nada de Defender.
  - Más de 10 s es «lento».
- Si es lento, `doctor` da como `fix` el comando exacto para un administrador, y también el que lo deshace:
  - `Add-MpPreference -ExclusionPath '<home>\data'`;
  - `Remove-MpPreference -ExclusionPath '<home>\data'`.
- **Crisol no lo ejecuta ni lo pide por el ayudante elevado.** Así no hay nada de Crisol que revertir en F11.2. Si más adelante se quiere que Crisol añada la exclusión, será una operación nueva del ayudante con su ADR, marcada «requiere admin», y F11.2 tendrá que revertirla.
- **Confirmado por el propietario:** Crisol no decide, pero le dice al usuario exactamente qué necesita. Los usuarios cautelosos lo agradecen.
- **Corregido por la ADR-0022 (2026-10-29):** Crisol sigue sin crear exclusiones, pero `uninstall --all` (y `system uninstall`) quita, con el UAC del ayudante, las que encuentre dentro de su home, aunque las haya puesto el usuario. Sí hay algo que revertir en F11.2.
- **Para la GUI (pendiente, no se hace ahora):** el `fix` puede volverse un botón, siempre con estas condiciones:
  - el botón **muestra** el comando exacto antes de ejecutarlo;
  - lo ejecuta en un PowerShell elevado, así que el UAC es el consentimiento;
  - como entonces sí lo hizo Crisol, deja constancia en `state/` para que `uninstall --all` (F11.2) ofrezca deshacerlo.
  - Para eso, `Check` necesita, además del texto, una acción que una máquina pueda leer: un campo opcional, por ejemplo `action: Call | AdminCommand { program, args } | OpenUrl`. Se diseña con la GUI, sin cambiar lo que ve el CLI.

### 4. `crisol trust` y `crisol untrust`

Pedidos por el propietario el 2026-10-29. `doctor` los sugiere: `crisol trust` cuando la CA no está confiada, y `crisol untrust --stale` cuando siguen confiadas CA de Crisol antiguas.

- **`crisol trust`:**
  - Usa la CA de `certs/`; si no hay, la crea para el sufijo de `crisol.toml` (solo sufijos de uso especial, ADR-0013).
  - **Antes** de que Windows pregunte, imprime: el nombre, para qué vale (el sufijo, `localhost` y las direcciones de loopback), hasta cuándo, la SHA-256 y la **SHA-1**, que es la huella que muestra el diálogo de Windows. La calcula CryptoAPI (`CERT_SHA1_HASH_PROP_ID`), sin dependencias nuevas.
  - La confirmación es el diálogo de Windows: no hay un segundo «escribe yes».
  - Decir que no en el diálogo da código de salida 1 y no cambia nada.
- **`crisol untrust`** quita la CA actual del almacén del usuario.
- **`crisol untrust --stale`** quita las CA de Crisol antiguas y nunca la actual. Si la actual no se puede leer (caducada, por ejemplo), cuenta como antigua.
- Ninguno de los dos borra `certs/`: los sitios siguen con sus certificados, y el navegador avisa hasta el siguiente `crisol trust`.
- **Dónde se ejecutan:** en el proceso del CLI, no en el daemon, para que el diálogo salga delante del usuario. El CLI solo encadena `trust::prepare_trust` (muestra el plan) y `trust::trust_ca`; `trust::untrust_local` hace el resto. La GUI llamará a las mismas funciones.
- La parte manual, con el almacén real, la hace el propietario (ver «Verificación manual»).

### 5. Acciones de `doctor` para la GUI (F13.7)

> Estado: **aceptada** (2026-11-06). Borrador de Opus; decisiones del propietario: **(1)** `crisol system install` es un botón con el UAC; **(2)** reinstalar son dos llamadas, `uninstall` y después `install`; **(3)** hay una acción `Reveal` para abrir en el Explorador un archivo o una carpeta de la home; **(4)** el núcleo manda el tipo y los datos, y `crisol-client` pone la etiqueta traducida.

Sustituye la idea de «Para la GUI» de la sección 3. Allí se proponía `AdminCommand { program, args }`, y eso ya no existe: la GUI nunca ejecuta un programa que le diga una respuesta (ADR-0026 §6).

#### 5.1 La forma

`Check` gana un campo opcional. Es un cambio compatible: el protocolo no cambia de versión y un cliente viejo lo ignora.

```rust
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub fix: Option<String>,
    /// What a button can do for `fix`. The CLI does not show it; `--json` carries it.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub action: Option<CheckAction>,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckAction {
    /// Calls to the daemon, one after another; the first that fails stops the rest.
    Calls { calls: Vec<Call> },
    /// An operation of the core that runs in the client's process, so that the dialogs of
    /// Windows (and the UAC) show in front of the person.
    Local { op: LocalOp },
    /// A web page (https only).
    Link { url: String },
    /// The log viewer on this service.
    ShowLog { service: String },
    /// A command to copy. Never run.
    Copy { text: String },
    /// Opens Explorer with this file or folder selected. Only paths inside the home.
    Reveal { path: String },
}

#[serde(rename_all = "snake_case")]
pub enum LocalOp { Trust, UntrustStale, AutostartOff, PathAdd, SystemInstall }
```

- **Una lista cerrada.** `LocalOp` nombra operaciones que ya existen en el núcleo y en `crisol-platform` (`trust::prepare_trust` + `trust_ca`, `trust::untrust_local`, `autostart`, `userpath`, `helper_install::install_self`). Las llama `crisol-client`, igual que el CLI, que ya las ejecuta en su propio proceso. No hay ningún tipo «ejecuta este programa».
- **Lectura tolerante.** Si una GUI más vieja recibe de un daemon más nuevo una acción que no conoce (un `LocalOp` o una `Call` nuevos), se queda sin acción, no sin `doctor`: `lenient` convierte el error en `None`. Hay una prueba que lo comprueba.
- **`fix` y `action` salen del mismo sitio.** El núcleo arma los dos en la misma función, así que no se pueden separar. Una prueba recorre cada `Check` con acción y comprueba que el comando que equivale a la acción es el que dice `fix` (por ejemplo, `Calls [Install { service: "php", version: "8.4.27" }]` y `crisol install php 8.4.27`).
- **El CLI no cambia.** El texto sale igual que hoy, y `--json` trae el campo nuevo. `docs/protocolo.md` y su prueba lo documentan.

#### 5.2 Qué `Check` lleva qué acción

| Comprobación | `fix` de hoy | Acción |
|---|---|---|
| `installed` | `crisol install X` | `Calls [Install]` (con la barra de progreso de Versiones) |
| `known vulnerabilities` | `crisol install php 8.4.x` | `Calls [Install]`. Sin versión que lo arregle (el texto con `--from <zip>`), ninguna |
| `end of life` | `crisol install X` | `Calls [Install]` |
| `unverified installs` (problema) | `crisol uninstall X && crisol install X` | `Calls [Uninstall, Install]` (decisión 2), con segundo clic |
| `unverified installs` (aviso) | «cuando un crisol.exe más nuevo…» | ninguna |
| `services` (alguno falló) | `crisol logs X` | `ShowLog { service: X }` |
| `services` (falta el runtime de Visual C++) | el enlace | `Link` |
| `Visual C++ runtime` | `install it from <url>` | `Link` |
| DNS (regla de Crisol con el daemon parado), `autostart` | `crisol start` | `Calls [Start]` |
| DNS (el nombre no resolvió) | `ipconfig /flushdns, then crisol doctor again` | `Copy { "ipconfig /flushdns" }` |
| `PATH` | `crisol path add` | `Local PathAdd` |
| ayudante | `crisol system install` | `Local SystemInstall` (UAC, decisión 1) |
| `local CA` | `crisol trust` / `crisol untrust --stale` | `Local Trust` / `Local UntrustStale` (diálogo de Windows) |
| `autostart` (entrada rota o ajena) | `crisol autostart off` | `Local AutostartOff` |
| `database startup` (lenta por Defender) | `Add-MpPreference …` | `Copy` (ADR-0026 §6: de administrador y ajeno a Crisol) |
| `configuration`, `ports` | `edit crisol.toml`, «set `port = N`» | `Reveal { crisol.toml }` (decisión 3) |
| `log leftovers` | «léelos y bórralos» | `Reveal` del primer resto, en su carpeta (decisión 3) |
| `site names` (`hosts`), `IPv6` | cambiar `hosts` o el registro | ninguna: es de administrador y no hay un comando exacto |
| el resto | sin `fix` | ninguna |

#### 5.3 Qué hace el botón en la GUI

- **Antes de hacer nada, dice qué va a hacer.** La etiqueta la pone `crisol-client` a partir de la acción («Instalar php 8.4.27», «Confiar en la CA local…», «Instalar el ayudante (pide permiso de administrador)»), y debajo va el `fix` exacto, que es lo que haría el CLI.
- **Hace falta un segundo clic** cuando la acción borra algo (`Uninstall` en la lista) o pide un permiso de Windows (`Trust`, `UntrustStale`, `SystemInstall`). En `Trust`, el segundo paso muestra lo mismo que imprime `crisol trust` antes del diálogo: el nombre, para qué vale, hasta cuándo, la SHA-256 y la SHA-1.
- **`SystemInstall`** (decisión 1) cumple las condiciones de la sección 3: el botón muestra el comando exacto, el UAC es el consentimiento y `uninstall --all` ya quita el ayudante (ADR-0022), así que no hay nada nuevo que anotar en `state/`. Si se dice que no en el UAC, es un resultado y no un error: «no se cambió nada».
- **`Copy`** es un botón *Copiar*, el mismo de Credenciales. **`Link`** abre el navegador con `Desktop::open_url`, que ya existe. **`ShowLog`** cambia a Registros con ese servicio. **`Reveal`** abre el Explorador con el archivo seleccionado (`explorer /select,`), mediante un método nuevo de `Desktop` (`reveal`); no edita nada.
- **Al terminar**, la GUI vuelve a pedir `doctor`, y la fila muestra el resultado nuevo.
- **Qué acción se ofrece, la etiqueta y si pide segundo clic** se deciden en `crisol-client` (`doctor_view`, `doctor_actions`), con pruebas. La página solo dibuja.

#### 5.4 Seguridad

- Las respuestas llegan por la tubería del daemon, que solo acepta la misma cuenta (ADR-0008). Aun así, la GUI no confía en el contenido más de lo necesario:
  - `Calls` son llamadas del protocolo, que el daemon valida igual que si vinieran del CLI;
  - `Local` es una lista cerrada, sin argumentos libres;
  - `Link` solo `https://` y que pase `is_web_address` (que por sí sola admite también `http://`, para los sitios locales);
  - `Copy` nunca se ejecuta;
  - `Reveal` solo abre rutas que `crisol-client` comprueba que están dentro de la home (`CrisolPaths`), ya normalizadas, para que `..` no saque de ella; si no, la fila se queda sin botón.
- Nada se ejecuta sin un clic, y lo que borra o eleva pide dos.

#### 5.5 Decisión 2: reinstalar con dos llamadas o con una (decidido: dos llamadas)

Solo la usa `unverified installs` cuando el hash de lo instalado no es el del catálogo: lo que hay **no es la compilación oficial**.

- **Dos llamadas (`Calls [Uninstall, Install]`).** Es lo que dice `fix` hoy.
  - A favor: no hay nada nuevo en el núcleo, el protocolo ni el CLI. Lo sospechoso se quita primero, aunque después falle la descarga. Si un programa de esa versión está corriendo, `uninstall` se niega («is in use; nothing was removed») y no se toca nada.
  - En contra: si la descarga falla (sin red), la versión se queda sin instalar. Los sitios que la fijan fallan con un error claro hasta que se repita, y los shims caen a la recomendada.
- **Una llamada (`install` con `replace: true` y `crisol install X --replace`).**
  - A favor: descarga y verifica en la carpeta temporal y solo entonces cambia la vieja por la nueva. Si algo falla, todo queda como estaba.
  - En contra: es otra opción del protocolo y del CLI, el `fix` cambia de texto y el reemplazo en Windows también se niega si un programa de la carpeta está corriendo. Y «como estaba» es una compilación que no es la oficial.
- **Decidido: dos llamadas** (propietario, 2026-11-06). Es un caso raro, quitar primero lo que no es oficial es lo prudente, y no agrega superficie. `Calls` ya admite una lista, así que no cuesta nada.

#### 5.6 Decisión 3: abrir un archivo o una carpeta (decidido: `Reveal`)

Lo usarían `configuration` y `ports` (`crisol.toml`) y `log leftovers` (la carpeta de logs).

- **Con una acción `Reveal { path }`:** abre el Explorador con el archivo seleccionado (`explorer /select,`).
  - A favor: un clic lleva al archivo que hay que tocar, y los errores de `crisol.toml` son los más comunes en `doctor`. Seleccionarlo en el Explorador evita que Windows pregunte con qué abrir un `.toml`, si no hay programa asociado.
  - En contra: es un método más en `Desktop` (`reveal`) y otra validación: la GUI solo abre rutas **dentro de la home**, comprobadas con `CrisolPaths`, para que una respuesta no pueda llevar a otro sitio. No edita nada; la persona hace el cambio.
- **Sin acción (solo texto):** nada nuevo, pero la persona tiene que ir sola hasta `crisol.toml` o `logs\`. La ruta ya sale en el detalle.
- **Decidido: `Reveal`, limitado a la home** (propietario, 2026-11-06). La ruta se comprueba en `crisol-client` con `CrisolPaths`; una fuera de la home deja la fila sin botón. Es poco código, el Explorador es lo que espera un usuario de Herd o Laragon, y deja para más adelante una llamada que escriba el puerto libre en `crisol.toml` (necesitaría su propia orden explícita, por la regla de no reescribir la configuración del usuario).

#### 5.7 Lo que F13.7 también recoge

- La GUI dibuja los avisos de `status` (`LiveState.warnings`), que hoy no se ven y solo se actualizan al conectar (revisión de F13.4).
- Cuando falta la versión por defecto, el aviso sale una sola vez y no una por cada sitio (revisión de F13.5).

#### 5.8 Cómo quedó (F13.7, 2026-11-07)

- La forma de §5.1 se implementó tal cual. Además, `CheckAction::command()` da el comando de `crisol` que equivale a la acción, y `Check::action_agrees_with_fix()` es lo que usan las pruebas para exigir que el botón y el `fix` digan lo mismo, en cada comprobación que construyen el núcleo, el daemon y la plataforma.
- `services` con el runtime de Visual C++ ausente lleva `Link` (la página sale del propio mensaje del cargador, solo si es de `aka.ms`); su `fix` no cambia (`crisol logs X`), para que el CLI quede igual.
- `database startup` copia solo el comando de Defender (`SystemChecks::slow_start_command`), no la frase entera del `fix`.
- La etiqueta no viaja traducida: `crisol-client` manda la clave y los valores, y la página la traduce con sus diccionarios, como las demás pantallas.
- Los avisos de `status` (§5.7) necesitaron más: el daemon los devolvía vacíos y ningún evento los actualizaba. Lo resuelve la ADR-0029 (avisos vigentes, evento `warnings`, protocolo 4).
- En *Confiar en la CA*, el primer clic hace `crisol-client::trust_preview`, que crea la CA si no hay (como `crisol trust` antes de preguntar) y no añade nada al almacén; el segundo clic es el que abre el diálogo de Windows.

## Consecuencias

- `doctor` sigue siendo de solo lectura. Lo único que «toca» es:
  - un *bind* efímero en `[::1]:0`;
  - el sondeo de puertos de `PortProbe::probe`, que puede reclamar un momento un puerto libre (por eso no se usa con los servicios que corren);
  - una consulta DNS a un nombre que no existe.
- `Catalog::is_end_of_life` cambia de significado: una versión que el catálogo no lista toma lo que dice de su línea (sin soporte si todas las versiones de esa línea lo están). Lo usan `list`, la resolución de versiones y el `Installed::end_of_life` de `--from`.
- Con el `hosts` real de este equipo (Herd y Laragon), la línea `hosts block` resume en una frase los 46 nombres ajenos en vez de listarlos. Todos apuntan a `127.0.0.1`, así que tampoco darían `site names` aunque coincidieran con un sitio de Crisol.

## Verificación manual (propietario)

Con una home de prueba, nunca `C:\Crisol`. Desde `src/`:

1. `set CRISOL_HOME=D:\scratch\crisol-trust` en `cmd`, `$env:CRISOL_HOME='D:\scratch\crisol-trust'` en PowerShell o `export CRISOL_HOME=/d/scratch/crisol-trust` en Git Bash. Comprueba que la primera línea de `doctor` nombra esa carpeta. Después, `cargo run -p crisol-cli -- trust`. Comprueba que la SHA-1 impresa es la «Huella digital (sha1)» del diálogo de Windows (mismo formato: mayúsculas en grupos de 8). Acepta.
2. `cargo run -p crisol-cli -- doctor`: la línea `local CA` debe salir `ok`.
3. Repite `trust`: «already trusts it», sin diálogo.
4. `cargo run -p crisol-cli -- untrust`: anota si Windows pregunta. Luego `doctor` debe dar `fix: crisol trust`.
5. Opcional: `trust` y después decir «No» en el diálogo. Debe dar código 1 y «nothing was changed».

### Resultado (propietario, 2026-10-29)

Se hizo sin querer sobre la home real `C:\Crisol` y no sobre una de prueba: las instrucciones daban `set`, que no define la variable en Git Bash. `trust` creó ahí `Crisol Local CA CEC769`. Se deja en su sitio: la quitará `uninstall --all` en sus pruebas (F11.2).

- `trust`: Windows mostró **un diálogo**, y el propietario lo aceptó a mano. La SHA-1 impresa (`333D7995 630D8E2E DEED0650 9F3A0E27 95216842`) **coincide** con la del diálogo, en el mismo formato.
- `doctor`: `local CA … is trusted by Windows`.
- `untrust`: Windows **también pide confirmación al quitar** el certificado, y muestra su información. Por eso `uninstall --all` (F11.2) puede mostrar un diálogo por cada CA que quite. Si el usuario dice que no, hoy sale como error de E/S con el texto de Windows.
