# ADR-0011 — Certificados raíz para PHP: Mozilla más el almacén de Windows

- **Estado:** aceptada (decidida por el usuario el 2026-10-14); implementada el 2026-10-14; **corregida el 2026-10-14** tras la revisión de la Fase 4 (hallazgos 1 y 2; ver «Corrección») y **la corrección está implementada**.
- **Fecha:** 2026-10-14

## Contexto

Después de F4.1, el cURL de PHP para Windows falla con HTTPS (`unable to get local issuer certificate`), y `file_get_contents` funciona. El libcurl que trae PHP para Windows está compilado con OpenSSL y necesita un archivo de CA (`curl.cainfo`). En cambio, el OpenSSL de PHP usa el almacén de certificados de Windows cuando `openssl.cafile` está vacío, y por eso los *streams* funcionan.

Lo afectado son las aplicaciones del usuario que usan cURL: Guzzle, el cliente `Http` de Laravel y las llamadas `curl_*`. Es probable que Composer **no** esté afectado, porque `composer/ca-bundle` usa su propio `cacert.pem` cuando no encuentra otro. Eso se comprueba en F4.3.

Se consideraron cuatro opciones:
- Solo el `cacert.pem` de Mozilla, que publica curl.se: completo, pero falla con CA corporativas.
- Solo exportar el almacén de Windows: Windows descarga muchas raíces solo cuando las necesita, así que la exportación puede salir incompleta.
- La combinación de las dos.
- Aplazar.

## Decisión

- **Base:** el `cacert.pem` de Mozilla publicado por curl.se entra como **entrada del catálogo** (`format = "file"`, con `sha256` y `size`), con la misma verificación que cualquier descarga (ADR-0005). Se instala y desinstala como un paquete más, por ejemplo con el servicio `cacert`. Se renueva al publicar un catálogo nuevo.
- **Agregado local:** al generar la configuración de PHP se agregan los certificados raíz **confiables del almacén de Windows**: `ROOT` del usuario y del equipo, incluidos los que distribuye la empresa por directiva. Se omiten los que ya están en el bundle de Mozilla, y nunca se incluyen los del almacén `Disallowed`. **Solo se agregan raíces aptas para autenticar servidores TLS** (ver «Corrección»). La exportación es específica de Windows: va en `crisol-platform`, detrás de un trait del núcleo.
- **Resultado:** un archivo **generado** en `etc/php/cacert.pem`, derivado como todo lo de `etc/`. El `php.ini` generado fija **solo `curl.cainfo`** a esa ruta; `openssl.cafile` queda como PHP lo trae (ver «Corrección»). Se regenera junto con el `php.ini`.
- **Sin bundle instalado:** se genera el `php.ini` sin esas directivas y con una advertencia, como hasta ahora. Crisol no bloquea por esto.

## Consecuencias

- El catálogo tiene un paquete nuevo y `analisis/fuentes-binarios.md` registra su fuente y cómo se verificó. Como Mozilla actualiza el bundle cada pocas semanas, conviene renovarlo en cada publicación del catálogo.
- Un usuario detrás de un proxy con inspección TLS funciona sin configurar nada, siempre que la CA de la empresa esté en el almacén de Windows, que es lo normal.
- Un certificado raíz agregado a Windows después de generar el archivo no se ve hasta regenerarlo. `doctor` (F11.1) puede comparar el archivo con el almacén y avisar.
- `php.ini` y sus `overrides/` siguen mandando: si el usuario fija `curl.cainfo` en un override, se respeta.
- Pruebas: la combinación y la deduplicación se prueban con PEM literales y un doble del trait. La exportación real se prueba solo para Windows, sin modificar el almacén del sistema.

## Implementación (2026-10-14)

- Catálogo: paquete `cacert`, versión `2026.9.25` (la fecha del bundle), `format = "file"`; URL con fecha. Fuente y comprobación en `analisis/fuentes-binarios.md`.
- Núcleo (`crisol-core::cacert`): trait `RootCertificates`, `merge_bundle` (el texto de Mozilla se copia **tal cual** y las raíces de más van después, sin duplicados y en orden estable) y `write_php_cacert`, que escribe `etc/php/cacert.pem` solo si cambió. `write_php_ini` fija `curl.cainfo` y `openssl.cafile` cuando ese archivo existe; si no, avisa en `WrittenPhpIni::warnings` y deja las directivas como PHP las trae.
- Plataforma (`crisol-platform::WindowsRootCertificates`): lee el almacén `ROOT` del usuario, del equipo, de la directiva de grupo (usuario y equipo) y de empresa, en solo lectura y solo si el almacén existe; descarta todo lo que esté en `Disallowed` de los mismos sitios.
- Si leer el almacén de Windows falla, el archivo se genera solo con Mozilla y se avisa; un almacén roto no deja a PHP sin certificados.
- Los overrides siguen mandando: se añaden al final del `php.ini` y PHP conserva la última asignación (hay prueba).
- Comprobado con PHP 8.4.26 real: sin el bundle, cURL falla con "unable to get local issuer certificate"; con el bundle generado, `curl_exec` a un sitio público devuelve 200, y los *streams* siguen funcionando. En el equipo de pruebas Windows aportó 54 raíces que Mozilla no tiene.
- Comprobado en F4.3 con el `composer.phar` real sobre PHP 8.4.26: Composer habla HTTPS con Packagist tanto **sin** el bundle (el `php.ini` sin `curl.cainfo`) como con él, así que no lo necesita, como suponía el ADR (trae su propio `cacert.pem`).

## Corrección (2026-10-14, revisión de la Fase 4)

La primera versión de este ADR tenía dos errores de redacción, que la implementación siguió al pie de la letra:

1. **No decía qué raíces de Windows sirven para TLS.** En el almacén `ROOT`, muchas raíces llevan **propiedades** que limitan su uso:
   - uso extendido (`CERT_ENHKEY_USAGE_PROP_ID`): solo firma de código, correo o sellado de tiempo;
   - desconfianza parcial (`CERT_DISALLOWED_FILETIME_PROP_ID`, `CERT_DISALLOWED_ENHKEY_USAGE_PROP_ID`): así retira Microsoft una CA sin borrarla.

   Al copiar solo el DER se pierden, y OpenSSL confía en esas raíces para cualquier servidor. El archivo terminaba confiando en **más** que Windows y que Mozilla.
2. **Fijaba `openssl.cafile`.** Con ese valor vacío, PHP en Windows verifica los *streams* (`file_get_contents`, `fopen` de `https://`) con el almacén del sistema **en vivo**: Windows descarga las raíces que le faltan, aplica las desconfianzas de Microsoft y ve una CA corporativa recién instalada. Al fijarlo, los *streams* pasaban a depender de una copia tomada en un momento dado, aunque antes funcionaban bien.

Se decide:

- **Filtro de raíces de Windows.** Una raíz del almacén se agrega solo si cumple las tres condiciones:
  - no tiene propiedad de uso extendido, o la que tiene incluye `serverAuth` (`1.3.6.1.5.5.7.3.1`);
  - no tiene `CERT_DISALLOWED_FILETIME_PROP_ID` ni un `CERT_DISALLOWED_ENHKEY_USAGE_PROP_ID` que excluya `serverAuth`;
  - no está en `Disallowed` (como antes).

  Las que no pasan el filtro se cuentan y se informan en `WrittenCacert::warnings` como "omitidas por uso o desconfianza", nunca en silencio.
- **El trait cambia:** `RootCertificates` devuelve, para cada raíz, el DER **y** lo que Windows dice de su uso: los usos permitidos (`None` = sin restricción) y si tiene desconfianza parcial. Así el filtro es lógica pura del núcleo, con pruebas sobre un doble, y la plataforma solo lee propiedades.
- **Solo `curl.cainfo`.** El `php.ini` generado deja de fijar `openssl.cafile`. cURL usa el archivo generado; los *streams* siguen con el almacén de Windows en vivo, como hacía PHP antes de Crisol.

Consecuencias:

- El número de raíces que aporta Windows bajó. Medido en el equipo de pruebas tras la corrección: 93 raíces de confianza en los almacenes, 69 aptas para TLS y 24 omitidas por uso o desconfianza parcial (las 54 de la primera medición incluían raíces solo de firma de código y heredadas).
- Pruebas nuevas (hechas):
  - una raíz con uso solo de firma de código no entra;
  - una con `serverAuth` sí entra;
  - una con desconfianza parcial no entra;
  - el `php.ini` generado no contiene `openssl.cafile =` activo.
- Implementación: `RootCertificates::trusted_roots` devuelve `SystemRoot { der, usages, partially_distrusted }`; `SystemRoot::is_fit_for_tls` es el filtro (núcleo, con doble). `WindowsRootCertificates` lee la propiedad de uso extendido (`CERT_ENHKEY_USAGE_PROP_ID`) y marca desconfianza parcial si existe `CERT_DISALLOWED_FILETIME_PROP_ID` o `CERT_DISALLOWED_ENHKEY_USAGE_PROP_ID` (cualquier contenido de esta última cuenta: solo puede volver el filtro más estricto). Un uso que no se puede leer se trata como el más estricto; un certificado presente en varios almacenes conserva la lectura más restrictiva.
- Queda abierto, sin decidir aquí: aplicar también `Disallowed` a los bloques de Mozilla (hallazgo 6 de la revisión de la Fase 4).

## Corrección (2026-10-16): `Disallowed` también se aplica a Mozilla

Decidida por el propietario (hallazgo 6 de la revisión de la Fase 4).

- Si un certificado del bundle de Mozilla está en un almacén `Disallowed` de Windows (usuario, equipo, directiva o empresa; comparación exacta del DER), **se excluye** de `etc/php/cacert.pem`.
- Se informa cuántos se excluyeron y cuáles (por su nombre) en `WrittenCacert::warnings`.
- El archivo deja de ser una copia literal del bundle. Un comentario al inicio lo dice y nombra la versión del bundle de origen.
- El trait `RootCertificates` devuelve también la lista de desconfiados.
- Pruebas con un doble: una CA de Mozilla que está en `Disallowed` no aparece en el resultado, y el resto del bundle queda igual, en el mismo orden.

### Cómo quedó (2026-10-16)

- `RootCertificates::disallowed_roots` devuelve el DER de los almacenes `Disallowed` de usuario, equipo, directiva y empresa (`WindowsRootCertificates` lo lee con el mismo código que ya los usaba para filtrar `ROOT`).
- `exclude_certificates` (núcleo, puro) quita del texto de Mozilla cada entrada cuyo DER está en esa lista: el bloque, las líneas de nombre justo encima y la línea en blanco que lo sigue. Lo demás queda igual y en el mismo orden. La CA local de Crisol se añade después y no se filtra por esta lista.
- El archivo empieza con un comentario que dice que ya no es una copia literal, nombra la versión del bundle de origen y lista los CN excluidos (`Left out: none.` si no hubo). `WrittenCacert::excluded` los lleva y una advertencia dice cuántos y cuáles.
- Si la lista de `Disallowed` no se puede leer, el bundle de Mozilla se deja entero y una advertencia lo dice.

## Corrección: quién lo genera y cuándo (2026-10-17, V1 de la verificación manual de la Fase 9)

Las funciones existían, pero nadie las llamaba: PHP corría sin `php.ini` ni `cacert.pem`. Ahora lo hace `crisol-core::phpsetup::refresh_php` (primero `etc/php/cacert.pem` con la CA local validada si existe y las raíces de Windows aptas para TLS; después el `php.ini` de cada versión), y se llama:

- en `WebStack::apply`, antes de crear o arrancar un pool (así el `start` general, cada recarga y la creación de la CA lo dejan al día);
- al cambiar `[php]` en `crisol.toml` (la vigilancia lo detecta): se regeneran los `php.ini` y se reinician **solo** los pools cuyo archivo cambió o cuyo `max_requests` cambió, esperando a que sus puertos queden libres;
- al instalar PHP o el paquete `cacert`, con o sin daemon (`Engine`), para que el shim encuentre su `php.ini`.

**Decisión: `crisol install php` instala también `cacert` si falta.** Es un archivo pequeño, con hash fijado en el catálogo, y sin él el cURL de PHP no confía en ningún sitio HTTPS (el síntoma que el usuario vería primero es un `curl error 60`). Si la descarga falla, PHP queda instalado igual; el siguiente `start` avisa «the CA bundle is not installed (`crisol install cacert`)» y el `php.ini` se genera sin `curl.cainfo`. Se conserva lo decidido antes: solo `curl.cainfo`, los *overrides* al final y escritura solo si cambió.
