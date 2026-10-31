# CLAUDE.md — Instrucciones para agentes

Este archivo lo lee Claude Code al iniciar. Síguelo en cada sesión.

## Qué es Crisol

Suite de desarrollo web local (PHP + Node) en Rust. El backend (núcleo, daemon y CLI) está completo y se instala con winget. Ahora se construye la **GUI** (Fase 13, ADR-0026): una carcasa de bandeja y ventana que habla con el daemon y con `crisol-client`, sin lógica propia.

## Antes de escribir código, en cada sesión

1. Lee `claude/ESTADO.md`: fase actual, tarea en curso y bloqueos.
2. Lee la sección de esa fase en `docs/plan.md`.
3. Si la tarea toca arquitectura, lee `docs/arquitectura.md` y las ADR relevantes en `docs/decisiones/`.
4. Trabaja **una tarea a la vez**, en el orden del plan.

## Al terminar cada sesión

1. Actualiza `claude/ESTADO.md` (tarea completada, siguiente tarea, bloqueos).
2. Crea una bitácora en `claude/sesiones/AAAA-MM-DD-NN.md` con: qué se hizo, archivos tocados, decisiones tomadas, pendientes.
3. Si tomaste una decisión de arquitectura, regístrala como ADR nueva en `docs/decisiones/`.
4. Haz commit con un mensaje convencional (`feat(core): ...`, `fix(platform): ...`).

## Reglas duras

- **No modifiques el PATH, el registro ni el archivo hosts del sistema** en pruebas. Usa directorios temporales y la variable `CRISOL_HOME` apuntando a una carpeta de prueba.
- **No pidas privilegios de administrador** salvo en las tareas que el plan marca explícitamente como "requiere admin", y siempre como operación aislada y opcional.
- **Nada de lógica en el CLI.** El CLI solo parsea argumentos y llama al núcleo (directo o por IPC al daemon). Si escribes lógica en `crisol-cli`, muévela a `crisol-core`.
- **Nada de lógica en la GUI.** La GUI dibuja y traduce clics en llamadas. Lo que decide algo va en `crisol-client` (conexión, vista en vivo, qué acciones se ofrecen) o en `crisol-core`, siempre con pruebas.
- **Nada específico de Windows en `crisol-core`.** Va en `crisol-platform`, detrás de un trait.
- **Nunca reescribas la config del usuario sin un comando explícito.** Para editar TOML del usuario usa `toml_edit`, que conserva comentarios y orden.
- **No agregues dependencias sin justificarlas** en la bitácora de la sesión. Prefiere crates mantenidos y populares. Verifica la versión actual en crates.io antes de agregarla.
- Cada tarea termina con pruebas que pasan (`cargo test --workspace`), `cargo clippy --workspace --all-targets -- -D warnings` limpio y `cargo fmt` (todo junto: `bash src/scripts/check.sh`).
- Si el plan es ambiguo o choca con la realidad (una API no existe, un binario no se distribuye así), **detente y regístralo como bloqueo en `ESTADO.md`** en lugar de improvisar un cambio de diseño grande.

## Convenciones

- Idioma: código, identificadores y comentarios de código en inglés; documentación en `docs/`, `claude/` y `analisis/` en español neutro (con "tú", sin voseo).
- Errores: `thiserror` en librerías, `anyhow` solo en binarios.
- Async: `tokio`.
- Logs: `tracing`.
- Rutas: nunca concatenes strings; usa `PathBuf` y la capa de rutas del núcleo (`CrisolPaths`).

## Entorno del desarrollador

Windows, VS Code, Git Bash. Ya tiene Herd y Devaron instalados: **no los toques** y elige puertos de prueba que no choquen (ver `docs/arquitectura.md`, sección de puertos).
