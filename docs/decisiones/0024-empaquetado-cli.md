# ADR-0024 — Empaquetado: Crisol completo sin GUI

- **Estado:** propuesta (2026-10-30), a la espera del propietario.
- **Fecha:** 2026-10-30

## Contexto

El backend está completo (fases 0 a 12), pero Crisol solo se usa compilándolo. El propietario quiere que **Crisol funcione y se instale con o sin GUI**: el daemon y el CLI son el producto, y la GUI será una capa ligera encima que se añade sin cambiar lo demás.

Lo que ya existe y condiciona el diseño:

- **Cuatro programas** que se buscan entre sí en la misma carpeta: `crisol.exe`, `crisold.exe`, `crisol-shim.exe` y `crisol-elevate.exe` (`main.rs` usa `current_exe().parent()`). Llevan el runtime de C dentro (ADR-0023), y el catálogo va incrustado.
- **`require_trusted_program`** (revisión de la Fase 8, hallazgo 3): `crisold.exe` y `crisol-elevate.exe` no se registran ni se elevan si otra cuenta puede escribir en el archivo o en su carpeta.
- **Una sola entrada en el PATH** (ADR-0002): `CRISOL_HOME\shims`, con `php`, `node`, `composer`… Hoy **`crisol` mismo no está en el PATH**: no es un shim, y una copia en `shims\` no encontraría a sus hermanos.
- **`uninstall --all`** (ADR-0022) ya quita todo lo que Crisol cambia y borra los programas. Su patrón es «plan → acuerdo → ejecutar» (`teardown::run_if_agreed`).
- **La home** (`C:\Crisol`) es de datos y es privada (ADR-0008). Los programas no van en ella: la desinstalación segura borra los programas y conserva los datos.
- **Firma de código** (P.1 del plan): necesita un certificado que todavía no hay.

## Decisión propuesta

### 1. Lo que se publica

- **Un ZIP por versión** para Windows x64: `crisol-<versión>-windows-x64.zip`.
  - Dentro: los cuatro programas, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-NOTICES.md` y un `LEEME.txt` corto.
- Junto al ZIP: `SHA256SUMS` y su **firma minisign**, con la misma herramienta con que Crisol verifica lo que descarga (ADR-0005). Quien desconfíe puede comprobarlo antes de abrirlo.
- Lo construye un flujo de GitHub Actions al crear una etiqueta `v*`, con `cargo build --release` en `windows-latest`. Antes de publicar, el mismo flujo corre `check.sh`.
- **Sin instalador de terceros** (MSI, Inno Setup…): el ZIP más `crisol setup` (punto 2) cubren lo mismo con código que ya tenemos, y la GUI podrá usar la misma instalación.

### 2. `crisol setup`: instalar desde el ZIP

El usuario descomprime el ZIP en cualquier sitio y ejecuta `crisol setup`. Es el simétrico de `uninstall --all`, con el mismo patrón y la lógica en `crisol-core` (una GUI o su instalador llamarían a lo mismo):

1. **Plan** (solo lee): qué se va a copiar y a dónde, qué entradas del PATH se añaden, y qué pasos opcionales hay.
2. **Acuerdo:** se muestra el plan y se pide un sí (`--yes` no pregunta).
3. **Ejecución**, paso por paso, con el resultado de cada uno:
   - copiar los cuatro programas a **`%LOCALAPPDATA%\Programs\Crisol\`**;
   - añadir esa carpeta al PATH del usuario (punto 3);
   - `crisol shims install` y `crisol path add` (los shims);
   - **opcionales, cada uno con su pregunta:** `crisol trust` (Windows pide confirmación), `crisol system install` (un UAC) y `crisol autostart on`.

**¿Por qué `%LOCALAPPDATA%\Programs\Crisol\`?**

- Está dentro del perfil del usuario: **solo él puede escribir ahí**, así que `require_trusted_program` lo acepta sin tocar permisos.
  - Una carpeta nueva en la raíz de `C:\` hereda permisos de escritura para «Usuarios autenticados», y Crisol se negaría a registrar el daemon desde ella.
  - `Program Files` necesitaría un administrador para instalar y para actualizar.
- Es donde Windows espera los programas instalados por usuario (VS Code, Warp…), y no se mezcla con la home de datos.

**Actualizar** es ejecutar `crisol setup` del ZIP nuevo. Para el daemon si corre, reemplaza los programas, y lo vuelve a arrancar si estaba corriendo. Si el ayudante instalado es de otra versión (`HELPER_VERSION`), lo dice y ofrece `crisol system install`.

### 3. El PATH: una entrada fija más

Se añade **una segunda entrada fija** al PATH del usuario: la carpeta de los programas.

- La ADR-0002 quería evitar las entradas **por versión** que se acumulan y se pisan. Esta no cambia nunca y es la de un programa de línea de comandos cualquiera.
- `uninstall --all` la quita junto con la de los shims.
- Se corrige el texto de la ADR-0002 para decirlo.

**Alternativa descartada:** un shim `crisol` dentro de `shims\` que reenvíe a los programas. Mantendría una sola entrada, pero el shim tendría que saber dónde están los programas (un archivo más en la home que puede quedar viejo), y añade un salto en cada orden.

### 4. Gestores de paquetes

- **Scoop**, primero: maneja ZIP de forma nativa y es lo que suele usar quien prefiere la terminal. El manifiesto descarga el ZIP, comprueba su SHA-256 y expone `crisol`.
- **winget**, después: admite ZIP con programas «portables».
- **Con un gestor, los programas son suyos.** El gestor los instala y los borra; `crisol setup --managed` hace solo los pasos del usuario (shims, PATH de los shims y opcionales), y `uninstall --all` no borra los programas.
  - El modo se recuerda en un archivo junto a los programas (`crisol-managed`), que crea el manifiesto.

### 5. Firma

- **Las primeras versiones salen sin firmar.** Windows mostrará SmartScreen («Windows protegió tu PC», editor desconocido) la primera vez, y el UAC de `system install` dirá «editor desconocido». La guía de instalación lo explica y cómo comprobar el SHA-256 y la firma minisign.
- Con un certificado, P.1 tal como está en el plan: Authenticode en los cuatro programas, y comprobar la firma de `crisol-elevate.exe` antes del UAC.

### 6. La GUI, después

La GUI será un quinto programa (`crisol-gui.exe`) en la misma carpeta. Su ZIP o instalador será el del CLI más ese archivo, y su primer arranque llamará al mismo plan de `setup`. Quien no la quiera, no la descarga.

## Tareas propuestas (para `docs/plan.md` cuando se acepte)

- **P.2** Flujo de publicación: etiqueta `v*` → `check.sh` → `cargo build --release` → ZIP, `SHA256SUMS` y su firma minisign como borrador de release.
- **P.3** `crisol setup` en `crisol-core` (plan, acuerdo, ejecución) y en el CLI, con la segunda entrada del PATH, la actualización y `--managed`.
  - *Aceptación:* en Windows Sandbox, ZIP → `setup` → usar → `uninstall --all` deja el sistema como al inicio, como en la ADR-0022.
- **P.4** `uninstall --all` y `doctor` conocen la carpeta de programas: la quitan del PATH, respetan `crisol-managed`, y `doctor` avisa si `crisol` del PATH no es el de la instalación.
- **P.5** Manifiesto de Scoop (en un bucket propio) y guía de instalación en `docs/`.
- **P.6** Manifiesto de winget.
- **P.1** (ya en el plan) Firma Authenticode, cuando haya certificado.

## Consecuencias

- Crisol se instala, se actualiza y se quita sin administrador y sin herramientas de terceros. El administrador solo se pide para los nombres `.test` (`system install`), como hasta ahora.
- Dos entradas fijas en el PATH del usuario en lugar de una. Las dos son de Crisol, ninguna cambia con las versiones, y `uninstall --all` quita ambas.
- Sin firma, la primera impresión incluye un aviso de SmartScreen. Es un costo conocido hasta tener un certificado.
- La GUI no necesita un camino de instalación propio.

## Preguntas abiertas

1. ¿Aceptas la segunda entrada fija en el PATH, o prefieres el shim `crisol` dentro de `shims\` (una sola entrada, un salto más)?
2. ¿Publicar sin firma al principio, o esperar a tener certificado?
3. ¿Scoop primero y winget después, o solo uno?
