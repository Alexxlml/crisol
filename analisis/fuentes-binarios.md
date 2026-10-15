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
- MySQL, MariaDB, PostgreSQL, Redis, Mailpit, Meilisearch, Memcached y MongoDB entran en la Fase 9. **Redis** no tiene builds oficiales para Windows (F9.3).
