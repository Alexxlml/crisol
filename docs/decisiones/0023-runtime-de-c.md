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
   - **También se niega si el runtime es demasiado viejo** (revisión de la Fase 11, hallazgo 5). La regla de Microsoft es que el runtime sea al menos tan nuevo como las herramientas con que se compiló el programa, y esa versión viene en la cabecera de cada `.exe` (la versión del enlazador). Se compara con la que registró el instalador del runtime (`v14.51.36247.00` → 14.51): «it needs the Microsoft Visual C++ runtime (x64) 14.44 or newer, and 14.29 is installed». No hay tabla de versiones mínimas que mantener.
     - En el equipo del propietario: PHP 8.4.26 y PostgreSQL 18.6 se compilaron con 14.44, MySQL 8.4.11 con 14.29, y el runtime instalado es 14.51.
     - Si las DLL del runtime están en `System32` pero ningún instalador registró versión, no hay con qué comparar: no se niega, y un fallo se explica por su código de salida, como antes.
   - Crisol **no instala** el runtime: da el enlace de Microsoft.
3. **En la GUI** (más adelante): una pantalla de requisitos antes de arrancar.

## Consecuencias

- Los `.exe` de Crisol casi no crecieron (`crisol.exe` mide unos 7 MB).
- En cada `start`, la comprobación lee las cabeceras de los `.exe` de la instalación (unos pocos KB por archivo, también con el runtime instalado) para compararlas con su versión. Solo se salta la lectura cuando hay DLL sin versión registrada.
- La instalación de PHP o de una base no se niega; el aviso llega al arrancar. `doctor` sigue avisando antes.
