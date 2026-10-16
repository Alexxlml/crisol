# ADR-0013 — HTTPS local: una CA restringida a `.test` y un certificado comodín

- **Estado:** aceptada (decidida por el usuario el 2026-10-15, tras la revisión de la Fase 6 propuesta); **corregida el 2026-10-16** al implementar F6.2: sin comodín (ver «Corrección»)
- **Fecha:** 2026-10-15

## Contexto

La Fase 6 da HTTPS a los sitios locales. Un único certificado autofirmado que no se instala en ningún almacén hace que el navegador avise siempre. `docs/plan.md` propone una CA local propia (`rcgen`), un certificado por sitio o comodín, y un `TrustStore` que instale **solo la CA** en `CurrentUser\Root` (sin admin; Windows muestra un diálogo de confirmación).

Riesgos y huecos que la revisión de la propuesta encontró:

- Una CA de confianza en el almacén del usuario puede firmar certificados de **cualquier** dominio. Si su clave se roba, el atacante puede suplantar `banco.com` para ese usuario.
- El plan solo contemplaba el navegador. PHP (`curl`) y Node validan contra sus propios almacenes: un `curl` de Laravel a `https://otro-sitio.test` fallaría con el mismo error que corrigió el ADR-0011.
- Un certificado por sitio obliga a emitir y recargar cada vez que se agrega un sitio.
- La instalación real en el almacén de Windows muestra un diálogo y modifica el almacén del usuario: no puede probarse de forma automática sin tocar el sistema.
- Firefox no usa el almacén de Windows.

## Decisión

**La CA**
- Una CA local por equipo y usuario, generada una vez con `rcgen`: ECDSA P-256, `BasicConstraints` con `pathlen = 0` (solo firma hojas), uso de clave `keyCertSign` y `cRLSign`, vigencia de 10 años. Su nombre incluye un identificador corto derivado de su clave pública (`Crisol Local CA 7F3A9C`), para distinguirla en el almacén de confianza y poder quitarla sin dudas.
- **Restricciones de nombres (críticas):** la CA solo puede emitir para `test`, `localhost`, el TLD configurado en `crisol.toml` (si es otro), `127.0.0.0/8` y `::1`. Si su clave se roba, no sirve para suplantar dominios públicos.
- La clave vive en `certs/ca.key` y el certificado en `certs/ca.pem`, dentro de la home privada (ADR-0008). Nunca sale de ahí ni se exporta. Proteger la clave con DPAPI queda como mejora posible (complica mover la carpeta entre equipos).
- Cambiar el TLD a uno que la CA no cubre exige una CA nueva con un **comando explícito** (hay que volver a confiar en ella); Crisol nunca la regenera sola, porque dejaría una CA vieja en el almacén y una clave sin respaldo.
- Si `certs/` tiene solo uno de los dos archivos, o no forman pareja, es un error que se informa; nunca se sobrescribe en silencio.

**El certificado de hoja**
- Uno solo, comodín: SAN `*.<tld>`, `<tld>`, `localhost`, `127.0.0.1` y `::1`; uso de clave `digitalSignature`; uso extendido `serverAuth`; no es CA. Vigencia de 1 año. Un sitio nuevo no emite nada.
- Se renueva cuando faltan menos de 30 días o cuando cambian los SAN esperados (se comparan con los del certificado, sin archivo marcador).
- Los subdominios anidados (`a.b.test`) no los cubre un comodín; es una limitación aceptada y documentada.
- Se genera con una clave propia (no la de la CA) en `certs/`.

**Confianza**
- Trait `TrustStore` en el núcleo, con un doble. La implementación de Windows instala **solo la CA** en `CurrentUser\Root` (sin admin) y la quita con un comando; se hace con el consentimiento explícito del usuario y Windows muestra su diálogo.
- **Las pruebas automáticas nunca tocan el almacén real:** la implementación de Windows se prueba contra un almacén privado (en memoria o en archivo). La instalación real, con su diálogo, la prueba **a mano el propietario** una vez, al final de la Fase 6; el resultado se anota en la bitácora.
- **PHP:** la CA local se agrega directamente a `etc/php/cacert.pem` (ADR-0011) como entrada propia, **independiente del almacén de Windows**, para que `curl` confíe en los sitios locales aunque el usuario no haya instalado la CA en el sistema.
- **Node:** el shim fija `NODE_EXTRA_CA_CERTS=certs/ca.pem` (salvo que el usuario ya la tenga fijada).
- **Firefox** usa su propio almacén y no se toca: se documenta que seguirá mostrando el aviso, y cómo importar `certs/ca.pem` a mano si se quiere.

**Qué significa «asegurar» un sitio**
- `sites.toml` gana una lista `secure = ["nombre", …]` (para que valga también para los sitios *parked*, que no tienen entrada propia). `crisol secure <sitio>` y `crisol unsecure <sitio>` la editan con `TomlEditor`.
- En Nginx, un sitio asegurado escucha en `127.0.0.1:<https_port>` con `ssl` y el certificado comodín (TLS 1.2 y 1.3), y su server block HTTP redirige a HTTPS (con el puerto en la URL si no es el 443).
- Con Herd instalado en esta máquina, 80 y 443 pueden estar ocupados: el puerto lo decide `crisol.toml`, y las pruebas usan el rango 19000–19999.

**Pruebas**
- La prueba real de Nginx que comprueba que el código de PHP nunca se sirve (revisión de la Fase 5, hallazgo 1) se repite sobre el puerto TLS.
- La validación de la cadena se prueba con un cliente que confía **solo** en `certs/ca.pem` (sin tocar el sistema): válida con la CA, rechazada sin ella y rechazada para un nombre fuera de las restricciones.

## Consecuencias

- Dependencias nuevas, justificadas en la bitácora: `rcgen` (CA y certificados; backend `ring`, que ya compila `ureq`; sin `aws-lc`, que exigiría CMake/NASM en Windows), `x509-parser` (leer la CA y el certificado de hoja: parejas, vigencia, SAN y restricciones) y `time` (fechas de validez).
- `crisol-core` genera y lee certificados (portable); la instalación en el almacén de Windows va en `crisol-platform` detrás del trait.
- Cambiar el TLD tiene un coste visible (CA nueva y volver a confiar), que `doctor` (F11.1) debe explicar.
- Orden de implementación: F6.1 (CA con restricciones) → F6.2 (hoja comodín, renovación e integración en `nginx.rs`, con snapshots y `nginx -t` real) → F6.3 (`TrustStore` y su implementación de Windows sobre un almacén privado) → la CA en `cacert.pem` y en `NODE_EXTRA_CA_CERTS` → prueba manual del propietario.

## Alternativas descartadas

- **Un certificado por sitio:** más emisiones y recargas por cada sitio nuevo, sin beneficio frente a un comodín restringido a `.test`.
- **CA sin restricciones de nombres:** el robo de su clave permitiría suplantar cualquier dominio.
- **Instalar la CA en el almacén de la máquina:** exige administrador y amplía el alcance a todos los usuarios del equipo.
- **Importar la CA a Firefox (NSS):** hay que tocar cada perfil; fuera de alcance.

## Corrección (2026-10-16, F6.2): el certificado no lleva comodín

La decisión original era un único certificado con SAN `*.<tld>`. Al implementarlo y verificarlo con un validador real (`rustls-webpki`, el de los clientes Rust y Deno) resultó que **rechaza todo el certificado** si contiene un comodín directamente bajo un TLD de una sola etiqueta (`*.test`), con `MalformedDnsIdentifier`, **aunque el host pedido figure también de forma explícita** en la lista (hay una prueba que lo fija). Navegadores y OpenSSL (PHP, Node) sí lo aceptan, pero un certificado que un cliente de la propia herramienta rechaza no es aceptable.

Se decide:

- El certificado de hoja lista **los hosts de los sitios asegurados** (`shop.test`…), más `localhost`, `127.0.0.1` y `::1`. Sin `*.<tld>` y sin el TLD suelto.
- Es más estricto que lo anterior: el certificado solo avala los sitios que el usuario pidió asegurar (menor superficie), y la CA sigue restringida a `.test`, `localhost`, etc. por sus *name constraints*.
- Asegurar o desasegurar un sitio cambia la lista y genera un certificado nuevo (clave nueva, 1 año); eso va acompañado de la recarga de Nginx que ya ocurre al cambiar la configuración. Un certificado vigente, con la lista igual en cualquier orden, se conserva. La renovación (30 días antes), por CA distinta o por archivos dañados, sigue igual.
- Si el certificado cambia pero el texto de la configuración no, `WebStack` fuerza igualmente la prueba y la recarga, porque Nginx mantiene el certificado viejo en memoria.
- Se pierde la propiedad «un sitio nuevo no necesita emitir nada» para los sitios asegurados; los no asegurados nunca lo necesitaron (se sirven por HTTP).

## Implementación de F6.2 (2026-10-16)

- `crisol-core::tls` (`ensure_site_cert`, `expected_names`, `Renewal`): hoja ECDSA P-256 con clave propia en `certs/sites.pem` y `certs/sites.key`, `digitalSignature` + `serverAuth`, firmada por la CA.
- `sites.toml`: lista `secure = ["nombre", …]` (parked y linked); `set_secure` la edita con `TomlEditor` conservando lo demás; `Site::secure`.
- `crisol-core::nginx`: `NginxTls`; un sitio asegurado escucha en `127.0.0.1:<https_port> ssl` con `http2 on`, TLS 1.2 y 1.3 y `fastcgi_param HTTPS on`; su bloque HTTP solo redirige con `301` (con el puerto si no es el 443); un `default_server` TLS con `ssl_reject_handshake on` rechaza los demás nombres. Sin certificado, el sitio se sirve por HTTP con una advertencia.
- `WebStack::apply` crea la CA si falta (`Applied::ca_created`: el usuario debe confiar en ella) y la hoja, y las usa en la configuración.
- Verificado: el verificador de `rustls-webpki` con la CA como única raíz acepta los hosts asegurados y los locales y rechaza otros nombres, fechas fuera de rango y un certificado que la propia CA firmara para `bank.example.com` (`NameConstraintViolation`). Prueba real a pedido con Nginx 1.28.3 y PHP 7.4.33: `curl` (Schannel) con `--cacert certs/ca.pem` recibe la página con `HTTPS=on`; sin la CA falla; `/v.PHP`, `/v.Php`, `/v.php.` y `/v.php%20` ejecutan el script por TLS y nunca devuelven el código; HTTP redirige a `https://…:<puerto>/…`; un sitio no asegurado recibe el handshake rechazado por TLS y sigue sirviéndose por HTTP.

## Corrección (2026-10-16, revisión de la Fase 6)

Decidida por el propietario a partir de los hallazgos 1 y 2 de `claude/revisiones/2026-10-16-opus-fase6.md`. **Implementada** el 2026-10-16 (ver «Implementación de la corrección de la revisión de la Fase 6»).

- **Solo TLD de uso especial.** Las restricciones de la CA solo pueden incluir `test`, `localhost`, `example`, `invalid`, `local`, `internal` y `home.arpa`, y sus subdominios. Con otro TLD (`dev`, `app`, `com`…) no se crea la CA y los sitios se sirven por HTTP con una advertencia. Si la clave se filtrara, una CA restringida a un TLD público podría suplantar dominios reales.
- **La CA se valida antes de cualquier confianza.** Antes de instalarla en el almacén, agregarla a `cacert.pem` o dársela a Node, se comprueba que:
  - es autofirmada, con `pathlen = 0`;
  - sus restricciones de nombres son críticas y no van más allá de lo anterior;
  - ni `certs/` ni sus archivos están abiertos a otras cuentas (ADR-0008, «Cambio de calendario»).

  Node recibe una copia generada y validada, no `certs/ca.pem` directamente.

## Implementación de la corrección de la revisión de la Fase 6 (2026-10-16)

Hecho, hallazgo por hallazgo de `claude/revisiones/2026-10-16-opus-fase6.md`:

- **La CA se valida siempre (hallazgo 1).** Un `LocalCa` solo puede salir de `ensure_ca`, `load_ca` o `create_ca`, que exigen en cada llamada: que `certs/` y los archivos de la CA no sean escribibles por otras cuentas (`PrivateDirs::is_writable_by_others`; si lo son, `CaError::Exposed` con la solución); que sea autofirmada, con `pathlen = 0` y `keyCertSign`; y restricciones de nombres **presentes y críticas** cuyos subárboles permitidos son solo nombres DNS de uso especial, `127.0.0.0/8` y `::1`, sin otros tipos de nombre. Como `trust_ca`, `write_php_cacert_with` (ahora recibe `Option<&LocalCa>`) y `write_node_ca` reciben un `LocalCa`, ninguno puede usar una CA sin pasar por esa validación. `certs/` se crea siempre con `PrivateDirs::create_private_dir` (y la home, si la crea Crisol), aunque el resto de la estructura no sea privada todavía (ADR-0008, «Cambio de calendario»).
- **Node recibe una copia (hallazgo 1).** `write_node_ca` escribe `etc/node/ca.pem` (carpeta privada propia) a partir de la CA validada, y el shim apunta `NODE_EXTRA_CA_CERTS` a ese archivo, nunca a `certs/ca.pem`.
- **Solo TLD de uso especial (hallazgo 2).** `test`, `localhost`, `example`, `invalid`, `local`, `internal` y `home.arpa` (y subdominios). Con otro TLD no se crea la CA ni el certificado, ni siquiera se toca `certs/`: los sitios asegurados se sirven por HTTP con una advertencia (`CaError::TldNotSpecialUse`). `Config` avisa con `ConfigWarning::PublicTld` cuando `sites.tld` no es de uso especial.
- **Certificado y configuración entran juntos (hallazgo 3).** `apply` solo *planifica* el certificado (`plan_site_cert`: el par nuevo queda en memoria). Al recargar, el par se escribe en `certs/.next/` (privada), la configuración preparada en `etc/nginx/.next/` apunta a él, `nginx -t` prueba el conjunto y solo si pasa se instalan primero el certificado y después la configuración. Si `-t` falla, ni `certs/` ni `etc/nginx/` cambian. Un certificado que cambia con la misma configuración también pasa por este camino.
- **El puerto HTTPS cuenta (hallazgo 4).** Con sitios asegurados, `https_port` entra en la comprobación previa y en la salud de Nginx: con otro programa en el 443 se informa quién lo usa (`PortBusy`) en lugar de ver salir a Nginx.
- **Redirección `302` (hallazgo 5)**, no `301`: el navegador no la recuerda para siempre tras desasegurar un sitio.

### Los clientes aplican las restricciones de nombres (hallazgo 6)

La garantía de la CA (una clave robada no sirve para suplantar un sitio público) depende de que cada cliente aplique las restricciones de la raíz en la que confía. Se comprobó con certificados reales: una hoja legítima para `ok.test` y otra para `bank.example.com` **firmada por la propia CA con su clave**, servidas por Nginx 1.28.3 (`crisol-platform/tests/tls_clients.rs`, a pedido):

| Cliente | `ok.test` (legítimo) | `bank.example.com` (firmado por la CA) |
|---|---|---|
| `curl` de Windows (Schannel, solo `--cacert`) | acepta | **rechaza**: `schannel: error 0x00004000` (`CERT_TRUST_HAS_NOT_PERMITTED_NAME_CONSTRAINT`) |
| cURL de PHP 7.4.33 (OpenSSL, `cacert.pem` generado) | acepta | **rechaza**: `SSL certificate problem: permitted subtree violation` |
| Node 22.23.3 (OpenSSL, `NODE_EXTRA_CA_CERTS` con la copia) | acepta | **rechaza**: `permitted subtree violation` |
| `rustls-webpki` (pruebas del núcleo) | acepta | **rechaza**: `NameConstraintViolation` |

No se probó Edge/Chrome (su verificador es el de Chromium, que aplica las restricciones de nombres desde hace años): si se quiere comprobar a mano, basta servir la hoja falsa de ese test y abrirla con la CA instalada.

### Los *streams* de PHP (hallazgo 7)

La CA local va en `etc/php/cacert.pem`, que solo usa cURL; `openssl.cafile` no se fija (ADR-0011, corrección), así que los *streams* de PHP (`file_get_contents('https://…')`, `fopen`) usan el almacén de Windows en vivo. Consecuencia: **si el usuario no instaló la CA en Windows o dijo que no, cURL confía en sus sitios locales pero `file_get_contents` no**. Es coherente con lo decidido y no se vuelve a fijar `openssl.cafile`. `doctor` (F11.1) debe decirlo: «PHP con *streams* no confía en tus sitios locales hasta que confíes en la CA (`crisol trust`)».

### Límite que queda

`etc/php/cacert.pem` (y los demás derivados de `etc/`) heredan la ACL de la home. Mientras la home no se cree privada (Fase 8), quien pudiera escribir ahí podría cambiarlo; por eso `etc/node/` se crea privada por sí misma y por eso la creación de la home privada sigue siendo prioritaria en la Fase 8.
