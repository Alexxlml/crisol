# ADR-0029 — Avisos vigentes en `status`, evento `warnings` y protocolo 4

- **Estado:** aceptada (propietario, 2026-11-07; revisión de F13.7, hallazgo 1).
- **Fecha:** 2026-11-07

## Contexto

F13.7 hizo que la GUI dibujara `LiveState.warnings`, que sale del campo `warnings` de la respuesta `status`. Al revisarla salieron dos problemas:

- **`status` nunca traía avisos.** El daemon respondía `warnings: []` siempre. Solo la respuesta final de `start` y `restart` los llenaba, con los avisos de ese arranque. La GUI no mostraba, por ejemplo, que la versión por defecto de `crisol.toml` ya no está instalada.
- **Ningún evento cambia los avisos.** Aunque `status` los trajera, solo se leerían al conectar. Tampoco basta con volver a pedir `status` al recibir `reloaded`: un cambio que deja los archivos de Nginx como estaban no anuncia nada. Es el caso de F13.5: se quita el default `php 8.3` y se elige con *Usar esta* `php 8.4`, que ya era la que se usaba.

## Opciones

1. **La GUI vuelve a pedir `status` después de sus propias acciones** y de `reloaded`. No cambia el protocolo, pero no se entera de lo que se hace desde la CLI o a mano en `crisol.toml`, y `status` seguiría vacío.
2. **El daemon guarda los avisos vigentes y emite un evento cuando cambian (elegida).**

## Decisión

- **Avisos vigentes.** El daemon guarda lo que tiene que decir de la pila web en marcha: los avisos del plan (sitios, default que falta), los de los certificados y los de `crisol.toml`, sin repetir ninguno. Los calcula al arrancar (`start`) y cada vez que vuelve a aplicar los archivos del usuario (`refresh`: el vistazo periódico y los cambios de sitios, `use`, `enable`…). Los vacía al parar. `status` los devuelve. La respuesta final de `start` y `restart` sigue llevando los avisos de ese arranque.
- **Evento `warnings { warnings }`**, con la lista entera, solo cuando cambia. El cliente sustituye la suya.
- **La vigilancia mira también las versiones instaladas.** `install` y `uninstall` corren en el trabajador, fuera del actor, así que la huella de vigilancia (`watch_fingerprint`) incluye la lista de lo instalado. Al quitar el default, el siguiente vistazo (cada 2 s) lo nota. Esto también hace que un sitio que fija una versión recién instalada se sirva sin reiniciar.
- **Protocolo 4.** Un evento nuevo no era compatible: un cliente de la versión 3 lo toma por un error de protocolo y corta la conexión. Se sube la versión, y desde la 4 **un evento desconocido se lee como `Event::Unknown`** (`#[serde(other)]`) y su `seq` cuenta. A partir de ahora, añadir un tipo de evento es compatible, igual que añadir un campo; lo dice `docs/protocolo.md`, «Versiones y compatibilidad».

## Consecuencias

- Un CLI o una GUI de la versión 3 frente a un daemon de la 4 (o al revés) reciben `unsupported_protocol`, como en cualquier cambio de versión. Se instalan juntos, así que solo pasa con un daemon viejo que sigue corriendo tras actualizar, y el aviso de versión de la GUI ya dice cómo salir.
- `crisol status` en la CLI también muestra ahora los avisos vigentes.
- Pruebas: `the_warnings_of_the_running_stack_are_in_status_and_said_again_when_they_change` y `removing_the_default_version_is_noticed_by_the_watch` en el daemon, `the_warnings_event_replaces_the_list_and_an_unknown_event_is_counted_not_fatal` en `LiveView` y `an_event_this_version_does_not_know_is_read_with_its_number` en el protocolo.
