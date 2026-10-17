# ADR-0016 — Servicios de datos (bases de datos y extras)

- **Estado:** aceptada (F9.1 implementada; las siguientes fases la extienden)
- **Fecha:** 2026-10-16

## Contexto

La Fase 9 añade MySQL, MariaDB, PostgreSQL y otros servicios que, a diferencia de Nginx y PHP, guardan **datos del usuario**. Un fallo aquí no es un reinicio: es una base corrupta. ADR-0006 (sin secretos en la config), ADR-0007 (MySQL y MariaDB excluyentes) y ADR-0008 (home privada) ya fijan parte del marco.

## Decisión

1. **Cada base de datos es un servicio propio** (`DbService` dentro de un `DbStack`), no parte de la pila web. `crisol start` arranca la pila web y las bases con `enabled = true`; `crisol start mysql` / `stop mysql` / `restart mysql` arrancan una sola. **Una que no puede arrancar** (puerto ocupado por Herd o Devaron, instalación ausente, inicialización fallida) queda `failed` en `status` con su razón y es un **aviso** en el `start` general: no impide que la pila web ni las demás funcionen. Si no hay Nginx pero hay una base habilitada, `start` arranca las bases y avisa.
2. **Exclusión por puerto, no por nombre.** MySQL y MariaDB comparten 3306 por defecto, así que arrancar la segunda con la primera activa se rechaza (`Conflict`, quién tiene el puerto y `crisol stop <la otra>`). Si el usuario les dio puertos distintos, pueden convivir: no hay colisión que evitar. Una colisión con un programa ajeno nunca se resuelve matándolo (ADR-0007): la falla nombra al dueño del puerto.
3. **Datos separados por serie** (F9.5): `data/<servicio>/<mayor.menor>/` (`8.4`, `11.8`, `10.11`). Dos versiones de series distintas no comparten ni corrompen datos; subir de serie es una carpeta nueva, nunca una actualización en el sitio. La configuración generada es `etc/<servicio>/<serie>/my.ini`; lo del usuario va en `etc/<servicio>/overrides/*.ini` (incluido con `!includedir`, nunca tocado).
4. **Inicialización una sola vez y atómica.** Si no existe `data/<servicio>/<serie>/`, se inicializa en `…/<serie>.init/` (MySQL: `mysqld --initialize-insecure`; MariaDB: `mariadb-install-db`) y **solo si termina con éxito** se renombra a su sitio. Una inicialización fallida, cancelada o interrumpida no deja una carpeta de datos a medias que parezca buena: la carpeta `.init` sobrante es nuestra y se borra antes de reintentar. La inicialización tiene tope de 180 s.
5. **Root sin contraseña y solo desde este equipo** (ADR-0006): `bind-address = 127.0.0.1`; ningún secreto en `crisol.toml` ni en `my.ini`. **Pendiente:** poner una contraseña a root desde configuración (referencia `{ env = … }` o credencial del sistema, ADR-0006) no está implementado; requiere arrancar el servidor y ejecutar un `ALTER USER`, y se hará con el comando de gestión de usuarios.
6. **Parada ordenada:** `mysqladmin shutdown` por TCP con 30 s de gracia antes de matar el grupo (matar una base obliga a recuperar el log en el siguiente arranque). El camino de la parada queda en el log (`StopPath`).
7. **Salud por puerto**; el arranque tiene hasta 90 s (la recuperación tras un cierre sucio puede tardar).
8. **MySQL sin `skip-name-resolve`**: `--initialize-insecure` solo crea `root@localhost` y una conexión desde 127.0.0.1 es esa cuenta únicamente si el servidor asocia la dirección de loopback a `localhost` (hallazgo con el MySQL 8.4 real). MariaDB sí lo usa (crea `root@127.0.0.1` y `root@::1`). MySQL desactiva el protocolo X (`mysqlx = OFF`) para no abrir el 33060 sin necesidad.

## Consecuencias

- `Daemon` conoce a `DbStack`; `status` lista las bases instaladas (paradas) y las arrancadas.
- Pruebas reales a pedido (`crisol-platform/tests/databases.rs`) con los binarios oficiales en los puertos 19306 a 19308: primer arranque con inicialización (1,7 s MariaDB, 4,5 s MySQL), consulta, datos persistentes, parada ordenada (0,25 s y 1,0 s), segundo arranque sin reinicializar.
- Los servicios siguientes (PostgreSQL, Redis, Mailpit…) reutilizan el patrón: plan, configuración generada, inicialización en `.init`, parada ordenada y estado propio.
