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
3. El flujo corre `check.sh`, compila, arma `crisol-<versión>-windows-x64.zip`, `SHA256SUMS` y `SHA256SUMS.minisig`, y deja un **borrador** de release. Una etiqueta con guion (`v0.2.0-rc.1`) sale marcada como pre-release.
4. Revisa el borrador, descarga el ZIP, comprueba `sha256sum -c SHA256SUMS` y `minisign -V -p crisol-release.pub -m SHA256SUMS`, y entonces publícalo.

## Probar el empaquetado en local

```bash
cd src && cargo build --release
bash scripts/package.sh 0.1.0 /ruta/de/salida
bash scripts/sign-release.sh /ruta/de/salida clave.key clave.pub   # MINISIGN_PASSWORD si la clave la tiene
```

`package.sh` genera `LICENSES-RUST.md` con `cargo-about` 0.9.3 (`cargo install cargo-about --locked --version 0.9.3`); sin él, `CRISOL_LICENSES_FILE=<archivo>` aporta uno ya generado.

Los ZIP salen sin firma Authenticode (ADR-0024, §5).
