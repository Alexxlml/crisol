# Crisol

[![Licencia: MIT o Apache-2.0](https://img.shields.io/badge/licencia-MIT%20%7C%20Apache--2.0-blue)](#licencia)
![Plataforma: Windows](https://img.shields.io/badge/plataforma-Windows%2010%20%7C%2011-0078D4)
![Hecho con Rust](https://img.shields.io/badge/hecho%20con-Rust-CE422B)

[English](README.md) | **Español**

Suite de desarrollo web local para PHP y Node, escrita en Rust.

Crisol instala y administra las herramientas de un entorno de desarrollo local —PHP, Nginx, Node, bases de datos y servicios de apoyo— para que **todo funcione completo, sin funciones de pago y sin ensuciar el sistema**.

> **Estado: en desarrollo, backend.** Las fases 0 a 10 del backend están completas (más de 1000 pruebas automáticas). Sigue la Fase 11 (diagnóstico y desinstalación limpia) y después la Fase 12 (contrato para la interfaz gráfica). Todavía no hay versiones publicadas ni interfaz gráfica: hoy se usa desde la línea de comandos y solo en Windows.

## El problema

Las suites de desarrollo local para Windows resuelven bien una parte del trabajo, pero en el uso diario aparecen los mismos tropiezos:

- **Funciones clave detrás de un pago**, como el gestor de bases de datos, que obligan a combinar varias herramientas.
- **Variables de entorno y entradas del PATH que se acumulan** por cada versión instalada, chocan entre herramientas y no se limpian al desinstalar.
- **Configuración que se revierte sola:** un ajuste hecho a mano desaparece la siguiente vez que la aplicación regenera sus archivos.
- **Pasos manuales para lo básico:** habilitar extensiones de PHP, confiar en el certificado HTTPS o editar el archivo `hosts`.
- **Aplicaciones pesadas** que cargan un runtime completo y quedan atadas a un sistema operativo.

## Lo que Crisol propone

1. **El backend es el producto.** Toda la lógica vive en un núcleo sin interfaz. La línea de comandos de hoy y la interfaz gráfica de mañana son carcasas intercambiables sobre el mismo protocolo.
2. **Cero contaminación del sistema.** Una sola entrada en el PATH (la carpeta de *shims*) y ninguna variable por versión. Desinstalar deja el sistema como estaba.
3. **La configuración del usuario es sagrada.** Lo que tú escribes en `crisol.toml` es la fuente de verdad; Crisol solo regenera sus propios archivos derivados y edita los tuyos únicamente con un comando explícito, conservando comentarios y orden.
4. **Rápido y ligero.** Ejecutables nativos pequeños, arranque inmediato y poca memoria en reposo. El *shim* que resuelve la versión de `php` o `node` añade unos pocos milisegundos.
5. **Seguro por defecto.** Descargas verificadas con SHA-256 y catálogo firmado, carpeta de datos privada, contraseñas generadas fuera de la configuración, sin telemetría y sin privilegios de administrador salvo una operación aislada y opcional.
6. **Multiplataforma por diseño, Windows primero.** Todo lo específico del sistema operativo queda detrás de *traits* en un crate aparte.

## Qué hace hoy

| Área | Estado |
|---|---|
| **Versiones** | Catálogo firmado; instala, desinstala y lista versiones de PHP, Nginx, Node, Composer y servicios de datos, con verificación SHA-256 y sin dejar restos ante un fallo. |
| **PHP** | Varias versiones a la vez. `php.ini` generado por versión con las extensiones comunes activas, certificados raíz del sistema para cURL/OpenSSL y tus `overrides/` al final. Pool de `php-cgi` por versión con reinicio de workers. |
| **Versión por proyecto** | El *shim* elige la versión según `.crisol.toml`, `.php-version`, `.nvmrc`, `engines` o `composer.json`, sin tocar el PATH por proyecto. |
| **Sitios** | `park` y `link` sirven carpetas como `nombre.test` con Nginx, cada una con su versión de PHP. Configuración generada desde plantillas, sin parchear archivos. |
| **HTTPS local** | CA propia limitada a dominios de uso especial (`.test`, `.localhost`…), un certificado por sitio e instalación de la CA en el almacén del usuario, sin administrador. |
| **Dominios `.test`** | Respondedor DNS local y una regla del sistema que se instala una sola vez, con el archivo `hosts` como respaldo. |
| **Daemon** | `crisold` supervisa los servicios (Job Objects: sin procesos huérfanos), vigila la configuración y responde por un *named pipe* que solo tu usuario puede abrir. Arranque opcional al iniciar sesión. |
| **Bases de datos y servicios** | MySQL, MariaDB, PostgreSQL, MongoDB, Redis/Garnet, Memcached, Meilisearch y Mailpit, con contraseñas generadas y `crisol db credentials` para tu `.env`. |
| **Node** | Node 20, 22 y 24 con *shims* de `node`, `npm`, `npx`, `corepack`, `pnpm` y `yarn`; paquetes globales y caché separados por versión. |

## Un vistazo al CLI

```bash
crisol install php 8.4          # descarga y verifica PHP 8.4
crisol install nginx
crisol park                     # cada carpeta de aquí se sirve como <carpeta>.test
crisol start                    # Nginx y PHP (arranca el daemon si hace falta)
crisol secure tienda            # https://tienda.test
crisol use php 8.3 --local      # este proyecto usa PHP 8.3
crisol enable mysql && crisol start
crisol db credentials mysql     # líneas listas para el .env
crisol doctor                   # revisa la instalación y dice qué corregir
```

Para que `php`, `node` o `composer` usen los *shims* en una sola terminal, sin cambiar nada del sistema: `crisol env | Invoke-Expression` (PowerShell). Para dejarlo fijo: `crisol path add`.

## Arquitectura

```
 crisol (CLI)     GUI futura     otros clientes
        └──────────────┼──────────────┘
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
| `crisol-core` | Modelos, configuración, catálogo, generación de configs, orquestación. Define los *traits* de plataforma y sus dobles para pruebas. |
| `crisol-platform` | Implementación de esos *traits* para Windows: procesos, puertos, permisos, certificados, DNS, PATH. |
| `crisol-protocol` | Mensajes del IPC, versionados. Es el contrato que usará la interfaz gráfica. |
| `crisol-daemon` | `crisold`: mantiene los servicios vivos y atiende el IPC. |
| `crisol-cli` | `crisol`: solo interpreta argumentos y llama al núcleo o al daemon. |
| `crisol-shim` | Ejecutable mínimo que se instala como `php`, `node`, `npm`… y lanza la versión correcta. |
| `crisol-elevate` | Ayudante que se instala una vez con administrador para la regla DNS de `.test`; nada más corre con privilegios. |

Más detalle en [`docs/arquitectura.md`](docs/arquitectura.md).

## Compilar

Requisitos: Windows 10/11 y Rust estable 1.88 o posterior.

```bash
cd src
cargo build --release
bash scripts/check.sh   # formato, clippy y pruebas
```

Los binarios quedan en `src/target/release/`. Por defecto Crisol usa `C:\Crisol` como carpeta de datos; la variable `CRISOL_HOME` la cambia (útil para pruebas).

> Crisol todavía no está listo para el uso diario: no tiene instalador ni versiones publicadas, y la desinstalación completa llega en la Fase 11.

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
| 11 | `crisol doctor` completo y desinstalación que revierte todo | ⏳ siguiente |
| 12 | Protocolo documentado para la interfaz gráfica | Pendiente |
| Después | Interfaz gráfica, soporte para Linux y macOS, instalador firmado | Planeado |

El plan detallado, con sus criterios de aceptación, está en [`docs/plan.md`](docs/plan.md).

## Cómo se desarrolla

Crisol es un proyecto personal desarrollado con agentes de IA bajo dirección humana:

- **Diseño y decisiones:** cada decisión de arquitectura queda registrada como ADR en [`docs/decisiones/`](docs/decisiones/) (18 hasta ahora: shims en lugar de PATH, sin telemetría, secretos fuera de la config, CA restringida, entre otras).
- **Implementación:** por fases y una tarea a la vez, con Claude Code. Cada tarea termina con pruebas, `clippy` sin advertencias y un commit convencional. Las reglas para los agentes están en [`CLAUDE.md`](CLAUDE.md).
- **Revisión:** al cierre de cada fase, una revisión independiente del código y una verificación manual en Windows antes de avanzar.

## Estructura del repositorio

| Carpeta | Contenido |
|---|---|
| `src/` | Código fuente (workspace de Cargo) |
| `docs/` | Arquitectura, plan por fases, catálogo y decisiones (ADR) |
| `analisis/` | Notas de investigación: DNS en Windows, fuentes de binarios, *shims* de Node y problemas a resolver |
| `licenses/` | Licencias de los archivos de terceros incluidos (ver `THIRD-PARTY-NOTICES.md`) |

## Apoya a Crisol

Crisol es gratis y de código abierto. Si quieres apoyar su desarrollo y mantenimiento, puedes hacerlo en [GitHub Sponsors](https://github.com/sponsors/Alexxlml) o [Ko-fi](https://ko-fi.com/alexxlml). Darle una estrella al repositorio y compartir ideas o reportar errores también ayuda mucho.

## Licencia

Crisol se distribuye bajo cualquiera de estas dos licencias, a tu elección:

- MIT ([`LICENSE-MIT`](LICENSE-MIT))
- Apache 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))

Salvo que indiques lo contrario, cualquier contribución que envíes para incluirla en Crisol, tal como la define la licencia Apache 2.0, se distribuye bajo las mismas dos licencias, sin términos adicionales.

Lo que proviene de terceros y sus licencias están en [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
