# Herd en Windows: observaciones de la instalación

- **Fecha:** 2026-10-16
- **Método:** solo observación de la instalación que hay en el equipo del propietario: listados de carpetas, permisos (`icacls`), configuración del servicio (`Win32_Service`), conexiones TCP del proceso y nombres de *pipes*. **No** se ejecutó, modificó ni descompiló nada de Herd (`CLAUDE.md`: no tocar Herd). La instalación no incluye la licencia propia de Herd, solo las de Electron, Chromium, Nginx y nvm; descompilarlo requeriría primero leer sus términos.
- **Motivo:** ver cómo separa Herd lo que corre con privilegios de los datos del usuario, a raíz del hallazgo 1 de la revisión de la Fase 7 (dónde vive `crisol-elevate.exe`).

## Lo observado

| Qué | Dónde | Permisos / cuenta |
|---|---|---|
| Programa (app de Electron: `Herd.exe`, recursos, binarios de Nginx y nvm empaquetados) | `C:\Program Files\Herd\` | Escriben solo SYSTEM y Administradores; Usuarios con lectura y ejecución |
| Servicio privilegiado `HerdHelper` (`HerdHelper.exe`) | `C:\Program Files\Herd\HerdHelper.exe` | Servicio de Windows, cuenta **LocalSystem**, inicio **automático**, en ejecución permanente |
| Datos del usuario: `bin\` (versiones de PHP y demás), `config\`, `Log\`, `.pid\`, `herd.sqlite`, `openssl.cnf` | `C:\Users\<usuario>\.config\herd\` | Heredados del perfil: SYSTEM, Administradores y el usuario. **Privada sin configurar nada** |
| Otros | `%APPDATA%\Herd`, `%LOCALAPPDATA%\herd-updater` | No se revisaron en detalle |
| Proyectos | `C:\Users\<usuario>\Herd` | (según el propietario) |

- `HerdHelper` no tenía conexiones TCP propias y no hay *pipes* con "herd" en el nombre. **No se sabe** cómo le habla la app; averiguarlo requeriría descompilar.

## Comparación con Crisol

- **Misma separación de fondo:** lo que corre con privilegios va en Program Files, protegido contra escritura del usuario, y los datos en una carpeta privada del usuario. Crisol usa `C:\Crisol` con una ACL protegida (ADR-0008) en lugar de una carpeta dentro del perfil; el efecto en permisos es el mismo.
- **Distinta forma de elevar:**
  - **Herd:** un servicio SYSTEM permanente. No pide UAC nunca, pero deja un proceso con el máximo privilegio siempre activo y aceptando órdenes. Su seguridad depende por completo de cómo valida quién le habla y qué le pide; un fallo ahí da SYSTEM a cualquier programa del equipo.
  - **Crisol (ADR-0014):** un ayudante que solo existe durante una operación, con UAC cada vez y sin nada privilegiado escuchando. Con NRPT, el administrador se usa una vez al instalar; solo el respaldo de `hosts` pide UAC por cambio. El ADR-0014 descartó el servicio permanente a propósito.

## Conclusión

Confirma la decisión del propietario sobre el hallazgo 1 de la Fase 7: `crisol-elevate.exe` vive en `%ProgramFiles%\Crisol\`, lo deja ahí un instalador con administrador, y nunca se ejecuta desde la home. No se adopta el servicio permanente de Herd.
