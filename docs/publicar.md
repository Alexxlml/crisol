# Publicar una versión

Lo hace GitHub Actions (`.github/workflows/release.yml`, ADR-0024). Tú solo preparas la etiqueta y revisas el borrador.

## Una sola vez: claves y secretos

1. Genera un par de claves minisign **solo para los releases** (no reutilices la del catálogo: esta quedará en GitHub). Con contraseña: `minisign -G -p crisol-release.pub -s crisol-release.key`; sin ella, agrega `-W`.
2. En el repositorio, *Settings → Secrets and variables → Actions*:
   - Secreto `MINISIGN_SECRET_KEY`: el contenido completo de `crisol-release.key`.
   - Secreto `MINISIGN_PASSWORD`: su contraseña (si la clave no tiene, no lo crees).
   - Variable (no secreto) `MINISIGN_PUBLIC_KEY`: el contenido de `crisol-release.pub`.
3. Guarda la clave privada fuera del repositorio. Publica la clave pública (en el LEEME del release o en la guía de instalación) para que otros puedan comprobar las descargas.

## Cada versión

1. Sube `version` en `[workspace.package]` de `src/Cargo.toml`, haz commit y push a `main`.
2. Crea y sube la etiqueta, que debe coincidir con la versión: `git tag v0.1.0 && git push origin v0.1.0`. Sube la etiqueta **sola**: empujarla junto con una rama en el mismo `push` no disparó el flujo.
3. El flujo corre `check.sh`, compila, arma `crisol-<versión>-windows-x64.zip`, `SHA256SUMS` y `SHA256SUMS.minisig`, y deja un **borrador** de release. Una etiqueta con guion (`v0.2.0-rc.1`) sale marcada como pre-release. Son dos jobs: `build` compila y empaqueta sin secretos y con permiso de solo lectura; `release` no compila nada, comprueba el ZIP contra `SHA256SUMS`, firma con los scripts de su propio checkout y crea el borrador. Así el código de las dependencias, que corre al compilar, nunca está en el mismo job que la clave.
4. Revisa el borrador, descarga el ZIP, comprueba `sha256sum -c SHA256SUMS` y `minisign -V -p crisol-release.pub -m SHA256SUMS`, y entonces publícalo.

## Probar el empaquetado en local

```bash
cd src && cargo build --release
bash scripts/package.sh 0.1.0 /ruta/de/salida
bash scripts/sign-release.sh /ruta/de/salida clave.key clave.pub   # MINISIGN_PASSWORD si la clave la tiene
```

`package.sh` genera `LICENSES-RUST.md` con `cargo-about` 0.9.3 (`cargo install cargo-about --locked --version 0.9.3`); sin él, `CRISOL_LICENSES_FILE=<archivo>` aporta uno ya generado.

Los ZIP salen sin firma Authenticode (ADR-0024, §5).

## winget

Crisol se publica en winget con el identificador `Alexxlml.Crisol` (ADR-0024). Los manifiestos son plantillas en `packaging/winget/`; el hash y la versión salen del `SHA256SUMS` del release.

### Generar y validar los manifiestos

Con el release **publicado** (no borrador):

```bash
gh release download v0.1.0 -p SHA256SUMS -D /tmp/crisol-0.1.0
bash src/scripts/winget-manifest.sh 0.1.0 /tmp/crisol-0.1.0/SHA256SUMS
```

Escribe `dist/winget/0.1.0/` con los tres archivos y corre `winget validate` (debe decir «Validación del manifiesto correcta», sin advertencias).

### Probar la instalación antes del PR (Windows Sandbox)

Es la prueba de aceptación de P.3 y **todavía no se ha hecho**:

1. En el equipo: `winget settings --enable LocalManifestFiles` (administrador, una vez).
2. En un Windows Sandbox con red y winget disponible (si no trae `winget`, instala «App Installer» dentro): copiar `dist/winget/0.1.0/` y correr `winget install --manifest <carpeta>`.
3. `crisol --version`, `crisol setup`, usar un sitio, `crisol uninstall --all`, `winget uninstall Alexxlml.Crisol`.
4. Comparar con las fotos de `src/scripts/sandbox/` (como en la ADR-0022): el sistema debe quedar como al inicio.
5. Apuntar en la ADR-0024 lo que quedó «sin verificar»: si la carpeta del paquete lleva la versión, qué carpeta del ZIP deja winget (¿están `crisold.exe`, `crisol-shim.exe` y `crisol-elevate.exe` junto a `crisol.exe`?) y qué pasa si winget no puede crear el enlace.

### Abrir el PR a `microsoft/winget-pkgs`

Cualquiera puede hacerlo, desde su cuenta de GitHub. Lo hace el propietario:

1. Fork de <https://github.com/microsoft/winget-pkgs>.
2. Copiar los tres archivos a `manifests/a/Alexxlml/Crisol/0.1.0/` (inicial en minúscula, luego editor, paquete y versión).
3. Commit y PR hacia `microsoft/winget-pkgs`. El título que ellos piden: `New package: Alexxlml.Crisol version 0.1.0`.
4. Un bot descarga el ZIP, comprueba el SHA-256, instala en una máquina limpia y pasa antivirus. Si pide cambios, se corrigen las plantillas aquí, no solo en el fork.
5. Para versiones siguientes: mismo flujo con la versión nueva (`Update: Alexxlml.Crisol version 0.2.0`). La herramienta oficial `wingetcreate` también sirve, pero estas plantillas no la necesitan.

Sin firma Authenticode es posible que la revisión pida aclaraciones o que SmartScreen marque los ejecutables nuevos; no es un requisito de winget.
