# ADR-0002 — Shims en lugar de modificar el PATH por versión

- **Estado:** aceptada
- **Fecha:** 2026-10-13

## Contexto

Las herramientas actuales agregan al PATH la carpeta de una versión concreta de PHP o Node. Al cambiar de versión o desinstalar, quedan entradas huérfanas, y dos herramientas instaladas se pisan entre sí.

## Decisión

Crisol agrega **una sola entrada** al PATH del usuario: `CRISOL_HOME\shims`, y nunca la cambia. En esa carpeta hay ejecutables mínimos (`php`, `composer`, `node`, `npm`, `npx`) que resuelven la versión correcta según el proyecto y ejecutan el binario real.

## Consecuencias

- Cambiar de versión no toca el PATH ni el registro.
- Cada proyecto puede usar su propia versión sin que cambie la de otros.
- El shim debe ser extremadamente rápido, porque se ejecuta en cada invocación de `php`.
- `crisol doctor` debe advertir si otro `php.exe` aparece antes en el PATH.
