# Protocolo de `crisold` (versión 3)

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

`crisol-platform/examples/watch.rs` es un cliente completo en unas 150 líneas: se suscribe y muestra el estado de los servicios mientras cambia (`cargo run -p crisol-platform --example watch`). Sigue el orden que debería seguir cualquier carcasa:

1. `hello`, para saber que el daemon habla esta versión;
2. `subscribe` **antes** de pedir el estado, para que ningún cambio caiga entre los dos;
3. `status`, cuya respuesta sustituye la vista. La respuesta y los eventos llegan por caminos distintos, así que un evento más nuevo que la foto puede llegar antes que ella: los que llegan durante la llamada se guardan, y después de la foto se aplican solo los de `seq` mayor que el `seq` de la respuesta (ver «Eventos»);
4. desde ahí, cada evento actualiza la vista, hasta `shutting_down` o el cierre de la conexión. Uno con `seq` menor o igual al de la foto ya está en ella. Si falta un número, se perdieron eventos y hay que volver al paso 3.

## Versiones y compatibilidad

- **`PROTOCOL_VERSION = 3`.** El primer mensaje de una conexión debería ser `hello` con esa versión. El daemon responde `unsupported_protocol` (código 1) a otra versión. No exige `hello`, pero un cliente que no lo envía no sabe si habla con un daemon compatible.
- **Añadir un campo** a una respuesta o a un evento es compatible. Los clientes **deben ignorar** los campos que no conocen, y los campos opcionales pueden faltar.
- **Cualquier otro cambio sube la versión:** quitar o renombrar algo, cambiar un tipo, o añadir un método, un tipo de respuesta, un evento o un código de error. Un cliente que encuentra un `type` desconocido no puede entender la respuesta.

## Errores

`error` es `{"code": <número>, "message": <texto>, "fix"?: <texto>}`. `message` está listo para mostrarse tal cual. `fix`, si viene, es lo exacto que hay que hacer: un comando, por ejemplo el `icacls` que cierra la home.

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
- `warnings`: textos.
- `seq?`: el número del último evento que la foto ya refleja (ver «Eventos»). Lo pone el daemon también en la respuesta final de `start` y `restart`; falta cuando contesta el CLI sin daemon.

### `done`

`message` (listo para mostrar), `changed` (`false` si la orden repetida no cambió nada) y `warnings`.

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

`checks`: lista de `{name, status, detail, fix?}`, en orden. `status` es `ok`, `warning` o `problem`; `fix` es lo exacto que hay que hacer.

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

`reason`: Crisol no arrancará ni cambiará nada hasta que se cierre una carpeta. El texto trae el comando y se muestra tal cual.

### `log`

`service` y `line`: una línea nueva en el log de un servicio.

### `progress`

`what` (`php 8.4.26`), `done` (bytes), `total?` y `from_file?` (`true` si los bytes vienen de un archivo del usuario, `install --from`).

Es el único evento que llega también **sin suscripción**, a la conexión que pidió el `install`.

### `shutting_down`

El daemon se va. Llega después del `stopped` de cada servicio (o de matarlos, si no pararon a tiempo) y es el último evento: un cliente puede dejar de leer aquí sin perderse nada.

## Lo que no está en el protocolo

Algunas cosas no pasan por el daemon y no forman parte de este contrato. Una GUI llamaría a las mismas funciones del núcleo, en su propio proceso:

- **`trust`/`untrust`:** el diálogo de Windows tiene que salir delante del usuario.
- **`uninstall --all`:** para el daemon. Una GUI hace lo mismo que el CLI con el núcleo: `teardown::plan` (solo lee) y `teardown::run_if_agreed`, que le pasa el `Plan` completo (pasos, lo que se hace a mano, lo que no se pudo comprobar, lo que se queda y, con purga, lo que se borra) y no cambia nada sin un sí. El `Plan` y cada `Outcome` se serializan a JSON con la misma forma que `crisol --json uninstall --all`.
- **`system install|uninstall`:** UAC.
- **`autostart`, `shims`, `path`, `env` y `shell`:** son ajustes del usuario.
