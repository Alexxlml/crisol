# ADR-0012 — Versión de PHP predeterminada y PHP 7.4 como compatibilidad

- **Estado:** aceptada (decidida por el usuario el 2026-10-14, a raíz de la revisión de la Fase 4, hallazgo 9). Pendiente de implementar.
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
