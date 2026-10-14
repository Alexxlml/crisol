# ADR-0006 — Secretos fuera de los archivos de configuración

- **Estado:** propuesta (pendiente de aprobación del usuario)
- **Fecha:** 2026-10-13

## Contexto

Guardar contraseñas de bases de datos o tokens de servicios externos en JSON sin cifrar, junto con el resto de la configuración, los expone en cualquier copia del archivo. Los archivos de configuración de Crisol son editables a mano, se pueden versionar y se comparten en capturas y reportes.

## Decisión propuesta

- `crisol.toml` y `sites.toml` **no contienen secretos**. Admiten una referencia (p. ej. `password = { env = "CRISOL_MYSQL_PASSWORD" }` o `{ credential = "mysql-root" }`).
- Los secretos que Crisol genere o reciba se guardan en el **almacén de credenciales del sistema** (Windows Credential Manager) o, si no hay uno disponible, en un archivo en `state/` con permisos restringidos al usuario.
- Por defecto las bases de datos locales usan root sin contraseña **solo en `127.0.0.1`** (como en el plan, F9.1), y el comando que cree usuarios con contraseña no las imprime en el registro.

## Consecuencias

- Un `crisol.toml` puede compartirse sin filtrar credenciales.
- Hay que implementar el trait de almacenamiento de secretos en `crisol-platform` (detrás de la interfaz de core).
