# ADR-0023 — Crisol no depende del runtime de Visual C++; PHP y las bases sí

- **Estado:** aceptada por el propietario (2026-10-29).
- **Fecha:** 2026-10-29

## Contexto

En Windows Sandbox (ADR-0022), los programas de Crisol no arrancaban sin el Visual C++ Redistributable: salían con `0xC0000135` y no imprimían nada. En un equipo sin el runtime, Crisol no podía decir qué faltaba, ni desinstalarse.

Además, PHP, MySQL y PostgreSQL importan el runtime (`vcruntime140.dll`; MySQL también `msvcp140.dll`), y Nginx, Node, Mailpit y Redis no. Se comprobó leyendo la tabla de importaciones de los binarios instalados. Sin el runtime, esos servicios fallan solo con un código de salida, que `doctor` explica después (F11.1).

## Decisión

1. **Los cuatro programas de Crisol llevan el runtime de C dentro** (`+crt-static`, en `src/.cargo/config.toml`, también para las pruebas, para probar lo mismo que se publica).
   - Así Crisol arranca en cualquier Windows, y `doctor` y la desinstalación funcionan siempre.
   - `crisol-elevate.exe` carga menos DLL.
   - Las correcciones del runtime llegan con cada versión de Crisol, no con Windows Update.
2. **El runtime de Visual C++ es un requisito antes de arrancar lo que lo necesita.**
   - `start` lee la tabla de importaciones de los `.exe` de la instalación (su carpeta y su `bin`) y, si alguno importa el runtime y Windows no lo tiene, **se niega** con `not_available`: «PHP 8.4.26 cannot start: it needs the Microsoft Visual C++ runtime (x64), which is not installed», con el enlace en `fix`.
   - Lo hace `SystemChecks::missing_runtime`; en Windows, `crisol_platform::pe_imports` y `diagnose::missing_runtime`. Solo se leen las cabeceras, no el archivo entero.
   - Se aplica en `start` (PHP de los sitios y cada base). No hay lista fija de servicios: lo dice el programa mismo, también en versiones futuras.
   - Crisol **no instala** el runtime: da el enlace de Microsoft.
3. **En la GUI** (más adelante): una pantalla de requisitos antes de arrancar.

## Consecuencias

- Los `.exe` de Crisol casi no crecieron (`crisol.exe` mide unos 7 MB).
- Con el runtime instalado, la comprobación no lee ningún archivo: primero mira si el runtime está.
- La instalación de PHP o de una base no se niega; el aviso llega al arrancar. `doctor` sigue avisando antes.
