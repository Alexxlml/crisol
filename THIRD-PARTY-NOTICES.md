# Avisos de terceros

Crisol se distribuye bajo MIT o Apache-2.0, a tu elección (`LICENSE-MIT`, `LICENSE-APACHE`). Este archivo reúne lo que proviene de terceros y la licencia que lo cubre.

## Archivos de terceros incluidos en este repositorio

Solo se usan como datos de prueba; ninguno forma parte de los ejecutables.

| Archivo | Origen | Licencia |
|---|---|---|
| `src/crisol-core/src/phpini/testdata/php.ini-development.excerpt` | Extracto de `php.ini-development` de PHP 8.x | The PHP License 3.01 ([`licenses/PHP-3.01.txt`](licenses/PHP-3.01.txt)) |
| `src/crisol-core/src/cacert/testdata/mozilla-excerpt.pem` | Extracto del bundle de certificados raíz de Mozilla publicado por el proyecto curl | Mozilla Public License 2.0 (<https://mozilla.org/MPL/2.0/>) |

Aviso que exige la licencia de PHP:

> This product includes PHP software, freely available from <http://www.php.net/software/>.

Copyright (c) 1999 - 2024 The PHP Group. All rights reserved.

## Dependencias de Rust

El código de las dependencias no está en este repositorio: Cargo lo descarga al compilar (versiones fijadas en `src/Cargo.lock`). Todas tienen licencias permisivas y compatibles con MIT y Apache-2.0: MIT, Apache-2.0, BSD-3-Clause, ISC, Zlib, Unicode-3.0 y CDLA-Permissive-2.0.

Quien distribuya ejecutables compilados de Crisol debe acompañarlos con los avisos de copyright y los textos de licencia de esas dependencias. El ZIP de cada versión los trae en `LICENSES-RUST.md`, generado con [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) a partir de `Cargo.lock` (configuración en `src/about.toml`, plantilla en `src/about.hbs`; una licencia que no esté en la lista aceptada hace fallar la publicación).

## Programas que Crisol descarga

PHP, Nginx, Node.js, Composer, MySQL, MariaDB, PostgreSQL, MongoDB, Redis, Garnet, Memcached, Meilisearch, Mailpit y el bundle de certificados raíz **no se incluyen en este repositorio ni en los ejecutables de Crisol**. Crisol los descarga de sus fuentes oficiales cuando tú lo pides y verifica su SHA-256 (ver `docs/catalog.md`). Cada uno se rige por su propia licencia, que acompaña a su descarga; algunas son copyleft (por ejemplo, GPL) o de código disponible (por ejemplo, SSPL). Al instalarlos, aceptas sus términos con sus autores.

Los nombres de esos productos y de otras herramientas mencionadas en la documentación pertenecen a sus respectivos dueños y se usan solo para identificarlos.
