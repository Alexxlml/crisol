# ADR-0008 — La home de Crisol es privada del usuario (ACL al crearla)

- **Estado:** aceptada (decidida por el usuario el 2026-10-14, a raíz de la revisión externa)
- **Fecha:** 2026-10-14

## Contexto

La arquitectura fija `C:\Crisol` como home por defecto. La raíz `C:\` concede **modificar** a `NT AUTHORITY\Authenticated Users` con herencia (`(OI)(CI)(IO)(M)`; comprobado con `icacls C:\`), y `D:\` también. Una carpeta creada en la raíz hereda ese permiso, de modo que **cualquier otra cuenta del equipo** podría reemplazar `bin\php\…\php.exe`, dejar una DLL junto a él o, más adelante, cambiar `shims\php.exe`, que estará en el PATH del usuario. Eso equivale a ejecutar código como el usuario de Crisol y anula la verificación de hashes de la ADR-0005 (el archivo descargado se puede sustituir entre la verificación y la extracción).

## Decisión

- Se mantiene `C:\Crisol` (ruta corta y portable) y **se protege al crearla**: `crisol-platform` la crea con una DACL **protegida** (`D:P`, sin herencia del padre) que da control total solo al **usuario actual** (por su SID real, no por "owner rights"), a `SYSTEM` y a los administradores locales, con herencia hacia lo que se cree dentro.
- El núcleo no sabe de ACL: define el trait `PrivateDirs` (`crisol-core::paths`) y `CrisolPaths::ensure_private_layout`, que crea la home por ese trait y completa la estructura. `crisol-platform::WindowsPrivateDirs` lo implementa.
- Una home que **ya existe** no se vuelve a permisionar (podría ser la carpeta de otra persona o tener datos): `ensure_private_layout` devuelve `HomeSecurity::WritableByOthers` y quien llama avisa. `crisol doctor` (F11.1) mostrará ese aviso con la solución sugerida.
- Todo `CRISOL_HOME` que el usuario elija pasa por el mismo camino.

## Consecuencias

- El daemon y la CLI (Fase 8) deben llamar a `ensure_private_layout` con `WindowsPrivateDirs` en lugar de `ensure_layout`. `ensure_layout` queda para pruebas y para Unix, donde la home en `~/.crisol` ya es privada por la `umask` habitual (una implementación Unix de `PrivateDirs` con modo `0700` queda pendiente).
- Una home creada por un proceso elevado sigue siendo accesible para el mismo usuario sin elevar, porque la ACL nombra su SID.
- No se añaden crates: se usan más *features* de `windows` (`Win32_Security_Authorization`, `Win32_Storage_FileSystem`).
- La comprobación de exposición lee el dueño y la DACL como texto SDDL. Una carpeta está **expuesta** si su dueño no es el usuario, `SYSTEM` o los administradores, o si cualquier entrada de *permitir* con algún derecho de escritura (crear archivos o carpetas, borrar hijos, cambiar permisos…) nombra a otra cuenta que no sea esas. Es una lista de quién es de confianza, no de grupos a temer; lo que no se entiende cuenta como expuesto, y las denegaciones se ignoran a propósito. (Corregido tras la segunda ronda de la revisión: la primera versión solo miraba tres grupos amplios.)

## Cambio de calendario (2026-10-16, revisión de la Fase 6)

La Fase 6 introdujo una clave que vale la pena proteger: la de la CA local (`certs/ca.key`, ADR-0013), en la que confían el navegador, PHP y Node. Por decisión del propietario, la protección **no espera a la Fase 8**:

- `certs/` se crea con `PrivateDirs::create_private_dir` desde ya. Si Crisol crea la home, también la crea privada.
- Antes de usar la CA para cualquier confianza, se comprueba con `is_writable_by_others` que ni `certs/` ni sus archivos están abiertos a otras cuentas. Si lo están, Crisol se niega y explica cómo arreglarlo.

**Hecho el 2026-10-16** para `certs/` (`ca::ensure_ca`/`create_ca` la crean privada y `load_ca` se niega si está abierta). El resto de este ADR no cambia. Que el daemon y la CLI llamen a `ensure_private_layout` sigue siendo de la Fase 8.

## Corrección (2026-10-16): una home abierta bloquea, no solo avisa

Decidida por el propietario (pregunta de la verificación V1 de la Fase 6). Sustituye a «quien llama avisa» de la decisión original.

- Si la home, o una carpeta suya que Crisol usa para ejecutar o para confiar, la pueden escribir otras cuentas (`HomeSecurity::WritableByOthers` o `is_writable_by_others`), Crisol **se niega** a:
  - arrancar servicios (Nginx, pools de PHP, bases de datos);
  - generar `php.ini` y la configuración de Nginx;
  - instalar o desinstalar versiones;
  - confiar en la CA (esto último ya se hacía desde la revisión de la Fase 6).
- El mensaje nombra la carpeta abierta y da el comando exacto para cerrarla, como el del shim.
- Siguen funcionando las operaciones de solo lectura y diagnóstico: `status`, `list` y `doctor`.
- Crisol nunca cambia por su cuenta los permisos de una carpeta que ya existía.
- Motivo: con la home abierta, otra cuenta puede editar un `php.ini` para cargar una DLL suya, o cambiar binarios en `bin\`, y eso se ejecutaría con el usuario de Crisol. Avisar no basta.
