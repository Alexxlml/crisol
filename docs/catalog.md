# Catálogo de versiones (`catalog.toml`)

El catálogo lista qué se puede instalar: de qué URL oficial se baja cada build, cómo verificarla y dónde queda el ejecutable. Lo lee `crisol-core` (módulo `catalog`). Hay un catálogo **incrustado en el binario** (`src/crisol-core/data/catalog.toml`) y, más adelante, uno **actualizable** que se descarga firmado.

## Formato

```toml
schema    = 1             # único formato que existe hoy
serial    = 7             # sube con cada catálogo publicado
generated = "2026-10-14"  # informativo

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
```

- `format = "file"` es un archivo suelto que se instala tal cual (por ejemplo `composer.phar`); `executable` es el nombre con el que queda.
- `root` evita que el ejecutable quede en una carpeta intermedia: Nginx y Node empaquetan todo dentro de una carpeta con el nombre de la versión.
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
- `latest` es la más alta no preliminar.
- El orden es numérico: `8.4.10` es mayor que `8.4.2`.

## Confianza (ADR-0005)

1. **Catálogo incrustado:** viaja dentro del binario y no necesita firma.
2. **Catálogo actualizable:** debe traer una firma [minisign](https://jedisct1.github.io/minisign/) que se verifica **antes** de leerlo, con la clave pública del proyecto. La firma cubre los bytes exactos del archivo: cualquier cambio, incluso de saltos de línea, la invalida.
3. **Anti-reversión:** un catálogo con `serial` menor que el que se usa se rechaza, aunque su firma sea válida; así no se puede reintroducir una versión vulnerable con un catálogo viejo.
4. **Cada descarga** se compara con el `sha256` del catálogo antes de extraerse (F3.3).

La clave pública del proyecto está en `CATALOG_PUBLIC_KEY` (`src/crisol-core/src/catalog.rs`); la privada la guarda el propietario fuera del repositorio y nunca debe entrar a él. Si se perdiera, habría que generar otro par y publicar un binario nuevo con la clave pública nueva. Con la constante en `None`, solo se acepta el catálogo incrustado.

## Cómo añadir una entrada (F3.2)

1. Tomar la URL de la fuente oficial y anotarla en `analisis/fuentes-binarios.md`.
2. Calcular el SHA-256 del archivo descargado y compararlo con lo que publica el proveedor.
3. Añadir el `[[package]]` y subir `serial`.
4. `cargo test -p crisol-core catalog` valida el catálogo incrustado completo.
