# ADR-0011 — Certificados raíz para PHP: Mozilla más el almacén de Windows

- **Estado:** aceptada (decidida por el usuario el 2026-10-14)
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
- **Agregado local:** al generar la configuración de PHP se agregan los certificados raíz **confiables del almacén de Windows**: `ROOT` del usuario y del equipo, incluidos los que distribuye la empresa por directiva. Se omiten los que ya están en el bundle de Mozilla, y nunca se incluyen los del almacén `Disallowed`. La exportación es específica de Windows: va en `crisol-platform`, detrás de un trait del núcleo.
- **Resultado:** un archivo **generado** en `etc/php/cacert.pem`, derivado como todo lo de `etc/`. El `php.ini` generado fija `curl.cainfo` y `openssl.cafile` a esa ruta. Se regenera junto con el `php.ini`.
- **Sin bundle instalado:** se genera el `php.ini` sin esas directivas y con una advertencia, como hasta ahora. Crisol no bloquea por esto.

## Consecuencias

- El catálogo tiene un paquete nuevo y `analisis/fuentes-binarios.md` registra su fuente y cómo se verificó. Como Mozilla actualiza el bundle cada pocas semanas, conviene renovarlo en cada publicación del catálogo.
- Un usuario detrás de un proxy con inspección TLS funciona sin configurar nada, siempre que la CA de la empresa esté en el almacén de Windows, que es lo normal.
- Un certificado raíz agregado a Windows después de generar el archivo no se ve hasta regenerarlo. `doctor` (F11.1) puede comparar el archivo con el almacén y avisar.
- `php.ini` y sus `overrides/` siguen mandando: si el usuario fija `curl.cainfo` en un override, se respeta.
- Pruebas: la combinación y la deduplicación se prueban con PEM literales y un doble del trait. La exportación real se prueba solo para Windows, sin modificar el almacén del sistema.
