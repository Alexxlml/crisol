# ADR-0003 — La configuración del usuario es la fuente de verdad

- **Estado:** aceptada
- **Fecha:** 2026-10-13

## Contexto

En Devaron, el cambio manual de puertos de Nginx (8000/8443 → 80/443) en `config\services.json` apareció revertido después: la aplicación reescribe su propia configuración. Eso hace imposible confiar en cualquier ajuste manual.

## Decisión

- Archivos del usuario: `crisol.toml`, `sites.toml`, `.crisol.toml` por proyecto y `etc/*/overrides/`. Crisol **solo** los modifica con un comando explícito del usuario, usando `toml_edit` para conservar comentarios y orden, con escritura atómica.
- Archivos generados: todo lo demás en `etc/`. Se regeneran a partir de los del usuario y llevan un encabezado de "archivo generado, no editar; usa overrides/".
- Estado interno (`state/`): solo Crisol lo lee y escribe; se puede borrar sin perder configuración.

## Consecuencias

- Toda personalización que una plantilla no cubra va en `overrides/`, que se incluye y nunca se pisa.
- Las pruebas deben verificar que una operación cualquiera no altera los archivos del usuario.
