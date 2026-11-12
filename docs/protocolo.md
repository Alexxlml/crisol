# Protocolo de `crisold` (versión 4)

Este es el contrato entre el daemon (`crisold`) y cualquier cliente: el CLI hoy, una GUI o un panel web después. La fuente de verdad es el crate `crisol-protocol`; una prueba de ese crate (`the_protocol_document_names_every_method_reply_event_and_error`) falla si este documento deja de nombrar un método, una respuesta, un evento o un código de error, o si la versión no coincide con `PROTOCOL_VERSION`.

## Transporte

- **JSON-RPC 2.0**, un objeto JSON por línea, terminado en `\n`, sin saltos de línea dentro. Una línea de más de **1 MiB** (`MAX_LINE`) cierra la conexión.
- **Una tubería con nombre por home y por usuario:** `\\.\pipe\crisol-<8 hex>-<8 hex>`. El primer grupo son los primeros 4 bytes del SHA-256 del SID del usuario en mayúsculas; el segundo, los del SHA-256 de la ruta de la home en minúsculas, con `\` como separador y sin `\` final (`crisol_core::ipc::pipe_name`).
- **Solo para el usuario:** la tubería tiene una DACL protegida que da acceso al usuario que corre el daemon y a `SYSTEM` (`D:P(A;;GA;;;<SID>)(A;;GA;;;SY)`), rechaza clientes remotos y se crea con `first_pipe_instance`.
- **Lo que debe hacer el cliente** antes de enviar nada. El nombre de la tubería sale de datos que no son secretos, así que, si el daemon no está corriendo, otra cuenta del equipo puede crear antes una tubería con ese nombre. Un cliente que no lo comprueba le enviaría a esa cuenta lo que pida (las contraseñas de `credentials`, por ejemplo) y creería sus respuestas. Por eso:
  1. abre la tubería con `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, para que el servidor no pueda actuar en su nombre;
  2. obtiene el proceso del servidor con `GetNamedPipeServerProcessId`, lo abre con `PROCESS_QUERY_LIMITED_INFORMATION` y lee el SID del usuario de su token;
  3. si no es el SID del propio cliente, cierra la conexión sin escribir nada.

  `crisol_platform::connect_to_daemon` lo hace así; cualquier otro cliente, en Rust o no, debe hacer lo mismo.
- **Rutas como texto**, absolutas, con la forma de Windows.
- Cada conexión atiende **una petición a la vez**: lee una, la responde y lee la siguiente. Un cliente que necesite hacer algo mientras espera una operación larga abre otra conexión.

## Mensajes

**Petición** (cliente → daemon). `id` lo elige el cliente; `params` se omite en los métodos sin parámetros.

```json
{"jsonrpc":"2.0","id":7,"method":"link","params":{"name":"shop","path":"D:\\shop","docroot":"public"}}
```

**Respuesta** (daemon → cliente): el mismo `id` y exactamente uno de `result` y `error`. `id` es `null` si la petición no se pudo ni leer.

```json
{"jsonrpc":"2.0","id":7,"result":{"type":"done","message":"…","changed":true,"warnings":[]}}
{"jsonrpc":"2.0","id":7,"error":{"code":2,"message":"there is no site called `shop`"}}
```

**Notificación** (daemon → cliente), sin `id`:

```json
{"jsonrpc":"2.0","method":"event","params":{"type":"service_state","service":"nginx","state":"running","seq":12}}
```

Para distinguirlas al leer: una notificación tiene `method` y `params`; una respuesta, `id` y `result` o `error`.

## Un cliente mínimo

`crisol-client` (`LiveView`) implementa este orden y `crisol-client/examples/watch.rs` lo usa para mostrar el estado de los servicios mientras cambia (`cargo run -p crisol-client --example watch`). Una carcasa en Rust usa `LiveView`; otra, en otro lenguaje, sigue el mismo orden:

1. `hello`, para saber que el daemon habla esta versión;
2. `subscribe` **antes** de pedir el estado, para que ningún cambio caiga entre los dos;
3. `status`, cuya respuesta sustituye la vista. La respuesta y los eventos llegan por caminos distintos, así que un evento más nuevo que la foto puede llegar antes que ella: los que llegan durante la llamada se guardan, y después de la foto se aplican solo los de `seq` mayor que el `seq` de la respuesta (ver «Eventos»);
4. desde ahí, cada evento actualiza la vista, hasta `shutting_down` o el cierre de la conexión. Uno con `seq` menor o igual al de la foto ya está en ella. Si falta un número, se perdieron eventos y hay que volver al paso 3.

## Versiones y compatibilidad

- **`PROTOCOL_VERSION = 4`.** El primer mensaje de una conexión debería ser `hello` con esa versión. El daemon responde `unsupported_protocol` (código 1) a otra versión. No exige `hello`, pero un cliente que no lo envía no sabe si habla con un daemon compatible.
- **Añadir un campo** a una respuesta o a un evento es compatible. Los clientes **deben ignorar** los campos que no conocen, y los campos opcionales pueden faltar.
- **Añadir un evento** también es compatible desde la versión 4. Los clientes **deben ignorar** una notificación cuyo `type` no conocen, pero cuentan su `seq` (`crisol_protocol::Event::Unknown`). Un cliente de la versión 3 trataba un evento desconocido como un error de protocolo y cortaba la conexión.
- **Cualquier otro cambio sube la versión:** quitar o renombrar algo, cambiar un tipo, o añadir un método, un tipo de respuesta o un código de error. Un cliente que encuentra un `type` de respuesta desconocido no puede entenderla.

## Textos traducibles

El daemon **siempre escribe sus frases en inglés**, en los campos de siempre (`message`, `detail`, `fix`, `warnings`…), y no sabe en qué idioma lee el cliente. Junto a cada frase que se puede traducir (ADR-0030) manda, en un campo opcional `*_text`, un `Text`: `{"key": "doctor.services.running", "args": {"list": "nginx"}}`. `key` es la clave del diccionario del núcleo y `args` son los datos que rellenan los `{marcadores}` de la frase. `args` falta si no hay datos, y los datos son siempre texto ya formateado (una ruta, una versión, un número). Lo que no se traduce (comandos, rutas, versiones, nombres de servicios y de sitios) va como dato, nunca dentro de la frase.

| Dónde | Campos |
|---|---|
| `error` | `text` (de `message`) y `fix_text` (de `fix`) |
| `done` | `message_text` y `warnings_text` |
| `status` | `warnings_text` |
| cada comprobación de `doctor` | `name_text`, `detail_text` y `fix_text` |

- `warnings_text` es una lista de `Text` o `null`, vacía o del mismo largo que `warnings`; si el largo no coincide, el cliente la ignora entera.
- `name` de una comprobación sigue siendo el identificador inglés de la fila; `name_text` es su nombre para mostrar.
- **Cómo lo lee el cliente:** dice el `Text` en su idioma. Si el diccionario de ese idioma no tiene la clave, la dice en inglés; si ningún diccionario la tiene (un daemon más nuevo que el cliente), muestra la frase inglesa del campo de siempre. Nunca muestra una clave. Un `*_text` que no entiende se lee como ausente.
- **Plurales:** la clave `x` se escribe como `x_one` y `x_other`, y se elige con el dato `count` (`one` si vale `1`).
- **Compatibilidad:** los campos son opcionales y el protocolo sigue en la versión 4. Un cliente que no los conoce los ignora y muestra el inglés; un daemon que no los manda da inglés.
- **`--json` no los lleva.** La CLI los quita antes de imprimir (`strip_texts`), así que su salida es la de siempre: inglés y sin campos nuevos.

## Errores

`error` es `{"code": <número>, "message": <texto>, "fix"?: <texto>, "text"?: <Text>, "fix_text"?: <Text>}`. `message` está listo para mostrarse tal cual. `fix`, si viene, es lo exacto que hay que hacer: un comando, por ejemplo el `icacls` que cierra la home.

| Código | Nombre | Cuándo |
|---|---|---|
| -32700 | `parse_error` | La línea no es JSON, o no es una petición de este protocolo. |
| -32600 | `invalid_request` | El JSON no es una petición válida (`jsonrpc` distinto de `"2.0"`). |
| -32601 | `method_not_found` | El método no existe. |
| -32602 | `invalid_params` | Los parámetros no valen para el método: una versión mal escrita, un nombre de sitio inválido… |
| -32603 | `internal` | El daemon falló por su culpa, o se está cerrando. |
| 1 | `unsupported_protocol` | El cliente habla otra versión del protocolo. |
| 2 | `not_found` | No existe algo que la petición nombra: una versión, un sitio, un servicio o la home. |
| 3 | `conflict` | La petición está bien pero no se puede en este estado: una versión en uso, un servicio que no corre, un sitio que ya existe. |
| 4 | `home_exposed` | Otras cuentas pueden escribir en la home y Crisol no ejecuta ni genera nada desde ella; `fix` trae el comando. |
| 5 | `io` | Falló un archivo o el sistema operativo. |
| 6 | `not_available` | Hace falta algo que no está instalado o configurado (el daemon, nginx, el ayudante…). |

En la serialización, el código viaja como número; los nombres de la tabla son los de `ErrorCode` y solo sirven para leer este documento.

## Métodos

Cada método indica sus parámetros (`?` = opcional), la respuesta que da y si **necesita el daemon**. Los que no lo necesitan solo cambian archivos de la home: un cliente que no encuentra el daemon puede hacerlos con el mismo núcleo en su proceso, como hace el CLI. Los que **escriben** crean la home (privada) si no existe; los demás responden aunque no exista.

### `hello`

- **Parámetros:** `client` (texto, para el log, p. ej. `crisol 0.1.0`) y `protocol` (número).
- **Respuesta:** `hello`. **Necesita el daemon.**

### `status`

- **Parámetros:** `service?`: `nginx`, `php-8.4.26`, `mysql`…; sin él, todos.
- **Respuesta:** `status`. **Necesita el daemon.** `not_found` si la home no existe.

### `start`

- **Parámetros:** `service?`: un servicio de datos (`mysql`, `redis`…), o nada para la pila web y los servicios de datos activados.
- **Respuesta:** `done`, con avisos de las bases que fallaron. **Necesita el daemon.** Escribe.
- Mientras arranca se emiten eventos `service_state` a los suscriptores.

### `stop`

- **Parámetros:** `service?`.
- **Respuesta:** `done`. **Necesita el daemon.** El daemon sigue vivo.

### `restart`

- **Parámetros:** `service?`.
- **Respuesta:** `done`. **Necesita el daemon.** Escribe.

### `shutdown`

- **Sin parámetros.** Para todo lo que el daemon arrancó, y el daemon termina.
- **Respuesta:** `done`, con avisos (por ejemplo, que los nombres locales dejan de resolverse). **Necesita el daemon.**
- Emite `shutting_down`. Después de la respuesta, la conexión se cierra.

### `install`

- **Parámetros:**
  - `service`: `php`, `nginx`, `node`, `mysql`…;
  - `version?`: `8.4`, `8.4.26` o `latest`; sin ella, la línea que recomienda el catálogo (ADR-0012);
  - `from?`: ruta absoluta de un ZIP ya descargado (ADR-0020); exige `version` exacta y `sha256`;
  - `sha256?`.
- **Respuesta:** `installed`. Escribe. Es una operación larga.
- Mientras descarga, el daemon envía eventos `progress` **por la misma conexión**, aunque no esté suscrita, entre la petición y la respuesta.

### `uninstall`

- **Parámetros:** `service` y `version` (exacta, o un prefijo que coincida con una sola).
- **Respuesta:** `removed`. Escribe. `conflict` si la versión está en uso.

### `list`

- **Parámetros:** `available?` (booleano, por defecto `false`): incluye lo que ofrece el catálogo.
- **Respuesta:** `versions`.

### `use`

- **Parámetros:** `tool` (`php` o `node`), `version` y `dir?` (carpeta de un proyecto; sin ella, el predeterminado global).
- **Respuesta:** `used`. Escribe.

### `park`

- **Parámetros:** `path` (absoluta). Sirve como sitio cada subcarpeta.
- **Respuesta:** `done`. Escribe. Con el daemon corriendo, la configuración web se recarga.

### `unpark`

- **Parámetros:** `path`, como se aparcó.
- **Respuesta:** `done`. Escribe.

### `link`

- **Parámetros:** `name` (`shop` para `shop.test`), `path` (absoluta) y `docroot?` (la subcarpeta servida, si no se detecta).
- **Respuesta:** `done`. Escribe.

### `unlink`

- **Parámetros:** `name`.
- **Respuesta:** `done`. Escribe.

### `sites`

- **Sin parámetros.**
- **Respuesta:** `sites`.

### `secure`

- **Parámetros:** `name`. Sirve el sitio también por HTTPS; crea la CA local si falta.
- **Respuesta:** `done`. Escribe.

### `unsecure`

- **Parámetros:** `name`.
- **Respuesta:** `done`. Escribe.

### `logs`

- **Parámetros:**
  - `service`: `nginx`, `php-8.4.26`, `crisold`…;
  - `lines?`: cuántas líneas del final, por defecto 50;
  - `from?`: desplazamiento en bytes, el `next` de la respuesta anterior.
- **Respuesta:** `logs`.
- Para seguir un log se pide otra vez con `from`, que es lo que hace `crisol logs -f`.

### `doctor`

- **Sin parámetros.** Revisa la instalación; solo lee (ADR-0021).
- **Respuesta:** `doctor`. Es una operación larga.

### `credentials`

- **Parámetros:** `service` (`mysql`, `postgresql`, `redis`, `meilisearch`…).
- **Respuesta:** `credentials`. No arranca nada.

### `enable`

- **Parámetros:** `service`. Pone `enabled = true` en `crisol.toml`; el siguiente `start` lo levanta.
- **Respuesta:** `done`. Escribe.

### `disable`

- **Parámetros:** `service`. Pone `enabled = false`; si estaba corriendo, sigue hasta que se pare.
- **Respuesta:** `done`. Escribe.

### `reset_password`

- **Parámetros:** `service`.
- **Respuesta:** `done`. **Necesita el daemon.** Escribe.
- Cambia la contraseña con el servicio corriendo (`ALTER USER`, `ALTER ROLE`, `CONFIG SET`…) y luego la guarda en `secrets/`. Garnet y Meilisearch solo la leen al arrancar: hay que pararlos antes; si corren, el error lo dice.

### `subscribe`

- **Sin parámetros.**
- **Respuesta:** `done`, con `changed` en `true`. Suscribirse dos veces no duplica los eventos. **Necesita el daemon.**
- Desde entonces, el daemon envía por esa conexión todos sus **eventos**, intercalados con las respuestas.
- Un suscriptor que no lee a tiempo **pierde eventos, no la conexión**. En cambio, un cliente que no lee ni sus respuestas es desconectado.

## Respuestas

`result` es un objeto con `type` y los campos de ese tipo. Los campos marcados con `?` pueden faltar.

### `hello`

`daemon` (versión de `crisold`), `protocol` y `home` (la ruta de su home).

### `status`

- `services`: lista, en orden estable, de `{name, state, detail?}`. `state` es `stopped`, `starting`, `running`, `stopping` o `failed`; `detail` dice por qué falló.
- `warnings_text?`: las frases de `warnings` con clave y datos (ver «Textos traducibles»).
- `warnings`: lo que el daemon tiene que decir de la pila web en marcha y que sigue vigente. Son los avisos de su plan (sitios, la versión por defecto que ya no está instalada), los de los certificados y los de `crisol.toml`. Es la misma lista que da el evento `warnings`, y queda vacía con la pila parada. En la respuesta final de `start` y `restart`, son en cambio los avisos de ese arranque.
- `seq?`: el número del último evento que la foto ya refleja (ver «Eventos»). Lo pone el daemon también en la respuesta final de `start` y `restart`; falta cuando contesta el CLI sin daemon.
- `blocked?`: el último `blocked` que sigue vigente (ver «Eventos»). Falta si no hay nada bloqueado.
- `reload_error?`: el último `reload_failed` que sigue vigente: la configuración web nueva no se aplicó y sigue la anterior. Falta si la última se aplicó.

Los dos siguen la misma regla que un cliente aplica a los eventos (`crisol_core::trouble::WebTrouble`): `blocked` y `reload_failed` se recuerdan; `reload_started` olvida el último fallo; `reloaded` olvida los dos; y que `nginx` pase a `starting` también olvida los dos, porque un arranque nuevo vuelve a probar la configuración y dice lo que falle **después** de ese evento. Con eso, un cliente que llega tarde, o que perdió eventos, ve en la foto lo mismo que habría deducido de ellos.

### `done`

`message` (listo para mostrar), `changed` (`false` si la orden repetida no cambió nada) y `warnings`. `message_text?` y `warnings_text?` traen las mismas frases con clave y datos (ver «Textos traducibles»).

### `installed`

- `service`, `version` y `path` (`bin/<servicio>/<versión>`).
- `already`: ya estaba instalada y no se descargó nada.
- `end_of_life`, `downloaded` (bytes).
- `unverified`: instalada con `from` (ADR-0020).
- `advisories`: lista de `{id, summary, url, fixed_in?}`.

### `removed`

`service` y `version`.

### `versions`

- `installed` y `available`: listas de `{service, version, installed, recommended, end_of_life, unverified, advisories}`. `advisories` son ids.
- `available` está vacía si no se pidió.

### `used`

- `file`: el archivo que guarda la elección.
- `request`: lo que se escribió.
- `selects`: la versión instalada que elige ahora.
- `previous?`, `changed` y `warnings`.

### `sites`

- `sites`: lista de `{name, domain, root, docroot, kind, secure}`. `kind` es `parked` o `linked`.
- `warnings`: carpetas y entradas que se dejaron fuera, con el motivo.

### `logs`

`lines` (sin saltos de línea), `next` (el `from` de la siguiente petición) y `path`.

### `doctor`

`checks`: lista de `{name, status, detail, fix?}`, en orden, cada una con `name_text?`, `detail_text?` y `fix_text?` (ver «Textos traducibles»). `status` es `ok`, `warning` o `problem`; `fix` es lo exacto que hay que hacer.

`action?` (ADR-0021 §5) es lo que un botón puede hacer por `fix`. El CLI no la muestra; `--json` la trae. Es un objeto con `kind`:

- `calls`: `{calls: [<llamada>, …]}`, llamadas del protocolo una tras otra; la primera que falla detiene las demás.
- `local`: `{op}`, una operación del núcleo que el cliente ejecuta en su propio proceso, para que los diálogos de Windows (y el UAC) salgan delante de la persona. `op` es `trust`, `untrust_stale`, `autostart_off`, `path_add` o `system_install`; la lista es cerrada y no lleva argumentos.
- `link`: `{url}`, una página web (solo `https`).
- `show_log`: `{service}`, el visor de registros de ese servicio.
- `copy`: `{text}`, un comando para copiar. Nunca se ejecuta.
- `reveal`: `{path}`, abrir el Explorador con ese archivo o carpeta seleccionados; solo rutas dentro de la home.

Agregar un `kind` o un `op` es compatible: un cliente que no conoce la acción la lee como si no hubiera (`action` ausente) y conserva el resto de la comprobación. `fix` y `action` salen de la misma función del núcleo, y una prueba comprueba que el comando equivalente a la acción es el que dice `fix`.

### `credentials`

`service`, `lines` (lista de `{key, value}`, las líneas de un `.env`) y `notes`.

## Eventos

`params` de una notificación es un objeto con `type`, los campos de ese tipo y `seq?`.

**`seq`, el número del evento.** El daemon numera sus eventos desde 1, de uno en uno, en el orden en que los envía a todos los suscriptores. La respuesta `status` lleva el `seq` del último evento enviado antes de hacer la foto, y la foto ya refleja ese evento y los anteriores. Con eso, un cliente sabe:

- **qué eventos ya están en la foto:** los de `seq` menor o igual al de la respuesta. La respuesta y los eventos llegan a la conexión por caminos distintos, así que un evento más nuevo que la foto puede llegar antes que ella, y uno más viejo, después;
- **que perdió eventos:** a un cliente que no lee a tiempo el daemon le descarta eventos y no le cierra la conexión, así que un salto en la numeración significa que debe pedir `status` otra vez.

El progreso de una operación que solo va a quien la pidió (`progress` durante `install`) no lleva `seq` y no cuenta para la numeración. Un daemon anterior a este campo no lo envía: entonces la respuesta `status` sustituye lo que dijeran los eventos llegados antes de ella, sin más garantías.

### `service_state`

`service`, `state` (los mismos de `status`) y `detail?`.

### `reload_started`

Se está probando una configuración web nueva (empezó `nginx -t`).

Cada `reload_started` va seguido de `reloaded` o de `reload_failed`, también si la pila se para a mitad de la prueba (entonces `reload_failed` lo dice), salvo que el daemon se vaya (`shutting_down`). Un cliente puede mostrar «recargando» entre los dos. Un cambio que deja los archivos como ya estaban no anuncia nada. `reload_failed` puede llegar sin `reload_started`: el fallo fue antes de probar (al preparar los archivos, esperando los procesos nuevos de PHP o en el primer arranque).

### `reloaded`

Nginx aceptó la configuración y se le pidió recargarla.

### `reload_failed`

`reason`: la configuración nueva no se aplicó, y sigue la anterior.

### `blocked`

`reason`: Crisol no cambiará la pila web en marcha hasta que se cierre una carpeta. El texto trae el comando y se muestra tal cual.

Se dice una vez por motivo. El daemon vuelve a intentar el cambio en cada vistazo a los archivos (cada 2 s), así que, en cuanto la carpeta se cierra, el cambio se aplica y `reloaded` levanta el bloqueo, sin que el usuario toque nada más. Un `start` con la carpeta abierta no emite `blocked`: responde con el error `home_exposed`.

### `log`

`service` y `line`: una línea nueva en el log de un servicio.

### `progress`

`what` (`php 8.4.26`), `done` (bytes), `total?` y `from_file?` (`true` si los bytes vienen de un archivo del usuario, `install --from`).

Es el único evento que llega también **sin suscripción**, a la conexión que pidió el `install`.

### `warnings`

`warnings`: la lista entera de avisos vigentes, la misma que daría ahora `status`. Llega cada vez que cambia: al arrancar o parar la pila web, y cuando el daemon vuelve a mirar los archivos del usuario (`crisol.toml`, los sitios, las versiones instaladas) o atiende un `use`, `link` u otro cambio de los sitios. Por ejemplo, después de `use php 8.4`, si el default que faltaba era `php 8.3`. Una lista vacía significa que ya no queda ninguno. Un cliente sustituye la suya; no hay que sumar.

### `shutting_down`

El daemon se va. Llega después del `stopped` de cada servicio (o de matarlos, si no pararon a tiempo) y es el último evento: un cliente puede dejar de leer aquí sin perderse nada.

## Lo que no está en el protocolo

Algunas cosas no pasan por el daemon y no forman parte de este contrato. Una GUI llamaría a las mismas funciones del núcleo, en su propio proceso:

- **`trust`/`untrust`:** el diálogo de Windows tiene que salir delante del usuario.
- **`setup`:** no usa el daemon. Una GUI hace lo mismo que el CLI con el núcleo: `onboarding::plan` (solo lee), `onboarding::run_if_agreed` y, para los pasos que se preguntan uno por uno (`Step::optional`: la CA, el ayudante y el autoarranque), la respuesta de cada uno. El `Plan` (pasos, lo que ya está hecho, problemas que impiden empezar y notas) y cada `Outcome` se serializan a JSON con la misma forma que `crisol --json setup`.
- **`uninstall --all`:** para el daemon. Una GUI hace lo mismo que el CLI con el núcleo: `teardown::plan` (solo lee) y `teardown::run_if_agreed`, que le pasa el `Plan` completo (pasos, lo que se hace a mano, lo que no se pudo comprobar, lo que se queda y, con purga, lo que se borra) y no cambia nada sin un sí. El `Plan` y cada `Outcome` se serializan a JSON con la misma forma que `crisol --json uninstall --all`.
- **`system install|uninstall`:** UAC.
- **`autostart`, `shims`, `path`, `env` y `shell`:** son ajustes del usuario.
