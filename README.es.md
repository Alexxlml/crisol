# Crisol

[![Licencia: MIT o Apache-2.0](https://img.shields.io/badge/licencia-MIT%20%7C%20Apache--2.0-blue)](#licencia)
![Plataforma: Windows](https://img.shields.io/badge/plataforma-Windows%2010%20%7C%2011-0078D4)
![Hecho con Rust](https://img.shields.io/badge/hecho%20con-Rust-CE422B)

[English](README.md) | **Español**

Suite de desarrollo web local para PHP y Node, escrita en Rust.

Crisol instala y administra las herramientas de un entorno de desarrollo local —PHP, Nginx, Node, bases de datos y servicios de apoyo— para que **todo funcione completo, sin funciones de pago y sin ensuciar el sistema**.

> **Estado: en desarrollo, interfaz gráfica casi terminada.** El backend está terminado y revisado (fases 0 a 12), y la interfaz gráfica (Fase 13) ya tiene todas sus pantallas: servicios y sitios, versiones, registros, credenciales, diagnóstico con acciones, primer arranque y desinstalación, empaquetada con lo demás y probada en Windows Sandbox. Antes de la primera versión falta traducir los últimos mensajes del núcleo, las verificaciones manuales finales y publicar en winget. Todavía no hay versiones publicadas, y solo funciona en Windows. 1488 pruebas automáticas.

## Progreso

| | Avance | Detalle |
|---|---|---|
| **Backend** | ███████████████████ **100 %** | Las 49 tareas de las fases 0 a 12 del [plan](docs/plan.md), todas revisadas. |
| **Interfaz gráfica** | █████████████████░░ **≈ 90 %** | F13.1 a F13.10b hechas y revisadas. Falta F13.10c (el resto de la CLI en el idioma del usuario) y las verificaciones manuales pendientes. |
| **Primera versión** | ████████████████░░░ **≈ 85 %** | El empaquetado y el flujo de publicación están listos y la instalación con winget pasó en Windows Sandbox. Falta la última tarea de la interfaz, la primera etiqueta y el PR a `winget-pkgs`. La firma de los ejecutables llega después. |

*Actualizado el 2026-11-14.*

## El problema

Las suites de desarrollo local para Windows resuelven bien una parte del trabajo, pero en el uso diario aparecen los mismos tropiezos:

- **Funciones clave detrás de un pago**, como el gestor de bases de datos, que obligan a combinar varias herramientas.
- **Variables de entorno y entradas del PATH que se acumulan** por cada versión instalada, chocan entre herramientas y no se limpian al desinstalar.
- **Configuración que se revierte sola:** un ajuste hecho a mano desaparece la siguiente vez que la aplicación regenera sus archivos.
- **Pasos manuales para lo básico:** habilitar extensiones de PHP, confiar en el certificado HTTPS o editar el archivo `hosts`.
- **Aplicaciones pesadas** que cargan un runtime completo y quedan atadas a un sistema operativo.

## Lo que Crisol propone

1. **El backend es el producto.** Toda la lógica vive en un núcleo sin interfaz. La línea de comandos y la interfaz gráfica son carcasas intercambiables sobre el mismo protocolo.
2. **Cero contaminación del sistema.** Una sola entrada en el PATH (la carpeta de *shims*) y ninguna variable por versión. Desinstalar deja el sistema como estaba.
3. **La configuración del usuario es sagrada.** Lo que tú escribes en `crisol.toml` es la fuente de verdad; Crisol solo regenera sus propios archivos derivados y edita los tuyos únicamente con un comando explícito, conservando comentarios y orden.
4. **Rápido y ligero.** Ejecutables nativos pequeños, arranque inmediato y poca memoria en reposo. El *shim* que resuelve la versión de `php` o `node` añade unos pocos milisegundos.
5. **Seguro por defecto.** Descargas verificadas con SHA-256 y catálogo firmado, carpeta de datos privada, contraseñas generadas fuera de la configuración, sin telemetría y sin privilegios de administrador salvo una operación aislada y opcional.
6. **Multiplataforma por diseño, Windows primero.** Todo lo específico del sistema operativo queda detrás de *traits* en un crate aparte.

## Qué hace hoy

| Área | Estado |
|---|---|
| **Interfaz gráfica** | Un icono en la bandeja y una ventana (Tauri 2, sin Node) encima del daemon: servicios y sitios en vivo, versiones con el progreso de la descarga, registros que se siguen solos, credenciales para el `.env` con restablecimiento de contraseña, diagnóstico con botones para los arreglos que Crisol sabe hacer, un asistente de primer arranque y la pantalla de desinstalación. Cerrar la ventana la oculta en la bandeja. Tema oscuro y claro según Windows. |
| **Versiones** | Catálogo firmado; instala, desinstala y lista versiones de PHP, Nginx, Node, Composer y servicios de datos, con verificación SHA-256 y sin dejar restos ante un fallo. Avisos de vulnerabilidad y de fin de soporte al instalar y al listar. |
| **PHP** | Varias versiones a la vez. `php.ini` generado por versión con las extensiones comunes activas, certificados raíz del sistema para cURL/OpenSSL y tus `overrides/` al final. Pool de `php-cgi` por versión con reinicio de workers. |
| **Versión por proyecto** | El *shim* elige la versión según `.crisol.toml`, `.php-version`, `.nvmrc`, `engines` o `composer.json`, sin tocar el PATH por proyecto. |
| **Sitios** | `park` y `link` sirven carpetas como `nombre.test` con Nginx, cada una con su versión de PHP. Configuración generada desde plantillas, sin parchear archivos. |
| **HTTPS local** | CA propia limitada a dominios de uso especial (`.test`, `.localhost`…), un certificado por sitio e instalación de la CA en el almacén del usuario, sin administrador. |
| **Dominios `.test`** | Respondedor DNS local y una regla del sistema que se instala una sola vez, con el archivo `hosts` como respaldo. |
| **Daemon** | `crisold` supervisa los servicios (Job Objects: sin procesos huérfanos), vigila la configuración y responde por un *named pipe* que solo tu usuario puede abrir, con eventos numerados para los clientes en vivo. Arranque opcional al iniciar sesión. |
| **Bases de datos y servicios** | MySQL, MariaDB, PostgreSQL, MongoDB, Redis/Garnet, Memcached, Meilisearch y Mailpit, con contraseñas generadas y `crisol db credentials` para tu `.env`. |
| **Node** | Node 20, 22, 24 y 26 con *shims* de `node`, `npm`, `npx`, `corepack`, `pnpm` y `yarn`; paquetes globales y caché separados por versión. |
| **Terminal limpia** | `crisol shell` (y un botón en la ventana, en la carpeta del proyecto) abre una terminal con los *shims* delante y sin las variables de otras suites: no hace falta un PHP, un Node ni un npm globales. Funciona con cmd, PowerShell 5.1 y 7, Git Bash y Windows Terminal; la preferida se elige en la configuración. |
| **Diagnóstico** | `crisol doctor` revisa sin cambiar nada: servicios caídos (con el significado de su código de salida), puertos ocupados por otro programa, bases lentas por el antivirus, sitios que el archivo `hosts` manda a otro lado, versiones sin soporte, un daemon de otra versión y el runtime de Visual C++ que necesitan PHP y las bases. |
| **Primer arranque** | `crisol setup` (o el asistente de la ventana) hace una vez lo que winget no puede: los comandos en tu PATH, el acceso del menú Inicio y, preguntando cada uno, la CA, los nombres `.test` y el arranque al iniciar sesión. Solo hace lo que falta. |
| **Certificados** | `crisol trust` hace que Windows confíe en la CA local e imprime su huella antes del diálogo, para compararla; `crisol untrust` la quita. |
| **Idiomas** | Inglés y español, según el idioma de Windows; `CRISOL_LANG` fuerza uno. Las traducciones son archivos JSON, uno por idioma. `--json` y `--help` siguen en inglés. |
| **Salida en la terminal** | Tablas, marcas y color cuando la salida es una terminal; texto plano si se redirige, con `--plain` o con `NO_COLOR`. |
| **Desinstalación limpia** | `crisol uninstall --all` (o la pantalla de desinstalación de la ventana) lista primero todo lo que Crisol cambió en Windows y lo revierte: autoarranque, PATH, acceso del menú Inicio, certificados, regla DNS, bloque de `hosts`, exclusiones de Defender y el ayudante, con un solo aviso de administrador. Conserva tus bases, sus contraseñas, tu configuración y tus proyectos; `--purge` borra también eso. Probado en Windows Sandbox: el sistema queda como estaba. |

## Un vistazo al CLI

```bash
crisol setup                    # primer uso: los comandos en tu PATH, el acceso del menú Inicio y, preguntando cada uno, la CA, los nombres .test y el arranque al iniciar sesión
crisol install php 8.4          # descarga y verifica PHP 8.4
crisol install nginx
crisol park                     # cada carpeta de aquí se sirve como <carpeta>.test
crisol start                    # Nginx y PHP (arranca el daemon si hace falta)
crisol secure tienda            # https://tienda.test
crisol use php 8.3 --local      # este proyecto usa PHP 8.3
crisol enable mysql && crisol start
crisol db credentials mysql     # líneas listas para el .env
crisol shell                    # una terminal con los shims delante y el entorno limpio
crisol doctor                   # revisa la instalación y dice qué corregir
crisol trust                    # Windows confía en la CA local (sin advertencias en el navegador)
crisol uninstall --all          # saca a Crisol de Windows; tus bases y proyectos se quedan
```

Para que `php`, `node` o `composer` usen los *shims* en una sola terminal, sin cambiar nada del sistema: `crisol env | Invoke-Expression` (PowerShell). Para dejarlo fijo: `crisol path add`.

## Instalar

Cuando salga la primera versión, se instalará con winget:

```powershell
winget install Alexxlml.Crisol
crisol setup
```

Después, la ventana se abre desde el menú Inicio. Cómo actualizar, desinstalar o instalar sin winget está en [`docs/instalacion.md`](docs/instalacion.md).

## Arquitectura

```
 crisol (CLI)    crisol-gui     otros clientes
        └─────── crisol-client ──────┘
                 IPC (named pipe)
               ┌───────▼───────┐
               │    crisold    │  daemon: dueño de los servicios
               └───────┬───────┘
               ┌───────▼───────┐
               │  crisol-core  │  lógica pura, sin interfaz ni APIs del SO
               └───────┬───────┘
               ┌───────▼────────┐
               │crisol-platform │  Windows hoy; Linux y macOS después
               └────────────────┘
  crisol-shim: resuelve la versión de php/node/composer por proyecto
```

| Crate | Responsabilidad |
|---|---|
| `crisol-core` | Modelos, configuración, catálogo, generación de configs, orquestación y los mensajes traducibles. Define los *traits* de plataforma y sus dobles para pruebas. |
| `crisol-platform` | Implementación de esos *traits* para Windows: procesos, puertos, permisos, certificados, DNS, PATH, accesos directos, terminales. |
| `crisol-protocol` | Mensajes y eventos del IPC, versionados. Es el contrato que usa cada cliente ([`docs/protocolo.md`](docs/protocolo.md)). |
| `crisol-daemon` | `crisold`: mantiene los servicios vivos y atiende el IPC. |
| `crisol-client` | Lo comparten el CLI y la GUI: la conexión con el daemon, la vista en vivo del estado y qué acciones se ofrecen en cada momento, todo probado sin interfaz. |
| `crisol-cli` | `crisol`: solo interpreta argumentos y llama al núcleo o al daemon. |
| `crisol-gui` | `crisol-gui`: la bandeja y la ventana. Solo dibuja y traduce clics en llamadas. |
| `crisol-shim` | Ejecutable mínimo que se instala como `php`, `node`, `npm`… y lanza la versión correcta. |
| `crisol-elevate` | Ayudante que se instala una vez con administrador para la regla DNS de `.test`; nada más corre con privilegios. |

Más detalle en [`docs/arquitectura.md`](docs/arquitectura.md).

## Compilar

Requisitos: Windows 10/11 y Rust estable 1.88 o posterior. La GUI necesita WebView2, que viene con Windows 11 y con Windows 10 actualizado; no necesita Node ni npm.

```bash
cd src
cargo build --release
bash scripts/check.sh   # formato, clippy y pruebas
```

Los binarios quedan en `src/target/release/`. Por defecto Crisol usa `C:\Crisol` como carpeta de datos; la variable `CRISOL_HOME` la cambia (útil para pruebas).

PHP, MySQL y PostgreSQL necesitan el [runtime de Visual C++ (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe); si falta o es demasiado viejo, `crisol start` lo dice antes de arrancarlos. Los programas de Crisol no lo necesitan.

## Hoja de ruta

| Fase | Contenido | Estado |
|---|---|---|
| 0–1 | Esqueleto, rutas y configuración que nunca se destruye | ✅ |
| 2 | Supervisión de procesos sin huérfanos | ✅ |
| 3 | Catálogo firmado y descargas verificadas | ✅ |
| 4–5 | PHP, *shims* y sitios con Nginx | ✅ |
| 6–7 | HTTPS local y dominios `.test` | ✅ |
| 8 | Daemon, IPC y CLI completa | ✅ |
| 9 | Bases de datos y servicios extra | ✅ |
| 10 | Node | ✅ |
| 11 | `crisol doctor` completo y desinstalación que revierte todo | ✅ |
| 12 | Protocolo documentado para la interfaz gráfica y cliente de ejemplo | ✅ |
| 13 | Interfaz gráfica: todas las pantallas, el empaquetado y la primera parte de las traducciones (✅); el resto de la CLI traducido (⏳) | ⏳ en curso |
| Empaquetado | Flujo de publicación y `crisol setup` (✅); publicación en winget (⏳) | ⏳ en curso |
| Después | Firma de los ejecutables, actualizaciones automáticas, soporte para Linux y macOS | Planeado |

El plan detallado, con sus criterios de aceptación, está en [`docs/plan.md`](docs/plan.md).

## Cómo se desarrolla

Crisol es un proyecto personal desarrollado con agentes de IA bajo dirección humana:

- **Diseño y decisiones:** cada decisión de arquitectura queda registrada como ADR en [`docs/decisiones/`](docs/decisiones/) (30 hasta ahora: shims en lugar de PATH, sin telemetría, secretos fuera de la config, CA restringida, una desinstalación que conserva los datos del usuario, la GUI sin lógica propia, mensajes traducibles, entre otras).
- **Implementación:** por fases y una tarea a la vez, con Claude Code. Cada tarea termina con pruebas, `clippy` sin advertencias y un commit convencional. Las reglas para los agentes están en [`CLAUDE.md`](CLAUDE.md).
- **Revisión:** al cierre de cada fase, una revisión independiente del código y una verificación manual en Windows (o en Windows Sandbox) antes de avanzar.

## Estructura del repositorio

| Carpeta | Contenido |
|---|---|
| `src/` | Código fuente (workspace de Cargo) |
| `docs/` | Arquitectura, plan por fases, protocolo, catálogo, guías de instalación y de publicación, y decisiones (ADR) |
| `analisis/` | Notas de investigación: DNS en Windows, fuentes de binarios, *shims* de Node, prototipos de la GUI y problemas a resolver |
| `assets/` | Identidad visual: símbolo, iconos y *tokens* de diseño |
| `packaging/` | Plantillas del manifiesto de winget |
| `licenses/` | Licencias de los archivos de terceros incluidos (ver `THIRD-PARTY-NOTICES.md`) |

## Apoya a Crisol

Crisol es gratis y de código abierto. Si quieres apoyar su desarrollo y mantenimiento, puedes hacerlo en [GitHub Sponsors](https://github.com/sponsors/Alexxlml) o [Ko-fi](https://ko-fi.com/alexxlml). Darle una estrella al repositorio y compartir ideas o reportar errores también ayuda mucho.

## Licencia

Crisol se distribuye bajo cualquiera de estas dos licencias, a tu elección:

- MIT ([`LICENSE-MIT`](LICENSE-MIT))
- Apache 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))

Salvo que indiques lo contrario, cualquier contribución que envíes para incluirla en Crisol, tal como la define la licencia Apache 2.0, se distribuye bajo las mismas dos licencias, sin términos adicionales.

Lo que proviene de terceros y sus licencias están en [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
