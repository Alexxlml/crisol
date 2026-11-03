# ADR-0007 — Una herramienta local a la vez

- **Estado:** aceptada
- **Fecha:** 2026-10-14

## Contexto

En el equipo del usuario conviven Herd, Laragon, Local y Devaron, y sus bloques en el archivo `hosts` ya comparten el sufijo `.test`. Además, MySQL y MariaDB usan el mismo puerto por defecto (3306). El usuario aclara que en la práctica nadie ejecuta dos suites locales a la vez, ni MySQL y MariaDB juntos.

## Decisión

- El sufijo por defecto sigue siendo **`test`**. Crisol no intenta repartirse nombres con otras herramientas.
- Crisol asume que **solo una suite local corre a la vez** (Herd, Laragon, etc.). `crisol doctor` (F11.1) **advierte** si detecta otra suite activa o dominios `.test` definidos fuera de su bloque, y recomienda usar una sola.
- **MySQL y MariaDB son alternativas excluyentes** que comparten el puerto 3306: se puede instalar ambas, pero solo una activa. Crisol avisa al intentar arrancar la segunda. MySQL (o MariaDB) con PostgreSQL sí pueden correr juntos: usan puertos distintos.
- Una colisión de puerto nunca se resuelve matando procesos ajenos (ya fijado en el plan, F2.4).

## Consecuencias

- F1.2 define un solo puerto por defecto por servicio, sin lógica de reparto.
- F9.1 incluye la comprobación de exclusión MySQL/MariaDB.
- F11.1 incluye la detección de otras suites y de nombres duplicados en `hosts`.

## Ampliación (2026-11-03): el puerto propio que Windows aún no libera

En la revisión de F13.4, un *Reiniciar* de Mailpit terminó una vez en «port 8025 is already in use by unknown (pid …)». Un reinicio es una parada seguida en el mismo tick de un arranque. La parada termina cuando el Job Object ya no tiene procesos, pero Windows puede seguir mostrando un momento la fila del puerto en escucha con el pid del proceso que acaba de terminar. «unknown» es la pista: el nombre de ese pid ya no se puede leer.

- **Se espera al puerto propio, nunca a uno ajeno.** Si al arrancar todos los dueños de un puerto ocupado son procesos de la última ejecución de ese servicio, o procesos que Windows no sabe nombrar (ya terminaron), el servicio se queda en `starting` y vuelve a mirar en cada tick, hasta `LifecyclePolicy::port_release_wait` (3 s). Después falla con el puerto ocupado, como antes.
- **Un puerto con un dueño ajeno con nombre** (Herd, Devaron) falla al instante, aunque haya también una fila propia. Un servicio que nunca corrió no espera a nadie.
- Los pids se recuerdan desde que termina una ejecución hasta que empieza la siguiente. Una parada durante la espera la cancela sin lanzar nada.
- Sigue sin liberarse nada por la fuerza: solo se espera.
