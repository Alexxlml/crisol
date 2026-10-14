# ADR-0005 — Verificación de descargas y actualizaciones

- **Estado:** propuesta (pendiente de aprobación del usuario)
- **Fecha:** 2026-10-13

## Contexto

Una suite que trae todos sus binarios en el paquete y se actualiza bajando un ZIP completo depende por completo de que ese paquete llegue íntegro; sin verificar hash ni firma, nada lo garantiza. Crisol sí descargará binarios de terceros (Fase 3), lo que amplía la superficie de ataque.

## Decisión propuesta

- Todo binario descargado se verifica contra el **SHA-256 fijado en `catalog.toml`** antes de extraerlo; si no coincide, se descarta y se informa.
- El catálogo actualizable se distribuye con **firma** (p. ej. minisign o equivalente) verificada con una clave pública incluida en el binario. Un catálogo sin firma válida no se usa.
- La actualización de Crisol, si existe, descarga un paquete firmado y lo reemplaza de forma atómica con reversión; no ejecuta scripts de PowerShell.
- La extracción rechaza rutas que salgan de la carpeta destino (zip-slip).
- Sin comprobaciones periódicas: solo bajo demanda o como mucho una vez al día si el usuario lo activa.

## Consecuencias

- Hay que mantener una clave de firma y un procedimiento de publicación del catálogo.
- F3.1 y F3.3 incluyen firma y pruebas de rechazo (hash incorrecto, ZIP malicioso).
