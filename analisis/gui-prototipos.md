# Prototipos de la GUI: Slint contra Tauri 2 (F13.2)

Fecha: 2026-10-30. Equipo: el del propietario (Windows 11, Rust 1.99, WebView2 156). **No se elige aquí:** decide el propietario y se anota en la ADR-0026.

## Qué se construyó

La misma pantalla en `prototipos/gui-slint/` y `prototipos/gui-tauri/` (fuera del workspace; usan `crisol-client` por ruta):

- la lista de servicios en vivo con `LiveView`, con start/stop por servicio y «Start all / Stop all»;
- el icono de la bandeja con el menú de la ADR-0026 §2 (Iniciar/Detener todo según el estado, Abrir Crisol, Salir, Detener todo y salir);
- cerrar la ventana la oculta; una segunda ejecución muestra la ventana que ya existe.

Verificado contra un `crisold` real, con una home de prueba, Nginx 1.31.6 y PHP 8.4.27 en el puerto 18080 (`prototipos/verificar.ps1`, que maneja la ventana por UI Automation y consulta `crisol status`): la lista y la versión del daemon se ven; «Stop all» para los servicios y el botón pasa a «Start all»; «Start all» los vuelve a arrancar; cerrar oculta sin terminar el proceso; la segunda ejecución termina y la primera vuelve a mostrar su ventana. Los dos registran su icono en la bandeja (`HKCU\Control Panel\NotifyIconSettings`).

**No se comprobó** (ver las preguntas): clic en las entradas del menú de la bandeja, «Salir» y «Detener todo y salir» (el código está, no se pudo automatizar el menú), el aspecto de la ventana y del icono, y la traducción.

## Resultados

Todo con `--release`, mismos servicios activos (Nginx y PHP), `prototipos/medir.ps1`.

| | Slint (GPU, femtovg) | Slint (software) | Tauri 2 |
|---|---|---|---|
| **RAM privada**, árbol de procesos, ventana abierta, 30 s / 5 min | 51,0 / 50,7 MB | 4,2 / 4,0 MB | 81,8 / 79,9 MB |
| **RAM privada**, ventana oculta en la bandeja, 30 s / 5 min | 50,7 / 50,8 MB | 4,0 / 4,0 MB | 79,9 / 79,6 MB |
| Working set total (los compartidos cuentan por proceso), abierta, 30 s | 87,8 MB | 25,9 MB | 370 MB |
| Procesos del árbol | 1 | 1 | 7 (6 de WebView2) |
| **Arranque en frío** hasta la primera ventana, 5 corridas (ms) | 658, 226, 210, 226, 226 (media 309; sin la primera, 222) | 302, 86, 55, 69, 70 (media 116; sin la primera, 70) | 296, 54, 55, 70, 52 (media 105; sin la primera, 58) |
| **Ejecutable** | 17,9 MB | 18,4 MB | 14,9 MB |
| Lo que hace falta además | nada | nada | WebView2 (ver abajo) |
| **Líneas de la pantalla** (sin vacías ni comentarios) | 56 `.slint` + 134 Rust | igual | 52 HTML/JS + 153 Rust |
| Líneas de instancia única | 40 (código propio) | 40 | 0 (plugin oficial) |
| Configuración aparte | 3 (`build.rs`) | 3 | 22 (`tauri.conf.json`, permisos) + 3 |
| Crates (`cargo tree`, con los de Crisol) | 389 | 389 | 335 |
| Build limpio con `crisol-client` | 1 min 14 s (antes de añadir `accessibility` y `system-tray`) | 54 s (build limpio en otro `target`) | 1 min 17 s |
| Carpeta `target` | 1,9 GB | 1,6 GB | 1,5 GB |
| Licencia | GPL-3.0 o *Royalty-free 2.0* (con atribución) o comercial | igual | MIT o Apache-2.0 (`tao`: Apache-2.0) |

### Cómo leer los números

- **Arranque de Tauri:** «primera ventana» es la ventana vacía; el contenido web llega después. Con UI Automation el contenido tardó en aparecer lo suficiente como para que hubiera que esperarlo. No se midió el tiempo hasta el contenido. El arranque de Slint sí incluye el contenido.
- **RAM:** la «privada» es lo que Task Manager llama *Memoria*. Con la ventana oculta no cambia en ninguno de los dos: WebView2 no suspende su proceso y Slint no libera su superficie.
- **El renderizador de Slint** es una decisión de build, y cambia la RAM en un orden de magnitud. No se evaluó el aspecto con el renderizador por software (texto, DPI alto); falta mirarlo a ojo.
- La primera corrida es más lenta en los tres (disco y DLL fríos); por eso se da también la media sin ella. Un arranque en frío de verdad (reinicio) no se midió.

### Lo que cuesta lo que falta

| | Slint | Tauri 2 |
|---|---|---|
| Bandeja | `SystemTrayIcon` incluido, **solo con la característica `system-tray`**. Sin ella compila y no hace nada: el icono no aparece y no hay aviso (nos pasó). La documentación oficial del elemento es la de la versión «next»; en 1.18.1 funciona. | Característica `tray-icon` de `tauri`; API madura. |
| Instancia única | No hay. 40 líneas con un mutex y un evento con nombre (`windows`), que es lo que se haría detrás del trait de `crisol-platform` de F13.3. | `tauri-plugin-single-instance`, oficial; probado. |
| Accesibilidad | Característica `accessibility`; sin ella UI Automation no ve nada. Con ella, ve los textos y los botones. | La da WebView2; el árbol se construye de forma perezosa. |
| Traducción | `@tr("…")` incluido (gettext). Se probó el marcado, no los archivos `.po`. | No incluida: hay que traer una biblioteca de JS o hacerla a mano. No se probó. |
| Estado hacia la pantalla | Directo, con los tipos de Rust. | `LiveState` no es serializable: hay que pasarlo a JSON (12 líneas en el prototipo). |
| Cerrar = ocultar | Es lo que hace Slint por defecto (con `run_event_loop_until_quit`). | `prevent_close()` y `hide()` (4 líneas). |

### Lo que añade al build y a la publicación

- **Slint:** nada que no sea `cargo`. Sin Node ni herramientas aparte.
- **Tauri con esta página** (HTML sin empaquetador): tampoco hace falta Node; `cargo build --release` basta, con la característica `custom-protocol`. **Con una interfaz real** (React, Vite…) hay que añadir Node y npm al flujo de `release.yml` (el ejecutor de GitHub ya los trae) y al build local de quien contribuya, y el build web a `docs/publicar.md`.
- **WebView2 (Tauri):** el ZIP de winget es portable y no puede instalarlo. En Windows 11 y en Windows 10 actualizado ya viene con el sistema (la documentación de Tauri lo dice así). Si falta, `crisol-gui.exe` no abre, así que haría falta una comprobación y un aviso con el enlace. Si se quisiera incluirlo: 1,8 MB el instalador en línea, unos 127 MB el instalador completo y unos 180 MB el runtime fijo, según la documentación de Tauri.

### Licencia (comprobada en las páginas oficiales)

- **Slint** (`slint.dev/terms-and-conditions`, y el campo `license` del crate): tres opciones.
  - **GPLv3.** Crisol es MIT o Apache-2.0: un `crisol-gui.exe` con Slint bajo GPL obligaría a distribuirlo bajo GPL.
  - **Royalty-free 2.0**, gratis para escritorio, móvil y web, con estas condiciones:
    - mostrar el *widget* `AboutSlint` en una pantalla «Acerca de» accesible desde el menú principal, o poner la insignia de Slint en la página pública de descarga;
    - no usarlo en sistemas embebidos;
    - no distribuir una aplicación que exponga las APIs de Slint;
    - no quitar los avisos de licencia.
  - **Comercial**, de pago.
- **Tauri:** `Apache-2.0 OR MIT` en `tauri`, `wry` y `tray-icon`; `tao` es Apache-2.0. Sin condiciones de atribución más allá de las licencias.

## Recomendación

**Slint**, con el renderizador por software si el aspecto lo permite, y con tres condiciones que decide el propietario:

1. **Licencia.** Hay que elegir *Royalty-free 2.0* y cumplir la atribución (pantalla «Acerca de» con `AboutSlint` o insignia en la página de descarga). Si no se quiere ninguna condición ajena a MIT o Apache-2.0, Slint no sirve y la respuesta es Tauri.
2. **Que el aspecto del renderizador por software sea aceptable.** Con GPU son 51 MB; con software son 4 MB. No se miró a ojo.
3. **Que se acepte un componente joven.** El `SystemTrayIcon` es nuevo y su documentación oficial está en «next». Funciona, pero un fallo ahí cuesta más que en la bandeja de Tauri.

**Por qué:**
- la GUI de Crisol es una herramienta que vive en la bandeja toda la sesión, junto a servicios que ya gastan memoria: 4 a 51 MB en un solo proceso contra 80 MB privados (370 MB contando los compartidos) en siete;
- no depende de un runtime que pueda faltar ni de Node;
- sus textos y la traducción van incluidos;
- no hay que serializar el estado.

**Qué cuesta:**
- 40 líneas de instancia única (la parte que Tauri da hecha);
- una licencia con una condición;
- menos libertad de diseño que una página web (tablas, gráficos, editores de texto largo): la v1 de la ADR-0026 §3 no necesita nada de eso, pero la pantalla de logs de F13.6 conviene probarla pronto con un registro grande.

**Cuándo sería Tauri:** si la licencia de Slint no es aceptable, si se quiere una interfaz de web con herramientas de web, o si una máquina sin WebView2 se considera un caso que no existe en los usuarios de Crisol y los 80 MB no importan.

## Hallazgos que valen con cualquiera de los dos

- **`any_running` y el texto de la conexión** están escritos dos veces, una en cada prototipo. Es lógica («¿se ofrece "Iniciar todo" o "Detener todo"?», «¿qué dice el estado?») y va en `crisol-client` (F13.3, como el estado general de la bandeja).
- **`LiveState` debería ser `Serialize`** si se elige Tauri.
- **Mi primera lectura del experimento se equivocó** (y se corrigió): mi script cerraba *todas* las ventanas visibles del proceso, incluidas las ocultas de la bandeja y del plugin de instancia única, y por eso parecía que con la ventana oculta nada funcionaba. Cerrando solo la ventana «Crisol», como hace el botón X, los dos funcionan con la ventana oculta: la bandeja se actualiza y la segunda ejecución despierta la ventana.

## Cómo repetir cada medida

Con `CRISOL_HOME` en una home de prueba con Nginx y PHP instalados y arrancados en puertos libres (`[services.nginx] http_port = 18080`, `https_port = 18443`) y un sitio en `www/`:

```text
# compilar (en cada carpeta de prototipos/)
cargo build --release                                                          # Slint con GPU, y Tauri
cargo build --release --no-default-features --features renderer-software --target-dir target-sw   # Slint por software

# medir (una vez por ejecutable; tarda unos 11 min: 5 arranques, y la RAM a 30 s y 5 min, abierta y oculta)
pwsh prototipos/medir.ps1 -Exe <ejecutable> -Label <nombre> -Out <salida.json>

# comprobar la conducta contra el daemon real
pwsh prototipos/verificar.ps1 -Exe <ejecutable> -Crisol src/target/release/crisol.exe

# líneas
grep -vE '^\s*($|//)' <archivo> | wc -l          # Rust y .slint; en el HTML, solo las vacías
# crates y build limpio
cargo tree -e normal --prefix none | sort -u | wc -l     # y `cargo build --release` con target vacío
# tamaño: el .exe de target/release
```

La RAM se suma sobre el árbol de procesos por `ParentProcessId` (WebView2 incluido) con `Win32_PerfFormattedData_PerfProc_Process.WorkingSetPrivate`. Los contadores de rendimiento por nombre no sirven en un Windows en español. El arranque es el tiempo desde `CreateProcess` hasta que el proceso tiene visible una ventana titulada «Crisol». «Cerrar» es un `WM_CLOSE` a esa ventana.
