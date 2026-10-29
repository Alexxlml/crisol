# ADR-0012 — Versión de PHP predeterminada y PHP 7.4 como compatibilidad

- **Estado:** aceptada (decidida por el usuario el 2026-10-14, a raíz de la revisión de la Fase 4, hallazgo 9). **Implementada** el 2026-10-14.
- **Fecha:** 2026-10-14

## Contexto

Cuando un proyecto pide una restricción abierta (`"php": ">=7.4"` en `composer.json`), `resolve` (F4.2) elige la versión **más alta** instalada que la cumple. Con 8.3, 8.4 y 8.5 instaladas, un WordPress viejo termina en la 8.5, aunque haya una predeterminada global. Si nada pide versión, el paso 6 también elige la más alta.

El usuario decidió:
- la versión que Crisol usa por defecto es la **línea 8.4** hasta que la 8.5 sea de uso común;
- hay que poder levantar proyectos viejos (un WordPress que solo se quiere revisar o actualizar), así que **PHP 7.4** debe poder instalarse como compatibilidad.

Interpretación del "8.4^" del usuario: la **línea 8.4** (cualquier 8.4.x, el parche más alto instalado), no `^8.4` como restricción, que incluiría la 8.5.

## Decisión

- **Línea recomendada en el catálogo.** El catálogo declara, por servicio, la línea que Crisol usa por defecto. Por ejemplo, `[recommended]` con `php = "8.4"`. Vive en el catálogo, no en el binario, para cambiarla a `8.5` publicando un catálogo nuevo y firmado (ADR-0005) cuando llegue el momento.
- **Predeterminada efectiva.** Es la de `[defaults]` en `crisol.toml` si el usuario la fijó y, si no, la línea recomendada del catálogo.
- **Restricciones abiertas.** Si la predeterminada efectiva está instalada y **cumple** la restricción del proyecto, se usa esa versión (el parche más alto instalado de esa línea). Si no la cumple, se usa la versión más alta instalada que la cumpla, como hasta ahora. Ejemplos con 7.4, 8.4 y 8.5 instaladas:
  - `>=7.4` → 8.4;
  - `>=7.4 <8.0` → 7.4;
  - `^8.5` → 8.5.
- **Sin ninguna fuente de versión** (paso 6 de `resolve`): se usa la predeterminada efectiva si está instalada; si no, la más alta estable instalada, como hasta ahora.
- **Versiones fijadas explícitamente** (`CRISOL_PHP`, `.crisol.toml`, `.php-version`, `crisol use`) no cambian: se pide una versión concreta y se respeta.
- **PHP 7.4 en el catálogo como versión sin soporte.** Se añade la última 7.4 (7.4.33, NTS x64, de `windows.php.net/downloads/releases/archives/`, verificada como las demás). La entrada lleva una marca de "sin soporte" (por ejemplo, `end_of_life = true`):
  - `install` la instala, pero avisa de que ya no recibe parches de seguridad;
  - el shim no avisa en cada ejecución, para no ensuciar la salida de WordPress o WP-CLI;
  - `doctor` (F11.1) la lista como versión sin soporte instalada.
- **La 7.4 nunca es predeterminada** ni se elige por el paso 6 mientras haya una versión con soporte instalada. Solo se usa cuando el proyecto la pide o la restricción la exige.

## Consecuencias

- Cambia el formato del catálogo: `[recommended]` y `end_of_life`. `docs/catalog.md` debe documentarlo, y la validación estricta debe aceptarlo. Un catálogo sin `[recommended]` sigue siendo válido: entonces la predeterminada efectiva es solo la de `crisol.toml` y, si tampoco hay, la más alta estable.
- `resolve.rs` necesita la predeterminada efectiva **antes** del paso 4, no solo en el paso 5. Hace falta probar los ejemplos de arriba, el caso sin `[recommended]` y que la 7.4 no se elija por defecto.
- **PHP 7.4 para Windows** está compilado con Visual C++ 2017 (`vc15`). El paquete redistribuible 2015-2022 que usan las 8.x lo cubre, pero hay que comprobarlo con el binario real.
  - Su `php.ini-development` es más antiguo: F4.1 ya omite las extensiones cuya DLL falta, pero hay que comprobar el `php.ini` generado con la 7.4 real.
  - La extensión `openssl` de 7.4 y su cURL también usan el `curl.cainfo` del ADR-0011.
- Las líneas intermedias (8.0, 8.1, 8.2) no se deciden aquí. Si hacen falta, se añaden con el mismo criterio (8.0 sin soporte; 8.1 y 8.2 según su estado en ese momento).

## Implementación (2026-10-14)

- Catálogo (`serial = 4`): sección `[recommended]` (`php = "8.4"`), campo `end_of_life` por paquete y PHP 7.4.33 (`vc15`). `Catalog::recommended`, `Catalog::is_end_of_life`; `latest` prefiere una versión con soporte. `Installed::end_of_life` avisa a quien instala.
- `resolve`: `ResolveContext` recibe el catálogo. La predeterminada efectiva (`[defaults]` o la línea recomendada) se calcula antes del paso 4; con una restricción abierta se usa si la cumple y, si no, la más alta que la cumpla. El paso 6 elige la línea recomendada si está instalada (`Source::CatalogRecommendation`) y, si no, la más alta estable **con soporte**; una sin soporte solo si no hay otra. Las versiones fijadas a mano no cambian.
- Comprobado con PHP 7.4.33 real: arranca con el `php.ini` generado sin advertencias, carga `curl`, `mbstring`, `openssl`, `pdo_sqlite` y `opcache`, y cURL con el bundle del ADR-0011 llega a un sitio HTTPS. El redistribuible 2015-2022 de esta máquina basta para la `vc15`.
- El ZIP de 7.4.33 no trae hash ni firma del proveedor: ver la tabla de `analisis/fuentes-binarios.md` (certeza menor).
- Sigue sin hacer: el aviso al instalar y la lista en `doctor` (CLI, Fase 8 y F11.1).

## Ampliación a Node (2026-10-17, F10.1)

El mecanismo es el mismo y se aplica a Node sin cambios en el código: `[recommended] node = "24"` (la LTS en activo; al pasar la 26 a LTS y estabilizarse se cambia con un catálogo nuevo) y Node 20.20.2 con `end_of_life = true`, instalable para proyectos viejos y nunca elegida por defecto ni por `latest`. Con 20, 22 y 24 instaladas, `engines.node = ">=18"` resuelve a 24 y `".nvmrc"` con `20` da 20.

## Ampliación: `crisol install` sin versión (2026-10-29, catálogo `serial = 13`)

Al añadir Node 26 apareció un desajuste: `crisol install node` pedía `latest` e instalaba 26.13.0, mientras la predeterminada efectiva seguía siendo 24. Con PHP pasaba lo mismo (`crisol install php` daba 8.5 y la recomendada es 8.4). El propietario decidió (2026-10-29) que **sin versión se instala la línea recomendada**: lo que se instala es lo que Crisol usará.

- `Call::Install.version` pasa a ser opcional en el protocolo. Sin él, el motor pide `[recommended]` del servicio y, si no hay, `latest`. Un cliente que manda `"latest"` sigue recibiendo la más alta.
- El CLI ya no rellena `latest` por defecto. `--from` exige escribir la versión (clap y el motor lo comprueban): un archivo no tiene versión predeterminada.
- `[defaults]` de `crisol.toml` no interviene: se usa solo la línea del catálogo. Si se quiere que una predeterminada del usuario también decida qué se instala, el cambio va en `Engine::requested`.
