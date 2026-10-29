# ADR-0020 — Salida de emergencia para instalar y avisos de vulnerabilidad

- **Estado:** aceptada (el propietario decidió lo esencial el 2026-10-29 y respondió la sección 3 el mismo día, eligiendo la opción A en los tres puntos).
- **Fecha:** 2026-10-29

## Contexto

El 2026-10-29 salieron PHP 8.3.36, 8.4.27 y 8.5.12 por PHP2Shell (CVE-2026-109618 y CVE-2026-109619). El catálogo va **dentro del binario** (ADR-0005): hasta que se publique un `crisol.exe` nuevo, nadie puede instalar la versión corregida, y la única forma de enterarse de que una versión es vulnerable es leer el tema en otro sitio. Además PHP 7.4 sigue siendo vulnerable y no tiene build corregida (ADR-0012 solo dice «sin soporte»; no distingue «sin parches» de «con un fallo conocido»).

Hacen falta dos cosas, independientes entre sí:

1. Una **salida explícita** para instalar un archivo que el catálogo aún no conoce.
2. Que el catálogo pueda **decir que una versión es vulnerable**, y que `install` y `doctor` lo cuenten.

## Decisión

### 1. Salida de emergencia: `crisol install <servicio> <versión> --from <archivo> --sha256 <hash>`

Decidido por el propietario.

- `--from` es un archivo **local** ya descargado por quien ejecuta el comando. Crisol no descarga nada en esta ruta.
- `--sha256` es **obligatorio** y se compara con el archivo antes de extraerlo, igual que con un archivo del catálogo (ADR-0005). Quien lo da es quien responde por él: debe sacarlo de la página del proveedor, no del propio archivo.
- La instalación pasa por el mismo instalador: misma extracción defensiva, mismos límites, mismo movimiento atómico. Solo cambia de dónde sale el archivo y quién garantiza el hash.
- La salida y `crisol list` dicen **«sin verificar por Crisol»**: el hash lo dio el usuario, no el catálogo firmado. Cuando un `crisol.exe` nuevo traiga esa versión en su catálogo, `crisol install` la reconoce y deja de ser «sin verificar».
- Es una operación explícita: no se usa nunca por sí sola, ni como reintento si falla la descarga.
- Si el catálogo **ya tiene** esa versión, `--from` solo se acepta si `--sha256` coincide con el del catálogo (es una instalación sin red, y queda verificada). Si no coincide, se rechaza: no se puede «pisar» una entrada del catálogo.

### 2. Avisos de vulnerabilidad en el catálogo

Decidido por el propietario: el catálogo trae avisos, y `install` y `doctor` los muestran, incluido PHP 7.4.

Un aviso **no impide** instalar ni usar la versión (un proyecto viejo puede necesitarla, ADR-0012); informa. No toca `end_of_life`, que sigue significando «ya no recibe parches», y los dos pueden darse a la vez.

- `crisol install` imprime el aviso cuando la versión instalada está afectada, con la versión corregida si existe.
- `crisol doctor` (F11.1) lista cada versión **instalada** afectada, con el aviso y la solución sugerida (instalar la corregida, o `--from` mientras el catálogo del binario no la traiga).
- `crisol list --available` marca las versiones afectadas.
- El shim **no** avisa en cada ejecución (como en ADR-0012): no ensucia la salida de WordPress ni de WP-CLI.

### 3. Detalles (decididos)

**3.1 De dónde salen `executable`, `root` y `format` con `--from`.** El catálogo los trae por entrada; con `--from` no hay entrada.

- **A (recomendada).** Se toman de la entrada **más alta del catálogo para el mismo servicio y plataforma** (para PHP 8.4.28, la de 8.4.27). Si la estructura del archivo cambió, el instalador ya falla con `MissingExecutable` y no deja nada instalado. Un servicio que el catálogo no conoce se rechaza.
- B. Flags explícitos (`--executable`, `--root`, `--format`). Más largo y fácil de equivocar justo cuando se tiene prisa.

**3.2 Formato de los avisos en `catalog.toml`.** Elegida A.

- **A (recomendada).** Una sección propia, separada de los paquetes:

  ```toml
  [[advisory]]
  id       = "PHP2Shell"                 # única en el catálogo
  service  = "php"
  affects  = ["8.3", "8.4", "8.5"]      # líneas afectadas (como `[recommended]`); sin él, todas las versiones del servicio
  fixed_in = ["8.3.36", "8.4.27", "8.5.12"]   # primera versión corregida de cada línea; vacío = sin corrección (PHP 7.4)
  summary  = "…"
  url      = "https://…"                 # https, como las demás
  ```

  Un aviso se puede añadir o retirar sin tocar las entradas de los paquetes, y un mismo aviso cubre varias versiones.
- B. Un campo `advisory = "texto"` dentro de cada `[[package]]`. Más simple, pero hay que repetir el texto y no dice cuál es la versión corregida.

**3.3 Dónde queda la marca «sin verificar».** Elegida A.

- **A (recomendada).** Un archivo `.crisol-unverified.toml` dentro de `bin/<servicio>/<versión>/` con el hash dado, la fecha y el nombre del archivo de origen. Se borra con la versión, no hay base de datos que se desincronice (como `list_installed`), y `list` y `doctor` solo lo leen.
- B. No dejar marca: el aviso sale al instalar y ya. Más simple, pero `list` no puede volver a decirlo.

## Consecuencias

- Cambia el formato del catálogo (`schema` sigue en 1: es un campo opcional y los campos desconocidos solo avisan). `docs/catalog.md` debe documentarlo y la validación estricta debe rechazar avisos mal formados (`url` no HTTPS, `id` repetido, `affects` ilegible).
- El catálogo **incrustado** no lleva firma: se firma solo el actualizable (ADR-0005), y ese todavía no existe. Cambiar `catalog.toml` solo exige subir `serial`; no hay nada que firmar hasta que exista la actualización.
- `Call::Install` del protocolo gana `from` y `sha256` (opcionales, con `#[serde(default)]`); `InstalledInfo` gana `unverified` y `advisories`. El CLI sigue sin lógica.
- Riesgo asumido: `--from` confía en el hash que escribe el usuario. Es la misma confianza que tiene quien descarga y comprueba un zip a mano; la diferencia es que Crisol lo dice en voz alta y no lo mezcla con lo verificado.

## Cuándo una versión está afectada

Una versión `v` del servicio del aviso está afectada si coincide con alguna línea de `affects` (o `affects` falta) y **ninguna** versión de `fixed_in` de esa misma línea es menor o igual que `v`. Con el ejemplo de arriba, 8.4.26 está afectada, 8.4.27 no, y 7.4.33 (un aviso con `affects = ["7.4"]` y `fixed_in = []`) lo está siempre. Las versiones preliminares no coinciden con una línea (como en `VersionReq`).

## Corrección tras la revisión (Opus, 2026-10-29)

Ver `claude/revisiones/2026-10-29-opus-adr0020.md`.

- **La plantilla ajusta `root`.** Nginx, Node, MySQL, MariaDB, MongoDB y Memcached empaquetan todo en una carpeta con la versión (`node-v24.22.0-win-x64`). Con la plantilla tal cual, `--from` rechazaba siempre el archivo de otra versión. Ahora, en el `root` de la plantilla, la versión de la plantilla se sustituye por la pedida; un `root` sin versión (`pgsql`) queda igual.
- **«Versión exacta» = tantas partes como la plantilla.** PostgreSQL usa dos (`17.12`); la regla anterior (3 o más) lo dejaba fuera.
- **La marca se retira sola** (lo que prometía §1). Si el catálogo de un `crisol.exe` nuevo lista la versión con **el mismo hash** que guarda `.crisol-unverified.toml`, `crisol install <servicio> <versión>` responde «ya instalada» y borra la marca. Con **otro hash**, la marca se queda y `doctor` lo marca como problema («no es la build oficial»), con `crisol uninstall … && crisol install …` como solución.
- **`from` debe ser una ruta absoluta** en el protocolo: el daemon tiene otro directorio actual. El CLI ya la convertía.
- Pendiente para F11.1: una instalación `--from` de una línea sin soporte (PHP 7.4.x nueva) no queda como `end_of_life`, porque esa marca es por versión exacta del catálogo. El aviso de vulnerabilidad sí la cubre.
