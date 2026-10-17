# ADR-0017 — Redis en Windows

- **Estado:** aceptada (el propietario eligió la opción 3, 2026-10-16) e implementada
- **Fecha:** 2026-10-16

## Contexto

El plan (F9.3) avisa de que **Redis no tiene builds oficiales para Windows**: el proyecto Redis solo publica código fuente y paquetes para Linux/macOS (en Windows se usa WSL o Docker, fuera del alcance de Crisol, que no instala nada con administrador ni depende de un hipervisor). Los casos de uso en una máquina de desarrollo con PHP son caché, sesiones y colas de Laravel/Symfony (comandos `GET/SET`, listas, `EVAL` con Lua en las colas de Laravel, `SCAN`, pub/sub y los *streams* de Horizon).

Se miraron las opciones el 2026-10-16 (datos de las páginas de releases de GitHub y de cada proyecto):

| Opción | Qué es | Versión y estado | Verificación | Licencia | Instalación en Crisol |
|---|---|---|---|---|---|
| **tporadowski/redis** | *Port* comunitario no oficial del Redis original | **5.0.14.1, febrero de 2022** (la última); el repositorio sigue vivo pero sin versiones nuevas; Redis 5 ya no recibe parches de seguridad | Ni hash ni firma (GitHub no da `digest` para esos archivos); solo HTTPS a github.com | BSD-3 del código original (la ficha de GitHub no declara licencia) | ZIP de 12 MB con `redis-server.exe`; sin administrador |
| **Garnet** (Microsoft Research) | Servidor de caché compatible con el protocolo RESP, escrito en .NET; no es Redis | **2.2.3, 2026-10-16**; versiones casi semanales; activo | Cada archivo de la versión trae `sha256` de GitHub (`digest`), pero no firma | **MIT** | `win-x64-based-readytorun.zip`, 26 MB; **por comprobar**: si necesita el *runtime* de .NET instalado (los *readytorun* suelen ser dependientes del *framework*) y cuánto de Redis cubre |
| **Memurai** | Redis comercial para Windows (compatible con Redis 7) | Activo | Instalador firmado | Comercial; la edición *Developer* es gratuita con condiciones | `.msi` que instala un **servicio de Windows con administrador**: choca con las reglas de Crisol |
| WSL / Docker | Redis oficial en Linux | — | — | — | Fuera de alcance: necesita virtualización y, en general, administrador |

Redis 8 (2025) volvió a una licencia con *copyleft* fuerte (AGPLv3 como opción), lo que descarta empaquetarlo por nuestra cuenta aunque se compilara para Windows.

## Opciones para decidir

1. **Garnet como `redis`** (recomendada): activo, MIT, de Microsoft, con hash por archivo y compatible con RESP para lo habitual. A favor: es el único mantenido y de licencia limpia. En contra: no es Redis; algunas órdenes o módulos pueden faltar o comportarse distinto, y puede exigir el *runtime* de .NET. Hay que probar con un `redis-cli`/`predis` y con las colas de Laravel antes de comprometerse.
2. **tporadowski/redis 5.0.14.1**: es Redis de verdad, pero de 2021 a 2022, sin parches, sin hash publicado y con un mantenimiento incierto. Servir una versión sin soporte contradice ADR-0012 salvo que se marque `end_of_life`.
3. **Ofrecer las dos** con nombres distintos (`garnet` y `redis` = tporadowski marcado `end_of_life`), y que `crisol install redis` avise con claridad de lo que se instala.
4. **No incluir Redis ahora**: dejar el puerto 6379 libre para quien ya tenga el suyo (Herd, Memurai) y que `doctor` lo diga.

## Decisión

**Opción 3: se ofrecen las dos, con nombres distintos y como alternativas excluyentes.** El propietario prefirió ofrecer opciones a casarse con una; lo único que Crisol hace es evitar que corran las dos a la vez.

- **`redis`** = el *port* de tporadowski, 5.0.14.1, marcado `end_of_life` en el catálogo (el que suelen traer las suites locales para Windows). Es Redis de verdad, pero sin parches.
- **`garnet`** = Garnet 2.2.3 de Microsoft: mantenido y MIT, pero **no es Redis**.
- Comparten el puerto 6379 y son **excluyentes**: arrancar la segunda con la primera activa se rechaza con `Conflict` (quién tiene el puerto y `crisol stop <la otra>`); `crisol.toml` con las dos `enabled = true` da el aviso de «alternativas» (como MySQL y MariaDB, ADR-0007). Con puertos distintos, pueden convivir.
- Implementado con el patrón de ADR-0016 (F9.3).

### Lo que se encontró con los binarios reales

- **Redis 5 (tporadowski):** arrancado solo con argumentos, **no guarda nada, ni con `SHUTDOWN`** (`config get save` vacío); por eso se piden los puntos de guardado habituales (`--save 900 1 …`). Se fija `--maxmemory 512mb` (el *port* lo usa para dimensionar el montón del guardado en segundo plano) y `--protected-mode yes` con `--bind 127.0.0.1`. Se detiene con `redis-cli shutdown` (0,3 s) y los datos vuelven tras reiniciar (`dump.rdb`).
- **Garnet:** **Lua está desactivado por defecto**; se enciende con `--lua` (las colas de Laravel lo usan). Es un `.exe` de 34 MB junto a su carpeta `net10.0/`, sin `.dll` ni `runtimeconfig` de él: parece autocontenido, pero esta máquina tiene .NET 10 instalado, así que **no se pudo comprobar** que no lo necesite. **Guarda los datos en memoria** salvo que se configure persistencia (no se configura): sirve de caché, no de almacén. Se detiene terminando el grupo (0,2 s).
- **Diferencia de comportamiento medida:** un script `EVAL "return redis.call('set', KEYS[1], 'x')"` responde `+OK` (estado) en Redis y la cadena `$2 OK` en Garnet. Los clientes suelen tratarlos igual, pero es la clase de diferencia que justifica ofrecer las dos.
- Una prueba real más amplia (Laravel/Horizon contra Garnet) sigue pendiente.

### Memurai, reconsiderado (2026-10-16)

Se miró de nuevo porque Chocolatey publica `memurai-developer.portable` 4.1.8 (noviembre de 2025): un ZIP sin servicio, con `memurai.exe` firmado con Authenticode por Janea Systems y compatible con la API de Redis 7.0. **No se añade**: su EULA exige aceptación, prohíbe el uso en producción y «compartir o distribuir», incluye una cláusula de recopilación de datos (choca con ADR-0004) y de actualizaciones automáticas, y la edición Developer actual dice requerir un reinicio cada 10 días (no comprobado en la 4.1.8). No se ejecutó para no aceptar la licencia en nombre del propietario. Si se retoma, hace falta un paso de aceptación de licencia en el catálogo y una prueba aislada de lo que envía. Decisión del propietario: quedarse con `redis` y `garnet`.

