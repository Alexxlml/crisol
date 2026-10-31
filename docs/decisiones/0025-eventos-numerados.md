# ADR-0025 — Eventos numerados y `seq` en la respuesta `status`

- **Estado:** aceptada (revisión de la Fase 12, hallazgo 1).
- **Fecha:** 2026-10-30

## Contexto

Una carcasa (GUI, panel, `watch`) se suscribe, pide `status` y desde ahí sigue los eventos. El protocolo decía que la respuesta a `status` «sustituye lo que dijeran los eventos llegados antes de ella». Eso no es cierto en `crisold`:

- El actor contesta por un `oneshot` y envía los eventos por un `broadcast`. En la conexión, la respuesta la escribe la tarea de la llamada (`ask`) y los eventos la del reenvío (`forward_events`). Entre las dos no hay orden.
- Si un tic emite un evento justo después de la foto, el evento puede llegar **antes** que la respuesta. El cliente lo aplica y la foto, que es más vieja, lo pisa. La vista se queda mal hasta el siguiente cambio, que puede no llegar.
- Además, a un suscriptor que no lee a tiempo se le descartan eventos sin avisarle (`Lagged`, o la cola de la conexión llena), y no tenía forma de saberlo.

## Opciones

1. **Ordenar en el daemon:** que la conexión no escriba la respuesta hasta haber escrito los eventos anteriores a la foto, y retenga los posteriores. Hace falta coordinar las dos tareas de cada conexión, retener eventos durante llamadas largas, y no resuelve la pérdida por un suscriptor lento.
2. **Numerar los eventos (elegida):** cada evento del flujo lleva `seq`, y la respuesta `status` lleva el `seq` del último evento que la foto ya refleja. El cliente ordena con esos números y detecta los huecos.

## Decisión

- `params` de cada notificación lleva **`seq`**: un número que empieza en 1 y sube de uno en uno, en el orden de envío. Lo asigna `crisol_daemon::Bus` bajo un candado, al enviar, sea el actor o el trabajador quien envía.
- La respuesta **`status`** lleva **`seq`**: el último número enviado **antes** de hacer la foto. Tomarlo antes erra del lado seguro: un evento del trabajador enviado en medio (solo progreso) se vuelve a aplicar, y ninguno se salta. Va también en la respuesta final de `start` y `restart`, que es un `status`.
- El progreso de una llamada que solo va a quien la pidió no lleva `seq`.
- Los dos campos son **añadidos**: según las reglas del protocolo es un cambio compatible, y `PROTOCOL_VERSION` sigue en 3. Un cliente viejo los ignora, y un cliente nuevo frente a un daemon viejo se comporta como antes.
- `crisol-protocol` expone `Numbered` (el evento con su `seq`), `Notification::numbered`, `Client::call_with_numbered` y `Client::next_numbered`. Las funciones anteriores siguen y descartan el número.

## Lo que hace un cliente

Lo describe `docs/protocolo.md` («Un cliente mínimo» y «Eventos») y lo implementa `crisol-client` (`LiveView`; antes, `crisol-platform/examples/watch.rs`):

1. Durante la llamada a `status`, guarda los eventos que llegan; después aplica la foto y, encima, los de `seq` mayor que el de la respuesta.
2. Después, ignora un evento con `seq` menor o igual que el último aplicado.
3. Si falta un número, pide `status` otra vez.

## Consecuencias

- El CLI no cambia: no sigue eventos de estado.
- `Shared.events` pasa de `broadcast::Sender<Event>` a `Bus` en `crisol-daemon`.
- Una GUI ya no tiene que adivinar si su vista está al día: o los números son seguidos, o sabe que debe volver a pedir el estado.

## Ampliación (2026-10-31): lo que sigue vigente va en la foto

Revisión de F13.1 y F13.2, hallazgo 1 (`claude/revisiones/2026-10-31-opus-f131-f132.md`). Aceptada por el propietario el 2026-10-31.

**Problema.** `blocked` y `reload_failed` solo existían como eventos, y la foto de `status` no los traía. Una GUI que se abre después de un bloqueo, o que se reconecta, mostraba «todo bien» mientras Crisol se negaba a aplicar cambios. Tras un salto de `seq`, un aviso podía quedarse puesto sin motivo, o faltar. Además, un bloqueo no se levantaba solo: cerrar la carpeta no cambia ningún archivo vigilado, así que el cambio pendiente no se reintentaba y no llegaba nunca el `reloaded`.

**Decisión.**

- `StatusInfo` gana **`blocked?`** y **`reload_error?`**, opcionales. Es un cambio compatible y el protocolo sigue en la versión 3.
- Una sola regla, `crisol_core::trouble::WebTrouble::note`, decide qué sigue vigente. El daemon la aplica a todos sus eventos para hacer su foto, y `LiveState` la aplica a los que recibe, así que no pueden contradecirse:
  - `blocked` y `reload_failed` se recuerdan;
  - `reload_started` olvida el último fallo;
  - `reloaded` olvida los dos;
  - `nginx` → `starting` olvida los dos.
- Para que la última regla no borre el fallo de un primer arranque que vuelve a la configuración anterior, `WebStack::fail_reload` arranca Nginx **antes** de decir `reload_failed`.
- Cuando el daemon encuentra la home abierta al aplicar un cambio, olvida la huella de los archivos y lo reintenta en cada vistazo (cada 2 s). `blocked` se dice una vez por motivo, no en cada vistazo. Al cerrarse la carpeta, el cambio se aplica y `reloaded` levanta el bloqueo.

**Lo que no cubre.** Si el vigilante de servicios reinicia Nginx tras una caída, el `starting` borra un `reload_error` aunque la configuración nueva siga sin aplicarse. Es raro, y el siguiente cambio de archivos vuelve a probarla.
