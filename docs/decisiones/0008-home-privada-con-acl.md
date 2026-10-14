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
- La comprobación de exposición lee la DACL como texto SDDL y busca entradas de *permitir* para `Todos`, `Usuarios autenticados` o `Usuarios` con cualquier derecho de escritura; las denegaciones se ignoran a propósito.
