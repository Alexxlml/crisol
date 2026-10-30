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

Lo describe `docs/protocolo.md` («Un cliente mínimo» y «Eventos») y lo implementa `crisol-platform/examples/watch.rs`:

1. Durante la llamada a `status`, guarda los eventos que llegan; después aplica la foto y, encima, los de `seq` mayor que el de la respuesta.
2. Después, ignora un evento con `seq` menor o igual que el último aplicado.
3. Si falta un número, pide `status` otra vez.

## Consecuencias

- El CLI no cambia: no sigue eventos de estado.
- `Shared.events` pasa de `broadcast::Sender<Event>` a `Bus` en `crisol-daemon`.
- Una GUI ya no tiene que adivinar si su vista está al día: o los números son seguidos, o sabe que debe volver a pedir el estado.
