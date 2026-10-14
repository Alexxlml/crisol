# Arquitectura de Crisol

## Vista general

```
┌──────────────┐   ┌──────────────┐   ┌──────────────────┐
│  crisol-cli  │   │  (GUI futura)│   │ (panel web futuro)│
└──────┬───────┘   └──────┬───────┘   └────────┬─────────┘
       │      IPC (JSON-RPC sobre named pipe / unix socket)
       └──────────────────┼────────────────────┘
                   ┌──────▼───────┐
                   │ crisol-daemon│  proceso residente, dueño de los servicios
                   └──────┬───────┘
                   ┌──────▼───────┐
                   │ crisol-core  │  lógica pura, sin UI, sin APIs del SO
                   └──────┬───────┘
                   ┌──────▼──────────┐
                   │ crisol-platform │  implementaciones por SO detrás de traits
                   └─────────────────┘
        crisol-shim: ejecutable diminuto para php/composer/node/npm
```

## Crates del workspace (`src/`)

| Crate | Tipo | Responsabilidad |
|---|---|---|
| `crisol-core` | lib | Modelos (servicio, versión, sitio, proyecto), configuración, catálogo de versiones, generación de configs (nginx, php.ini, my.ini…), orquestación. Define los traits de plataforma. |
| `crisol-platform` | lib | Implementación de los traits para Windows (después Linux/macOS): procesos, confianza de certificados, DNS, privilegios. |
| `crisol-protocol` | lib | Tipos de request/response del IPC, versionados. **Es el contrato que usarán las GUIs.** |
| `crisol-daemon` | bin (`crisold`) | Mantiene vivos los servicios, expone el IPC, observa cambios de config. |
| `crisol-cli` | bin (`crisol`) | Interfaz de línea de comandos. Habla con el daemon; algunas operaciones (instalar versiones, `doctor`) pueden ir directo al núcleo si el daemon no corre. |
| `crisol-shim` | bin | Se copia/enlaza como `php.exe`, `composer.bat`, `node.exe`, etc. en `shims/`. Resuelve la versión del proyecto y hace `exec` al binario real. Debe arrancar en milisegundos: sin tokio, sin dependencias pesadas. |

## Traits de plataforma (en `crisol-core`)

```rust
trait ProcessSupervisor   // lanzar, detener, matar árbol, saber si vive. Windows: Job Objects
trait PortProbe           // ¿el puerto está libre? ¿qué proceso lo usa? ¿ya escucha alguien? (is_listening no toca el puerto)
trait PrivateDirs         // crear la home con ACL solo del usuario; saber si una existente está abierta a otros
trait TrustStore          // instalar/quitar la CA local en el almacén del usuario
trait LocalDns            // resolver *.test → 127.0.0.1 (DNS propio + regla del SO, o hosts como respaldo)
trait Elevation           // ejecutar una operación puntual con admin, solo si es indispensable
trait UserPath            // agregar/quitar la ÚNICA entrada de shims del PATH del usuario
```

Cada trait tiene una implementación falsa (`fake`) en `crisol-core` para pruebas.

## Directorio raíz: `CRISOL_HOME`

Por defecto `C:\Crisol` en Windows (`~/.crisol` en Unix). Se puede cambiar con la variable `CRISOL_HOME`, pensada sobre todo para pruebas. La instalación es portable: mover la carpeta y correr `crisol repair` debe bastar.

**Permisos (ADR-0008):** la raíz de un disco da "modificar" a todos los usuarios autenticados y lo hereda a lo que se cree dentro. Por eso la home **nueva** se crea con una ACL protegida (solo el usuario, SYSTEM y administradores) mediante `PrivateDirs`; una home que ya existe no se toca, pero se avisa si está abierta a otros (`doctor`).

```
CRISOL_HOME/
├── crisol.toml          # config global del USUARIO (fuente de verdad, Crisol no la reescribe sola)
├── sites.toml           # sitios enlazados y carpetas "parked" (editada solo vía comandos)
├── state/               # estado interno de Crisol (PIDs, versiones activas, caché). Se puede borrar.
├── bin/<servicio>/<versión>/   # binarios descargados, intactos
├── etc/<servicio>/      # configs GENERADAS a partir de crisol.toml. Se regeneran; no editar a mano.
│   ├── php/<versión>/php.ini   # generado desde el php.ini-development de esa versión (F4.1)
│   └── overrides/       # fragmentos del usuario que se incluyen en lo generado (no se tocan)
├── data/<servicio>/<versión-mayor>/   # datos de bases de datos
├── logs/<servicio>/
├── certs/               # CA local y certificados por sitio
├── shims/               # ÚNICA carpeta que entra al PATH
└── www/                 # carpeta "parked" por defecto
```

**Regla clave (contra lo que pasa en Devaron):** `etc/` es derivado; `crisol.toml`, `sites.toml` y `etc/*/overrides/` son del usuario. Si el usuario necesita algo que la plantilla no cubre, va en `overrides/`, que la generación incluye y nunca pisa.

## Resolución de versión por proyecto (shims)

Orden de búsqueda al ejecutar `php` dentro de un directorio:

1. Variable `CRISOL_PHP` (sobrescritura temporal en la terminal actual).
2. `.crisol.toml` del proyecto (buscando hacia arriba desde el directorio actual, hasta el primer directorio con `.git`; se ignoran archivos de otro dueño — ADR-0010).
3. Para PHP: `.php-version`; para Node: `.nvmrc` / `.node-version` / campo `engines` de `package.json`. (Estos archivos, y `composer.json` del paso 4, se buscan hacia arriba con las mismas reglas del ADR-0010 que `.crisol.toml`: dueño de confianza y tope en `.git`.)
4. Para PHP: restricción `require.php` de `composer.json` → la versión instalada más alta que la cumpla.
5. Versión por defecto global de `crisol.toml`.

Los sitios web usan la misma resolución, así cada sitio `.test` corre con su versión de PHP.

## PHP en Windows

No hay php-fpm en Windows. El daemon mantiene un **pool de procesos `php-cgi.exe`** por versión activa, cada uno en su puerto local, y Nginx los usa como `upstream`. El daemon reinicia procesos caídos y recicla cada N peticiones (`PHP_FCGI_MAX_REQUESTS`). El reinicio es por worker: una salida con código 0 es reciclaje y no cuenta como caída, y el servicio sigue activo mientras quede un worker vivo (ADR-0009). En Unix se usará php-fpm.

## Puertos

Defaults normales (80/443, 3306, 5432…). Si el puerto está ocupado, Crisol **no falla en silencio ni cambia el puerto solo**: avisa qué proceso lo ocupa y sugiere uno. En pruebas automatizadas usa el rango 19000–19999 para no chocar con Herd ni Devaron.

## IPC

- Transporte: named pipe `\\.\pipe\crisol` en Windows; unix socket en `CRISOL_HOME/state/crisol.sock` en Unix (crate sugerido: `interprocess`).
- Formato: JSON-RPC 2.0, mensajes delimitados por línea.
- Eventos: el daemon emite notificaciones (cambio de estado de servicio, líneas de log) para que una GUI se actualice sin hacer polling.
- `crisol-protocol` lleva un número de versión; el cliente lo valida al conectar.

## Descarga de binarios

Un **manifiesto** (`catalog.toml`, incluido en el binario y actualizable) lista por servicio y versión: URL oficial, SHA-256, formato de archivo y ruta del ejecutable dentro. Se descarga, se verifica el hash y se extrae en `bin/`. Si un proveedor oficial no publica hash, el manifiesto lo fija tras verificarlo manualmente.

El instalador (`crisol-core::install`) sigue siempre este orden: descarga a una carpeta de trabajo `bin/.staging-*` (mismo disco) calculando el SHA-256 al vuelo; **verifica el hash antes de extraer nada**; extrae rechazando rutas que salgan de la carpeta, enlaces simbólicos, nombres reservados de Windows y bombas ZIP; comprueba que exista el ejecutable declarado; y mueve el resultado a `bin/<servicio>/<versión>/` con un solo renombrado. Ante cualquier fallo o cancelación la carpeta de trabajo se borra: `bin/` solo contiene instalaciones completas. La descarga va detrás del trait `Fetch` (HTTPS con `ureq` y `rustls`), así todo se prueba sin red. Un catálogo que no sea el incrustado solo se lee con `Catalog::load_update` (firma + `serial` mínimo); el análisis sin esas comprobaciones es privado del crate.

## Crates sugeridos (verificar versiones actuales antes de agregar)

`tokio`, `serde`, `toml`, `toml_edit`, `clap`, `tracing`, `tracing-subscriber`, `thiserror`, `anyhow`, `ureq` (rustls; ya en uso en `crisol-core::fetch`), `sha2`, `zip`, `semver`, `interprocess`, `windows` (Job Objects, PATH, certificados), `rcgen` (CA y certificados), `hickory-server` (DNS local), `tempfile` (pruebas), `insta` (snapshots de configs generadas).
