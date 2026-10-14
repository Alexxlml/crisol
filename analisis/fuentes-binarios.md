# Fuentes de los binarios del catálogo

Qué se descarga, de dónde, qué publica cada proveedor para verificarlo y qué comprobación se hizo antes de poner cada entrada en `src/crisol-core/data/catalog.toml`.

- **Fecha de la verificación:** 2026-10-14.
- **Plataforma:** Windows x86_64.
- **Regla (plan F3.2):** solo fuentes oficiales, y cada entrada con un SHA-256 **calculado sobre el archivo descargado** y comparado con lo que publica el proveedor. Una prueba del repositorio (`every_built_in_package_comes_from_an_official_host`) rechaza cualquier URL que no sea de estos cuatro sitios.

## Resumen

| Herramienta | Sitio oficial | Qué publica el proveedor | Comprobación hecha | Resultado |
|---|---|---|---|---|
| PHP 8.3.35, 8.4.26, 8.5.11 (NTS x64) | `windows.php.net` | SHA-256 en `releases.json` | Hash calculado = hash publicado; la URL de `archives/` entrega los mismos bytes | ✅ coincide |
| Nginx 1.28.3, 1.30.5, 1.31.6 | `nginx.org` | **Solo** firma PGP (`.zip.asc`); sin hash | Firma verificada con `gpg` y la clave publicada por nginx.org | ✅ firma válida |
| Node 22.23.3, 24.21.0 (win-x64) | `nodejs.org` | `SHASUMS256.txt` firmado con PGP | Hash calculado = hash publicado; firma de `SHASUMS256.txt` verificada con la clave de `nodejs/release-keys` | ✅ coincide y firma válida |
| Composer 2.10.3 | `getcomposer.org` | `composer.phar.sha256sum` y firma PGP (`.asc`) | Hash calculado = hash publicado; firma verificada con la clave de `keys.openpgp.org` | ✅ coincide y firma válida |

Descarga total: unos 182 MB en una carpeta temporal fuera del proyecto. Nada se ejecutó ni se extrajo; solo se leyeron los nombres de los ZIP para fijar `root` y `executable`, y se comprobó que ningún nombre fuera absoluto, contuviera `..` o una unidad.

## PHP

- **Fuente:** `https://windows.php.net/downloads/releases/`. El índice `releases.json` trae, por rama, el nombre del ZIP y su `sha256`.
- **Build elegida:** `nts` (sin hilos), `x64`. Crisol ejecuta PHP con `php-cgi.exe` (sin php-fpm en Windows), y ese modo es el de la build NTS.
- **Versiones:** una por rama con soporte activo o de seguridad: **8.3.35** (compilada con VS16), **8.4.26** y **8.5.11** (VS17). No se incluyen 8.2 ni anteriores.
- **URL permanente:** la carpeta `releases/` solo conserva el último release de cada rama (8.4.25 ya devuelve 404 ahí); `releases/archives/` conserva **todos**, incluidos los vigentes. Las entradas usan `archives/`, así la URL no caduca cuando sale el siguiente parche. Se descargó también desde `archives/` y los tres hashes coinciden con los publicados.
- **Ejecutable:** `php.exe`; `php-cgi.exe` queda a su lado. El ZIP no trae carpeta raíz.
- **Dependencia del sistema:** las builds VS16 y VS17 necesitan el *Microsoft Visual C++ Redistributable* (2015–2022, x64). Debe revisarlo `crisol doctor` (F11.1).

## Nginx

- **Fuente:** `https://nginx.org/download/nginx-<versión>.zip` y `.zip.asc`.
- **Versiones:** `1.28.3` (heredada), `1.30.5` (estable) y `1.31.6` (principal), según la página de descargas de nginx.org.
- **Sin hash publicado.** El proveedor solo ofrece firmas PGP. Se verificaron las tres con `gpg` en un **llavero temporal** (el llavero del usuario no se tocó), importando las cinco claves que nginx.org publica en `/keys/`:

  | Archivos | Firmante | Huella |
  |---|---|---|
  | `1.28.3` | Roman Arutyunyan | `4338 7825 DDB1 BB97 EC36 BA5D 007C 8D7C 15D8 7369` |
  | `1.30.5`, `1.31.6` | Sergey Kandaurov | `D678 6CE3 03D9 A902 2998 DC6C C846 4D54 9AF7 5C0A` |

  Resultado: `Good signature` en las tres. El hash de la entrada es el SHA-256 calculado sobre ese mismo archivo ya verificado.
- **Limitación:** las claves se obtuvieron de nginx.org, el mismo sitio que sirve los archivos. La firma prueba que el archivo corresponde a esa clave, pero no que la clave sea la del proyecto frente a un sitio comprometido. No se encontró una segunda fuente independiente que publique estas huellas.
- **Carpeta raíz:** el ZIP trae `nginx-<versión>/`; el catálogo la declara en `root`.

## Node

- **Fuente:** `https://nodejs.org/dist/v<versión>/node-v<versión>-win-x64.zip` y `SHASUMS256.txt` (más `.asc`, texto firmado).
- **Versiones:** las dos líneas LTS vigentes, **22.23.3** (Jod) y **24.21.0** (Krypton). Se excluyen Node 20 (fin de vida) y 26 (aún no es LTS).
- **Verificación:** el hash calculado coincide con la línea correspondiente de `SHASUMS256.txt`. La firma de ese archivo es válida con la clave `5BE8 A3F6 C8A5 C01D 106C 0AD8 20B1 A390 B168 D356` (Antoine du Hamel), descargada de **`github.com/nodejs/release-keys`**, es decir, de una fuente distinta de nodejs.org.
- **Carpeta raíz:** `node-v<versión>-win-x64/`. Ejecutable: `node.exe` (`npm`, `npx` y `corepack` van dentro).

## Composer

- **Fuente:** `https://getcomposer.org/download/2.10.3/composer.phar`, con `.sha256sum` y `.asc`.
- **Versión:** la estable actual, **2.10.3**. Existe también una rama LTS 2.2 (solo seguridad hasta 2026-12-31) que no se incluye.
- **Verificación:** el hash coincide con `composer.phar.sha256sum`. La firma es válida con la clave `161D FBE3 4288 9F01 DDAC 4E61 CBB3 D576 F2A0 946F` ("Packagist Conductors"), obtenida de **`keys.openpgp.org`**, independiente de getcomposer.org.
- **Formato:** `file` (un archivo suelto). Es un `phar` que funciona igual en cualquier sistema; hoy solo se lista para Windows x64 porque es el único objetivo del catálogo.

## Cómo actualizar el catálogo

Para añadir una versión nueva o actualizar una existente:

1. Mirar el índice del proveedor (`releases.json` de PHP, `index.json` de Node, `/versions` de Composer, la página de descargas de Nginx).
2. Descargar el archivo a una carpeta **nueva y vacía fuera del repositorio**. No ejecutarlo.
3. Calcular su SHA-256 y compararlo con lo que publica el proveedor. Para Nginx, verificar la firma `.asc` con `gpg` y un llavero temporal (`GNUPGHOME` propio).
4. Comprobar el contenido del ZIP (solo los nombres): carpeta raíz y ejecutable.
5. Añadir el `[[package]]` a `src/crisol-core/data/catalog.toml` y **subir `serial`**.
6. Ejecutar `cargo test -p crisol-core catalog`: valida el formato, los hosts oficiales y las carpetas `root`.
7. Anotar aquí la versión, la fecha y el resultado.

Para PHP usar siempre la URL de `archives/`.

## Pendiente

- Las entradas para Linux y macOS (Fase posterior).
- MySQL, MariaDB, PostgreSQL, Redis, Mailpit, Meilisearch, Memcached y MongoDB entran en la Fase 9. **Redis** no tiene builds oficiales para Windows (F9.3).
