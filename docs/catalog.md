# Catálogo de versiones (`catalog.toml`)

El catálogo lista qué se puede instalar: de qué URL oficial se baja cada build, cómo verificarla y dónde queda el ejecutable. Lo lee `crisol-core` (módulo `catalog`). Hay un catálogo **incrustado en el binario** (`src/crisol-core/data/catalog.toml`) y, más adelante, uno **actualizable** que se descarga firmado.

## Formato

```toml
schema    = 1             # único formato que existe hoy
serial    = 12            # sube con cada catálogo publicado
generated = "2026-10-14"  # informativo

[recommended]             # opcional: la línea que Crisol usa por defecto (ADR-0012)
php = "8.4"

[[package]]
service    = "nginx"      # minúsculas, dígitos, "-" y "_"
version    = "1.28.2"     # números separados por puntos; "-rc1" para preliminares
os         = "windows"    # windows | linux | macos
arch       = "x86_64"     # x86_64 | aarch64
url        = "https://nginx.org/download/nginx-1.28.2.zip"
sha256     = "…64 dígitos hexadecimales…"
format     = "zip"        # zip | tar.gz | tar.xz | file
root       = "nginx-1.28.2"   # opcional: carpeta del archivo que se sube al nivel raíz
executable = "nginx.exe"  # ejecutable principal, relativo a la carpeta de instalación
size       = 1802240      # opcional, bytes
end_of_life = true        # opcional: ya no recibe parches de seguridad (ADR-0012)
```

- `format = "file"` es un archivo suelto que se instala tal cual (por ejemplo `composer.phar`); `executable` es el nombre con el que queda.
- `root` evita que el ejecutable quede en una carpeta intermedia: Nginx y Node empaquetan todo dentro de una carpeta con el nombre de la versión.
- `[recommended]` asigna a cada servicio la **línea** (`8.4`, no una versión exacta) que se usa cuando nada pide otra cosa y cuando la restricción abierta de un proyecto (`>=7.4`) la cumple. Vive en el catálogo y no en el binario para poder cambiarla (a `8.5`) publicando un catálogo firmado nuevo. Un catálogo sin esta sección es válido. La clave debe ser un nombre de servicio válido y el valor, una línea de versión.
- `end_of_life = true` marca una versión que se puede instalar (un proyecto viejo la puede necesitar) pero que **nunca** es la predeterminada ni la que elige `latest` mientras exista otra. Al instalarla, `Installed::end_of_life` avisa a quien llama. Hoy PHP 7.4.33 y Node 20.20.2 (más Redis 5 y Memcached 1.6.8, ver ADR-0017).
- Los campos desconocidos se avisan, no fallan: un catálogo más nuevo sigue leyéndose.

## Qué se rechaza

| Campo | Regla |
|---|---|
| `url` | Solo `https://`. Sin usuario ni contraseña (`https://confiable@malo/`), sin espacios, host con etiquetas válidas. |
| `sha256` | Exactamente 64 dígitos hexadecimales (se guarda en minúsculas). |
| `executable`, `root` | Ruta **relativa** que no sale de la carpeta: sin `..`, sin `/` inicial, sin unidad (`C:`), sin componentes vacíos. Las `\` se convierten en `/`. |
| `version` | Números con puntos (`8.4`, `5.0.14`, `1.2.3.4`) y sufijo `-tag` opcional. Sin ceros a la izquierda ni prefijo `v`. |
| `size` | Mayor que cero si aparece. |
| Duplicados | No puede haber dos entradas del mismo servicio, plataforma y versión (`8.4` y `8.4.0` son la misma). |

## Cómo se elige una versión

`find(servicio, petición, plataforma)` devuelve la **versión más alta** que cumpla la petición:

- `8.3` coincide con `8.3.0`, `8.3.12`…, no con `8.30.1` ni con `8.4.0`. Es un prefijo por partes numéricas.
- `8` coincide con cualquier `8.x.y`.
- Las versiones preliminares (`8.5.0-rc1`) **solo** se eligen pidiéndolas exactamente.
- `latest` es la más alta no preliminar, prefiriendo una que aún reciba parches (`end_of_life` no se elige mientras haya otra).
- El orden es numérico: `8.4.10` es mayor que `8.4.2`.

## Confianza (ADR-0005)

1. **Catálogo incrustado:** viaja dentro del binario y no necesita firma.
2. **Catálogo actualizable:** debe traer una firma [minisign](https://jedisct1.github.io/minisign/) que se verifica **antes** de leerlo, con la clave pública del proyecto. La firma cubre los bytes exactos del archivo: cualquier cambio, incluso de saltos de línea, la invalida.
3. **Anti-reversión:** un catálogo con `serial` menor que el más alto visto se rechaza, aunque su firma sea válida; así no se puede reintroducir una versión vulnerable con un catálogo viejo.

   Las dos comprobaciones van juntas en **`Catalog::load_update(texto, firma, serial_mínimo)`**, la única forma pública de leer un catálogo externo (el análisis sin comprobar es privado del crate, así que no se puede olvidar una de las dos). `serial_mínimo` debe ser el mayor entre el `serial` del catálogo incrustado y el más alto que se haya aceptado alguna vez; **dónde se guarda ese valor** lo decide la tarea que implemente la actualización (no puede ser `state/`, que se puede borrar).
4. **Cada descarga** se compara con el `sha256` del catálogo antes de extraerse (F3.3).

La clave pública del proyecto está en `CATALOG_PUBLIC_KEY` (`src/crisol-core/src/catalog.rs`); la privada la guarda el propietario fuera del repositorio y nunca debe entrar a él. Si se perdiera, habría que generar otro par y publicar un binario nuevo con la clave pública nueva. Con la constante en `None`, solo se acepta el catálogo incrustado.

## Avisos de vulnerabilidad (ADR-0020)

```toml
[[advisory]]
id       = "PHP2Shell"                      # nombre corto, único en el catálogo, sin espacios
service  = "php"
affects  = ["7.4", "8.3", "8.4", "8.5"]     # opcional: líneas afectadas; sin él, todas las versiones del servicio
fixed_in = ["8.3.36", "8.4.27", "8.5.12"]   # primera versión corregida de cada línea; vacío = sin corrección
summary  = "Remote code execution in PHP (CVE-2026-109618 and CVE-2026-109619), exploited in the wild"
url      = "https://…"                      # solo https, como los demás
```

- Una versión está **afectada** si coincide con alguna línea de `affects` y **ninguna** versión de `fixed_in` de esa misma línea es menor o igual que ella. Con el ejemplo, 8.4.26 está afectada, 8.4.27 no, y 7.4.33 lo está siempre (su línea no tiene corrección).
- Un aviso **informa, no bloquea**: no impide instalar ni usar la versión, y no es lo mismo que `end_of_life` (los dos pueden darse a la vez).
- `crisol install` imprime los avisos de la versión instalada (también si ya lo estaba), con la versión corregida y el comando; `crisol list` marca las versiones afectadas; `crisol doctor` lista las **instaladas** afectadas con su solución. El shim no avisa en cada ejecución.
- Se rechazan: `url` que no sea https, `id` repetido o con espacios, `service` inválido, `summary` vacío, y líneas o versiones ilegibles. Un catálogo sin avisos es válido.

## Instalar un archivo que el catálogo no conoce (ADR-0020)

`crisol install <servicio> <versión exacta> --from <archivo> --sha256 <hash>` es la salida de emergencia cuando el catálogo de este `crisol.exe` aún no trae una versión (por ejemplo, una corrección de seguridad publicada después). `--sha256` es obligatorio y se compara antes de extraer nada; quien lo da responde por él.

- La estructura del archivo (`format`, `root`, `executable`) se toma de la **versión más alta** de ese servicio en el catálogo (`Catalog::template`). Si no coincide, el instalador falla con «el archivo no contiene…» y no deja nada instalado.
- Si el catálogo ya tiene esa versión: con **el mismo hash** es una instalación sin red y queda verificada; con **otro hash** se rechaza (no se puede pisar una entrada del catálogo).
- Lo instalado así lleva `.crisol-unverified.toml` (hash, nombre del archivo y fecha) dentro de `bin/<servicio>/<versión>/`; `list` lo marca «not verified by Crisol» y `doctor` lo lista. Se borra con la versión.

## Cómo añadir una entrada (F3.2)

1. Tomar la URL de la fuente oficial y anotarla en `analisis/fuentes-binarios.md`.
2. Calcular el SHA-256 del archivo descargado y compararlo con lo que publica el proveedor.
3. Añadir el `[[package]]` y subir `serial`.
4. `cargo test -p crisol-core catalog` valida el catálogo incrustado completo.
