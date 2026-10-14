# ADR-0010 — Qué `.crisol.toml` se obedece al buscar hacia arriba

- **Estado:** aceptada (decidida por el usuario el 2026-10-14, a raíz de la revisión externa, hallazgo 19)
- **Fecha:** 2026-10-14

## Contexto

Para elegir la versión de PHP o Node, los shims (F4.2) buscan un `.crisol.toml` desde el directorio actual hacia arriba (`docs/arquitectura.md`, F1.4). Si no hay techo, la búsqueda llega a la raíz del disco. En este equipo, `D:\` da "modificar" a todos los usuarios autenticados (comprobado con `icacls`, igual que `C:\` en el ADR-0008). Otra cuenta puede dejar un `D:\.crisol.toml` que fije la versión de todos los proyectos que no tengan uno propio. Además, `.crisol.toml` llega en repositorios clonados.

Se consideraron tres opciones: comprobar el dueño y poner un tope en `.git`; solo comprobar el dueño (cierra el ataque, pero un archivo propio olvidado en `D:\` sigue aplicándose a todo); y solo el tope en `.git` (un proyecto sin `.git` sigue subiendo hasta la raíz).

## Decisión

- **Solo se obedece un `.crisol.toml` cuyo dueño sea el usuario actual, `SYSTEM` o los administradores**, igual que `safe.directory` en git. Un archivo de otro dueño se **ignora con una advertencia** que nombra la ruta y el dueño, y la búsqueda **continúa** hacia arriba como si no existiera.
- **La búsqueda se detiene en el primer directorio que contenga `.git`** (carpeta o archivo, para cubrir los worktrees y submódulos). El `.crisol.toml` de ese directorio sí se considera; no se sube más allá.
- Se mantiene el `ceiling` explícito de `find_project_file` (comparación sin distinguir mayúsculas en Windows).

## Consecuencias

- La comprobación del dueño es específica de Windows: va en `crisol-platform`, detrás de un trait del núcleo. Debe reutilizar el criterio de "confiable" de `PrivateDirs` (usuario, `SY`, `BA`) y no duplicarlo. El shim debe seguir siendo rápido: se lee solo el dueño (`GetNamedSecurityInfoW` con `OWNER_SECURITY_INFORMATION`), y solo de los archivos que existen.
- Un `.crisol.toml` copiado desde otra cuenta o desde un disco externo puede quedar con otro dueño y ser ignorado. La advertencia debe decir cómo arreglarlo (`takeown` o volver a crearlo), y `doctor` (F11.1) puede mostrarlo.
- `crisol use php X --local` (F4.5) crea el archivo como el usuario, así que siempre será confiable.
- Un proyecto anidado en otro repositorio se detiene en su propio `.git` y no hereda la configuración del repositorio de fuera. Es el comportamiento deseado.
- F1.4 (`find_project_file`) y F4.2 deben implementarlo, con pruebas del tope en `.git` (carpeta y archivo) y con un doble del trait para el caso "otro dueño".
