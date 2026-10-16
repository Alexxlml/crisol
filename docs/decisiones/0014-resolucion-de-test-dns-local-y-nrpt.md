# ADR-0014 — Resolución de `.test`: DNS local con NRPT, `hosts` de respaldo

- **Estado:** aceptada el 2026-10-16 (la verificación manual con administrador sigue pendiente)
- **Fecha:** 2026-10-16

## Contexto

Los sitios necesitan que `*.test` llegue a `127.0.0.1`/`::1`. Windows no lo resuelve solo (medido: ni siquiera `*.localhost` con `getaddrinfo`). Las dos vías, con sus pruebas y riesgos, están en `analisis/dns-windows.md`: un servidor DNS local más una regla NRPT, o un bloque gestionado en `hosts`. Ambas necesitan administrador; difieren en cuándo.

## Decisión

1. **Primaria: DNS local + NRPT.** El daemon responde A=`127.0.0.1` y AAAA=`::1` para el sufijo configurado, escuchando en una IP de loopback dedicada (`127.77.0.53:53`, que se enlaza sin elevar; la NRPT solo acepta IP sin puerto). La regla `Add-DnsClientNrptRule -Namespace ".<tld>" -NameServers 127.77.0.53` se crea **una vez** en la instalación.
2. **Respaldo: bloque gestionado en `hosts`** entre `# BEGIN CRISOL` y `# END CRISOL`, con líneas IPv4 e IPv6 por nombre, copia previa y sin tocar nada fuera del bloque.
3. **La parte con administrador se aísla** en un ejecutable/operación mínima (`crisol-elevate`) que solo sabe: crear/quitar la regla NRPT del sufijo, y añadir/reemplazar/quitar el bloque de `hosts`. Nada de lógica de Crisol dentro; recibe argumentos fijos y valida todo (sufijo en la lista de TLD de uso especial del ADR-0013, IP solo loopback, nombres solo de la lista de sitios). El resto del producto nunca corre elevado.
4. **La lógica no está en la plataforma:** el formato del bloque de `hosts` y la generación de respuestas DNS son puras y viven en `crisol-core`; leer/escribir `hosts`, la NRPT y la elevación van en `crisol-platform` detrás de un trait con doble para pruebas.
5. **Sin permisos por defecto:** hasta que el usuario ejecute el paso de instalación con administrador, no se toca nada del sistema.
6. **Colisiones (ADR-0007):** antes de crear la regla o el bloque se lee `Get-DnsClientNrptRule` y `hosts`; si el sufijo o un nombre ya lo gestiona otra herramienta, se avisa y no se pisa.
7. **Desinstalar** quita la regla NRPT y el bloque, y deja `hosts` y las reglas como estaban; se prueba contra copias, no contra el sistema real.

## Alternativas descartadas

- Solo `hosts`: pide UAC en cada sitio y no cubre subdominios.
- Solo DNS+NRPT: falla con GPO, VPN que fije la regla, o daemon parado.
- `*.localhost`: no resuelve fuera de los navegadores.
- Persistir un proceso elevado: contradice la regla de no pedir administrador salvo operaciones aisladas.

## Consecuencias

- F7.2 implementa 1 y 3 (respondedor en `crisol-core`/`crisol-dns`, regla y elevación en `crisol-platform`); F7.3 implementa 2.
- `crisol doctor` (F11.1) comprueba con `getaddrinfo`, no con `nslookup`, y avisa de reglas/nombres ajenos.
- Queda por verificar a mano (con administrador): que la NRPT con IP de loopback funciona en Chrome, Edge, Firefox y `curl`; el tiempo de espera con el respondedor parado; la limpieza exacta de la regla.

## Notas de implementación (F7.2)

- `crisol-core::dns`: `Zone` + `answer` (respuestas puras: A=`127.0.0.1`, AAAA=`::1`, resto de tipos vacío, fuera de la zona `REFUSED`, nunca reenvía), `DnsServer` (UDP, rechaza direcciones que no sean loopback), `NrptPolicy` + `enable`/`disable`/`status` y `ElevatedOp` (las dos únicas operaciones del helper, con validación). La regla propia se marca con el comentario `Crisol`: una regla de otra herramienta para el mismo sufijo se informa (`DnsError::Foreign`) y no se toca ni se quita.
- `crisol-platform::nrpt`: `PowerShellNrpt` (cmdlets `DnsClient`; la lectura no necesita administrador, medido) y `ElevatedNrpt` (lee directo y cada cambio lanza el helper con UAC por `ShellExecuteExW` y `runas`).
- `crisol-elevate`: ejecutable mínimo, `nrpt-add <sufijo> <ip>` / `nrpt-remove <sufijo>`; códigos de salida 0 hecho, 2 rechazado, 1 falló.
- Sin dependencias nuevas (solo la feature `Win32_UI_Shell` de `windows`). Prueba manual: `crisol-platform/examples/dns_manual.rs`.

## Notas de implementación (F7.3)

- `crisol-core::hosts` (texto puro): `apply_block` / `remove_block` / `block_names`. Respeta los finales de línea del archivo, deja intacto todo lo de fuera del bloque, rechaza un bloque dañado (sin `END`, `END` sin `BEGIN`, dos bloques) en lugar de repararlo, valida los nombres (mismos sufijos de uso especial, solo caracteres de host) y devuelve como `Conflict` los nombres que otra herramienta ya define fuera del bloque, sin tocarlos (ADR-0007). `remove_block` deja el archivo byte por byte como estaba, salvo que un archivo sin salto de línea final conserva el que se añadió.
- `HostsManager` (`names`/`set`/`clear`) con `DirectHosts<F: HostsFile>` para el helper y las pruebas; solo escribe si el contenido cambia.
- `crisol-platform::hosts_file`: `FileHosts` (cualquier ruta; `system()` es `drivers\etc\hosts`), escritura **en el sitio** para conservar los permisos del original, con copia `hosts.crisol-bak` antes del primer cambio, borrada al quitar el bloque; un archivo que no es UTF-8 se rechaza sin tocarlo. `ElevatedHosts` valida y comprueba el archivo **antes** de pedir el UAC y lanza el helper solo si hay un cambio real. `flush_dns` (`ipconfig /flushdns`) lo llama el helper tras cambiar `hosts`.
- `crisol-elevate` suma `hosts-set <nombre>...` y `hosts-clear` (cuatro operaciones en total).

## Corrección (2026-10-16, revisión de la Fase 7)

Decidida por el propietario a partir de `claude/revisiones/2026-10-16-opus-fase7.md`. Implementada el 2026-10-16 (la nota de implementación está al final).

- **Dónde vive el ayudante.** `crisol-elevate.exe` se instala en `%ProgramFiles%\Crisol\`, una carpeta que solo pueden escribir los administradores. Lo deja ahí un instalador que corre una vez con administrador, y **nunca** se ejecuta desde la home ni desde otra carpeta que pueda escribir el usuario.
  - Antes de mostrar el UAC, quien lo llama comprueba que ni el ejecutable ni su carpeta los pueden escribir otras cuentas.
  - El ayudante usa rutas absolutas del directorio del sistema (`GetSystemDirectoryW`) para `powershell.exe`, `ipconfig.exe` y `hosts`, y restringe la carga de DLL a System32.

  Sin esto, el aislamiento del punto 3 de la decisión no protege nada: otra cuenta que pudiera escribir junto al ayudante obtendría administrador.
- **IPv6.** Nginx escucha también en `[::1]`, en los mismos puertos, para que la respuesta `::1` del DNS y de `hosts` llegue a Crisol y no a otro programa que escuche en IPv6.
- **Con el daemon parado,** la regla NRPT se queda instalada y las consultas `.<tld>` esperan el tiempo de espera del DNS. Se acepta: `crisol stop` y `doctor` lo avisan, y el tiempo se mide en la verificación manual.

### Cómo quedó el hallazgo 1 (ayudante de confianza)

- **Ubicación fija.** `HelperLocation::installed()` construye `%ProgramFiles%\Crisol\crisol-elevate.exe` con `SHGetKnownFolderPath(FOLDERID_ProgramFiles)`; ni `ElevatedNrpt` ni `ElevatedHosts` reciben una ruta, y `run_elevated` ya no es pública. No existe instalador todavía (F8.5 y F8.6, marcadas como «requiere admin» en el plan).
- **Comprobación antes del UAC.** `PrivateDirs::is_writable_by_non_administrators`, más estricta que `is_writable_by_others`: solo SYSTEM, Administradores y TrustedInstaller pueden escribir o ser dueños de la carpeta y del ejecutable; el usuario actual **no** cuenta como de confianza. Reutiliza el análisis de SDDL de `private_dir.rs` (el mismo que juzga la home), probado con texto SDDL literal, incluida la forma de un `Program Files\Crisol` por defecto. Si no se cumple, se niega sin mostrar el UAC y el mensaje dice qué hacer.
- **Dentro del ayudante.** Rutas absolutas de `GetSystemDirectoryW` para `powershell.exe`, `ipconfig.exe` y `drivers\etc\hosts` (también en la lectura sin elevar); nunca `%SystemRoot%` ni búsqueda por nombre. `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` al empezar. `build.rs` pide al enlazador MSVC el *manifest* `requireAdministrator` (`/MANIFESTUAC`, sin `mt.exe`) y `/DEPENDENTLOADFLAG:0x800`; `crisol-elevate/tests/hardening.rs` lo comprueba leyendo los bytes del binario construido, sin ejecutarlo. El binario no tiene arnés de pruebas propio (`test = false`): un ejecutable de pruebas con `requireAdministrator` ni siquiera arrancaría.
- **Referencia.** Herd (`analisis/herd/observaciones.md`) separa igual lo privilegiado (Program Files) de los datos (carpeta del usuario), pero con un servicio SYSTEM permanente; Crisol mantiene el ayudante efímero con UAC.

### Cómo quedó el hallazgo 2 (IPv6)

- Nginx escucha en `127.0.0.1` y en `[::1]`, en los mismos puertos: cada `listen` va duplicado (`listen_loopbacks` en `nginx.rs`), incluidos los `default_server` de HTTP y de TLS, el bloque HTTPS de cada sitio y su redirección. Los *snapshots* y una prueba de invariante (cada `listen` de IPv4 tiene su gemelo en IPv6 y no hay ningún otro) lo vigilan.
- La comprobación previa de puertos (`PortProbe::probe`) y la salud (`is_listening`) ya leían las tablas de IPv4 y de IPv6; ahora lo cubren pruebas con un programa en `[::1]` y el mensaje de «puerto ocupado» nombra la dirección además del programa y el pid. La prueba real (a pedido, necesita Nginx y PHP en `CRISOL_TEST_DOWNLOADS`) está en `crisol-platform/tests/web_stack.rs`.
- Si el equipo no tuviera `::1` (IPv6 desactivado por completo), Nginx no arrancaría; se anota como limitación y `doctor` (F11.1) debe decirlo.
