# ADR-0009 — El pool de `php-cgi` se reinicia por worker

- **Estado:** aceptada (decidida por el usuario el 2026-10-14, a raíz de la revisión externa, hallazgo 5); **implementada** con F5.3 el 2026-10-14
- **Fecha:** 2026-10-14

## Contexto

En Windows no hay php-fpm. El daemon mantiene un pool de procesos `php-cgi.exe` por versión, cada uno en su puerto, y Nginx los usa como `upstream` (`docs/arquitectura.md`, F5.3). Con `PHP_FCGI_MAX_REQUESTS`, cada worker **termina solo con código 0** después de N peticiones: es el reciclaje normal, no una falla. Lo habitual es reiniciar cada worker caído, de modo que la caída de algunos no tumbe el servicio.

Hoy `Service` (F2.5) trata el grupo como una unidad: solo detecta una caída cuando muere **todo** el grupo (`survey.alive == 0`). Con 4 workers, si mueren 3, el servicio sigue `Running` y Nginx devuelve 502 en 3 de cada 4 peticiones.

Se consideraron tres opciones: reinicio por worker; reinicio de todo el pool ante cualquier salida (simple, pero cada reciclaje normal corta el servicio y cuenta como caída); y reinicio por worker sin reciclaje (`PHP_FCGI_MAX_REQUESTS = 0`, cualquier salida es caída; reglas más simples, pero sin control de fugas de memoria).

## Decisión

- **Se reinicia solo el worker que terminó**, dentro del mismo servicio; los demás siguen atendiendo.
- **Una salida con código 0 de un worker del pool es reciclaje:** se relanza al momento, sin la espera de reinicio y **sin contar como caída**.
- **Una salida con cualquier otro código es una caída** de ese worker. Se relanza después de `restart_delay` y cuenta para `max_crashes` dentro de `crash_window`, a nivel del servicio. Si se supera el límite, el servicio pasa a `Failed(TooManyCrashes)`.
- **El servicio sigue `Running` mientras quede al menos un worker vivo.** Si faltan workers, emite un evento de **degradado** con cuántos están vivos y cuántos se esperan, y otro cuando se recupera.
- Si mueren todos, se comporta como hoy: es una caída del servicio.
- Durante el arranque se mantiene la regla actual: un arranque pedido por el usuario que muere antes de estar sano no se reintenta.

## Consecuencias

- La máquina de estados de `Service` necesita saber qué procesos del grupo forman un pool y seguir a cada uno por separado (hoy solo mira el conteo del grupo). Se necesitan pruebas con `FakeSupervisor` para: reciclaje con código 0, caída parcial, degradado y recuperación, límite de caídas sumando workers, y muerte total.
- La regla "código 0 = reciclaje" aplica **solo** a los workers de un pool. En un servicio de un solo proceso (Nginx, MySQL), una salida inesperada con 0 sigue siendo una caída.
- Un reciclaje no debería pasar por `Starting`: el servicio no cambia de estado, solo se relanza el proceso.
- `docs/arquitectura.md` ya describe "reinicia procesos caídos y recicla cada N peticiones"; esta ADR fija el detalle. Se implementa con F5.3 (pool de `php-cgi`), no con F4.1.
