# Fuentes de los binarios del catálogo

Qué se descarga, de dónde, qué publica cada proveedor para verificarlo y qué comprobación se hizo antes de poner cada entrada en `src/crisol-core/data/catalog.toml`.

- **Fecha de la verificación:** 2026-10-14.
- **Plataforma:** Windows x86_64.
- **Regla (plan F3.2):** solo fuentes oficiales, y cada entrada con un SHA-256 **calculado sobre el archivo descargado** y comparado con lo que publica el proveedor. Una prueba del repositorio (`every_built_in_package_comes_from_an_official_host`) rechaza cualquier URL que no sea de estos cuatro sitios.

## Resumen

| Herramienta | Sitio oficial | Qué publica el proveedor | Comprobación hecha | Resultado |
|---|---|---|---|---|
| PHP 8.3.35, 8.4.26, 8.5.11 (NTS x64) | `windows.php.net` | SHA-256 en `releases.json` | Hash calculado = hash publicado; la URL de `archives/` entrega los mismos bytes | ✅ coincide |
| PHP 7.4.33 (NTS x64, vc15) | `windows.php.net/downloads/releases/archives/` | **Nada**: ni hash ni firma para versiones archivadas (`releases.json` ya no la lista; `php.net` publica hashes solo del código fuente) | Hash calculado = `14ae3250…8aace`, idéntico en **dos descargas** HTTPS separadas; `unzip -t` sin errores; nombres sin rutas absolutas, `..` ni unidades; `php.exe -v` imprime `PHP 7.4.33 … NTS Visual C++ 2017 x64`; `php -m` limpio con el `php.ini` generado y cURL con el bundle del ADR-0011 llega a un sitio HTTPS | ⚠️ **certeza menor**: integridad de la transferencia, no autenticidad contra el proveedor |
| Nginx 1.28.3, 1.30.5, 1.31.6 | `nginx.org` | **Solo** firma PGP (`.zip.asc`); sin hash | Firma verificada con `gpg` y la clave publicada por nginx.org | ✅ firma válida |
| Node 22.23.3, 24.21.0 (win-x64) | `nodejs.org` | `SHASUMS256.txt` firmado con PGP | Hash calculado = hash publicado; firma de `SHASUMS256.txt` verificada con la clave de `nodejs/release-keys` | ✅ coincide y firma válida |
| Composer 2.10.3 | `getcomposer.org` | `composer.phar.sha256sum` y firma PGP (`.asc`) | Hash calculado = hash publicado; firma verificada con la clave de `keys.openpgp.org` | ✅ coincide y firma válida |

Descarga total: unos 182 MB en una carpeta temporal fuera del proyecto. Nada se ejecutó ni se extrajo; solo se leyeron los nombres de los ZIP para fijar `root` y `executable`, y se comprobó que ningún nombre fuera absoluto, contuviera `..` o una unidad.

| CA bundle de Mozilla (`cacert` 2026.9.25) | `curl.se` | `cacert-AAAA-MM-DD.pem.sha256` | Hash calculado = hash publicado, y el mismo que el de `cacert.pem` del día; 188 900 bytes, 121 certificados | ✅ coincide |

## PHP

- **Fuente:** `https://windows.php.net/downloads/releases/`. El índice `releases.json` trae, por rama, el nombre del ZIP y su `sha256`.
- **Build elegida:** `nts` (sin hilos), `x64`. Crisol ejecuta PHP con `php-cgi.exe` (sin php-fpm en Windows), y ese modo es el de la build NTS.
- **Versiones:** una por rama con soporte activo o de seguridad: **8.3.35** (compilada con VS16), **8.4.26** y **8.5.11** (VS17). No se incluyen 8.2 ni anteriores, con **una excepción**: **7.4.33** (`vc15`, 26 132 214 bytes), última versión de la rama, marcada `end_of_life` en el catálogo para poder levantar proyectos viejos (ADR-0012). Sin hash del proveedor (ver la tabla); comprobada con una prueba real (`real_php_7_4_runs_with_the_generated_ini_and_bundle`, `#[ignore]`).
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

## CA bundle (`cacert`)

- **Fuente:** `https://curl.se/ca/`. curl extrae del código de Mozilla (`certdata.txt`) las autoridades raíz en formato PEM y las publica como `cacert.pem`. No es un programa: es la base del archivo que Crisol genera para el cURL de PHP (ADR-0011, `etc/php/cacert.pem`).
- **URL fijada:** `cacert-2026-09-25.pem`. El `cacert.pem` sin fecha cambia cada pocas semanas y rompería el hash; la copia con fecha no cambia. La versión del catálogo es la fecha sin ceros a la izquierda (`2026.9.25`), porque el formato de versiones no los admite.
- **Qué publica el proveedor para verificarlo:** un `.sha256` por archivo (`cacert-2026-09-25.pem.sha256`). No publica firma. La confianza descansa en HTTPS con curl.se y en el hash fijado en el catálogo.
- **Comprobación hecha (2026-10-14):** el hash calculado sobre la descarga coincide con el publicado, y es el mismo que el de `cacert.pem` en ese momento; 188 900 bytes, 121 certificados, fin de línea LF.
- **Cómo renovar:** cada vez que se publique un catálogo nuevo (Mozilla lo actualiza cada pocas semanas): buscar el último `cacert-AAAA-MM-DD.pem` en `https://curl.se/docs/caextract.html`, descargarlo, comparar con su `.sha256`, y cambiar `version`, `url`, `sha256` y `size` de la entrada `cacert`. Las versiones anteriores quedan instaladas hasta que se desinstalen; Crisol usa la más reciente.

## Bases de datos (F9.1, 2026-10-16)

| Herramienta | Sitio oficial | Qué publica el proveedor | Comprobación hecha | Resultado |
|---|---|---|---|---|
| MariaDB 11.8.9 y 10.11.19 (winx64.zip) | `archive.mariadb.org` | SHA-256 por archivo (API `downloads.mariadb.org/rest-api/mariadb/<versión>/`), también MD5, SHA-1 y SHA-512 | Hash calculado = hash publicado en los dos; nombres del ZIP sin rutas absolutas ni `..`; carpeta raíz `mariadb-<versión>-winx64`; servidor `bin/mariadbd.exe` | ✅ coincide |
| MySQL 8.4.11 (winx64.zip) | `cdn.mysql.com` (el CDN al que enlaza `dev.mysql.com/downloads`) | **MD5** (`.md5`) y firma GPG (`.asc`); sin SHA-256 | MD5 calculado = MD5 publicado (`2e833921…bd811bc`); firma verificada con `gpg` y un llavero temporal con la clave de `repo.mysql.com/RPM-GPG-KEY-mysql-2023` (huella `BCA4 3417 C3B4 85DD 128E C6D4 B7B3 B788 A8D3 785C`): `Good signature from "MySQL Release Engineering"`. El SHA-256 del catálogo (`a492371d…3d25c`) se calculó sobre ese archivo ya verificado | ✅ firma válida y MD5 coincide. **Fecha de la firma frente a la caducidad de la clave (revisión de la Fase 9, 2026-10-17):** la firma se hizo el **2026-07-01 18:19 UTC**. El archivo `RPM-GPG-KEY-mysql-2023` trae la clave con caducidad el **2025-10-22**, es decir, **antes** de la firma: con ese archivo gpg dice `[expired]`, y una firma posterior a la caducidad no valdría. Pero `repo.mysql.com` publica también `RPM-GPG-KEY-mysql-2025`, que es **la misma clave primaria** (misma huella `BCA4 3417 …785C`, mismo `2023-10-23` de creación) con la caducidad ampliada al **2027-10-23**; con ese archivo la firma es `Good signature` sin aviso de caducidad (solo el de confianza, porque el llavero es temporal), y la fecha de la firma cae dentro de la validez. **Certeza algo menor:** la clave y el archivo vienen de `repo.mysql.com`, el mismo proveedor, y no encontré una segunda fuente independiente de la huella `dev.mysql.com` no responde a clientes sin navegador (página «Technical Difficulties»), por eso el CDN |
| PostgreSQL 17.11 y 18.6 (windows-x64-binaries.zip, build `-1` de EDB) | `get.enterprisedb.com` (EDB, a quien enlaza `postgresql.org/download/windows`) | **Nada**: ni hash ni firma (`.sha256`, `.md5` y `.asc` dan 403), y los ejecutables **no están firmados con Authenticode** (`Get-AuthenticodeSignature` sobre `postgres.exe` e `initdb.exe` de las dos versiones: `NotSigned`). `postgresql.org` publica hashes solo del código fuente | Hash calculado = `6eabdf00…0cfea3` (17.11) y `fbe23da2…4252f8c` (18.6), **idéntico en dos descargas** HTTPS separadas; nombres sin rutas absolutas ni `..`; raíz `pgsql/`; `bin/postgres.exe`, `initdb.exe`, `pg_ctl.exe`, `pg_isready.exe`, `psql.exe` presentes; las dos versiones inicializan, arrancan, responden y se detienen en una prueba real | ⚠️ **certeza menor**: integridad de la transferencia, no autenticidad contra el proveedor (como PHP 7.4). El ZIP incluye pgAdmin 4, de ahí los 340 MB |
| Mailpit 1.31.6 (windows-amd64.zip) | `github.com/axllent/mailpit/releases` | Hash SHA-256 de cada archivo que **calcula GitHub** al subirlo (`digest` de la API de releases) | Hash calculado = `digest` (`5d5ad094…4d3b1`); ZIP de 3 archivos (`mailpit.exe`, licencia, README) sin rutas peligrosas; una prueba real recibe un mensaje por SMTP y lo muestra en la web | ✅ coincide con GitHub. Certeza algo menor: es el hash del anfitrión de la descarga, no una firma del autor |
| Meilisearch 1.55.0 (windows-amd64.exe) | `github.com/meilisearch/meilisearch/releases` | `digest` de GitHub, igual que arriba | Hash calculado = `digest` (`94853557…1e62a`); un único `.exe` de 353 MB (formato `file`); una prueba real indexa un documento, lo busca y lo conserva tras reiniciar | ✅ coincide con GitHub (misma salvedad) |
| MongoDB 8.0.34 (windows x86_64 zip, edición comunidad) | `fastdl.mongodb.org` | SHA-256 por archivo en su propio feed `downloads.mongodb.org/current.json` | Hash calculado = hash publicado (`6b021a08…630db0`); 12 entradas, raíz `mongodb-win32-x86_64-windows-8.0.34/`, `bin/mongod.exe`; una prueba real arranca, hace sus datos (`WiredTiger`) y reinicia. El archivo no trae cliente (`mongosh` es otra descarga) | ✅ coincide |
| Redis 5.0.14.1 (tporadowski) | `github.com/tporadowski/redis/releases` | **Nada** (ni hash, ni `digest` de GitHub, ni firma). *Port* no oficial de Windows; no existe uno oficial | Hash `018ea18a…720ccf` **idéntico en dos descargas**; 16 archivos sin rutas peligrosas; `redis-server.exe` y `redis-cli.exe`; prueba real: arranca, guarda al apagar y recupera los datos. Marcado `end_of_life` | ⚠️ **certeza menor** (integridad de la transferencia, no autenticidad) |
| Garnet 2.2.3 (win-x64-based-readytorun.zip) | `github.com/microsoft/garnet/releases` | `digest` de GitHub | Hash calculado = `digest` (`5cd13068…f9f16`); 93 entradas bajo `net10.0/` | ✅ coincide con GitHub (misma salvedad que Mailpit) |
| Memcached 1.6.8 (`memcached-1.6.8-win64-mingw.zip`, build de terceros `jefyt/memcached-windows`, publicada el 2026-02-28) | `github.com/jefyt/memcached-windows/releases` (el proyecto Memcached solo publica código fuente; no existe un build oficial para Windows) | `hashes.txt` en la misma publicación (SHA-256 y SHA-512) y el `digest` de GitHub | SHA-256 calculado = `hashes.txt` = `digest` de GitHub (`48ec62ce…5994b`); SHA-512 también coincide; tamaño 3 540 169 bytes; 7 entradas bajo `memcached-1.6.8-win64-mingw/` sin rutas peligrosas; `bin/memcached.exe`; prueba real: guarda y devuelve un valor, escucha solo en `127.0.0.1` y se detiene en 0,2 s. Marcado `end_of_life` | ⚠️ **certeza menor**: las tres cifras vienen de la misma cuenta que subió el archivo, así que prueban la transferencia, no quién lo compiló. Memcached 1.6.8 es de 2020 y ya no recibe parches |

Solo se listan estas versiones: MySQL **8.4 LTS**, MariaDB **11.8 LTS** (la actual) y **10.11 LTS**, PostgreSQL **18** y **17** (para tener dos series con datos separados). Una prueba del repositorio exige que los hosts sean estos oficiales (`archive.mariadb.org`, `cdn.mysql.com`, `get.enterprisedb.com`).

## Cómo actualizar el catálogo

Para añadir una versión nueva o actualizar una existente:

1. Mirar el índice del proveedor (`releases.json` de PHP, `index.json` de Node, `/versions` de Composer, la página de descargas de Nginx).
2. Descargar el archivo a una carpeta **nueva y vacía fuera del repositorio**. No ejecutarlo.
3. Calcular su SHA-256 y compararlo con lo que publica el proveedor. Para Nginx, verificar la firma `.asc` con `gpg` y un llavero temporal (`GNUPGHOME` propio).
4. Comprobar el contenido del ZIP (solo los nombres): carpeta raíz y ejecutable.
5. Añadir el `[[package]]` a `src/crisol-core/data/catalog.toml` y **subir `serial`**.
6. Ejecutar `cargo test -p crisol-core catalog`: valida el formato, los hosts oficiales y las carpetas `root`.
7. Comprobar que el archivo **real** se instala con las validaciones estrictas del instalador (rutas, nombres reservados de Windows, enlaces simbólicos). Con los archivos descargados en la carpeta del paso 2:

   ```bash
   CRISOL_TEST_DOWNLOADS=<carpeta> cargo test -p crisol-core real_provider -- --ignored --nocapture
   ```

   Instala cada entrada del catálogo en un `CRISOL_HOME` temporal. Con `cargo test -p crisol-core real_network -- --ignored` se prueba además la descarga real por HTTPS (Composer, 3,6 MB).
8. Anotar aquí la versión, la fecha y el resultado.

Para PHP usar siempre la URL de `archives/`.

**Resultado de la instalación de prueba (2026-10-14):** las 9 entradas se instalan completas con el instalador real: Composer (1 archivo), Nginx (31–32), Node 22 (2 032), Node 24 (1 994) y PHP (78–82).

## Pendiente

- Las entradas para Linux y macOS (Fase posterior).
- MySQL, MariaDB, PostgreSQL, Redis, Mailpit, Meilisearch, Memcached y MongoDB entran en la Fase 9. **Redis** y **Memcached** no tienen builds oficiales para Windows (F9.3 y la revisión de la Fase 9); los dos son de terceros, con certeza menor.
