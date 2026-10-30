# ADR-0022 — `crisol uninstall --all` deja el sistema sin rastro de Crisol

- **Estado:** aceptada (2026-10-29, F11.2). El propietario pidió que también se quiten las exclusiones de Defender (corrige la sección 3 de la ADR-0021).
- **Fecha:** 2026-10-29

## Contexto

F11.2 pide revertir todo lo que Crisol cambia en el sistema y listarlo antes de hacerlo. Lo que Crisol puede haber cambiado fuera de su carpeta:

| Cambio | Quién lo hace | Necesita administrador |
| --- | --- | --- |
| `shims/` en el PATH del usuario | `crisol path add` | no |
| Entrada `Run` del usuario (autoarranque) | `crisol autostart on` | no |
| CA de Crisol en el almacén `Root` del usuario (la actual y las antiguas) | `crisol trust`, `crisol secure` | no, pero Windows pide confirmar **cada** certificado al añadirlo y al quitarlo |
| Regla NRPT del sufijo | el ayudante (`crisol system install`) | sí |
| Bloque de Crisol en `hosts` | el ayudante | sí |
| `%ProgramFiles%\Crisol\` (el ayudante) | `crisol system install` | sí |
| Exclusiones de Defender | **el usuario a mano**, con el comando que sugiere `doctor` | sí |

La ADR-0021 decidió que Crisol no crea exclusiones de Defender, «así no hay nada de Crisol que revertir en F11.2». El propietario lo corrigió: al desinstalar, el sistema debe quedar limpio de Crisol, también de las exclusiones que el usuario puso siguiendo a `doctor`. El paso elevado ya pide un UAC, así que quitarlas no añade otro.

Hay dos detalles de Windows:

- **Sin administrador no se ven las exclusiones.** `Get-MpPreference` devuelve «N/A: Must be an administrator to view exclusions». Antes del UAC no se puede decir si existen.
- **El ayudante solo devuelve un código de salida.** No escribe en ningún sitio que el lado sin elevar pueda elegir (ADR-0014).

## Decisión

### 1. Qué hace y qué no

`crisol uninstall --all` revierte los cambios de la tabla, en este orden:

1. Para el daemon y sus servicios si está corriendo (por IPC; no arranca uno).
2. Quita la entrada de autoarranque.
3. Quita `shims/` del PATH del usuario (y nada más del PATH).
4. Quita del almacén del usuario **todas** las CA de Crisol: la actual y las antiguas. Windows pregunta por cada una.
5. Con un solo UAC, el ayudante instalado quita la regla NRPT, el bloque de `hosts`, las exclusiones de Defender dentro de la home y, al final, el propio ayudante.

**Sin `--purge` no borra la home** (`CRISOL_HOME`): ahí están los datos de las bases, la configuración y las versiones instaladas. Lo dice al final, con la ruta y con `--purge` como forma de borrarla. `crisol.exe` y sus compañeros no se borran nunca: el usuario los quita a mano.

**Con `--purge`** (pedido por el propietario, para quien ya respaldó):

- Antes de preguntar, lista lo que se pierde: los servicios con datos en `data/` (`mysql`, `postgresql`…), lo que hay en `www/` (proyectos incluidos), las versiones instaladas, la configuración, las contraseñas y los certificados. Aclara que los sitios enlazados o aparcados desde carpetas de fuera no se tocan.
- La pregunta cambia: «¿Ya respaldaste tus proyectos y las bases de datos que necesitas? La carpeta se borra y no se puede recuperar. Escribe yes para seguir».
- **Solo borra lo que Crisol crea** en la home (`crisol.toml`, `sites.toml`, `state`, `bin`, `etc`, `data`, `logs`, `certs`, `secrets`, `cache`, `shims`, `www`) y después la home vacía.
- **Si la home tiene cualquier otra cosa, no borra nada** y la enumera. Un `CRISOL_HOME` que apunte por error a una carpeta del usuario (su perfil, por ejemplo) nunca pierde nada.
- **No se borra si algo anterior se rechazó o falló:** la CA seguiría confiada sin su archivo, o el ayudante seguiría instalado. Se dice «skipped», y la siguiente ejecución lo termina.
- Un archivo que un servicio recién parado todavía tiene abierto se reintenta unas veces.

Cada paso se intenta aunque falle uno anterior. Si el usuario dice que no (a una CA o al UAC), eso es un **resultado**, no un error. El resumen final dice qué se hizo, qué no hacía falta, qué se rechazó y qué falló. El código de salida es 1 si algo se rechazó o falló. Volver a ejecutarlo termina lo que quedó.

### 2. Primero la lista, después la confirmación

- Antes de cambiar nada, se imprime la lista de lo que se va a quitar, con lo concreto de este equipo: la entrada de autoarranque con su comando, la carpeta del PATH, el nombre de cada CA, el sufijo de la regla NRPT y la home donde se buscarán exclusiones.
- Luego pide escribir `yes`. Con `--yes` no pregunta. Con `--json` y sin `--yes`, solo imprime la lista y no cambia nada.
- Si no hay nada que quitar, lo dice y no pregunta.
- La lista y la ejecución están en `crisol-core::teardown`. El CLI solo muestra la lista, pide la confirmación y para el daemon por IPC.

### 3. Defender (corrige la sección 3 de la ADR-0021)

- **Qué se quita:** las entradas de `ExclusionPath` y `ExclusionProcess` que son la home o están dentro de ella (sin distinguir mayúsculas, por componentes de ruta). Nunca otras: las de Herd, las de los proyectos del usuario o las de otras herramientas quedan.
- **Quién lo hace:** el ayudante, dentro de `uninstall-self`, que pasa a recibir la home: `uninstall-self <sufijo> <home>`.
  - La home va en hexadecimal UTF-8, porque los argumentos del UAC no pueden llevar espacios (`C:\Users\Nombre Apellido\…`).
  - El ayudante la valida: ruta absoluta con unidad, que no sea la raíz de una unidad, sin caracteres de control.
  - Los valores llegan a PowerShell entre comillas simples, con las comillas simples duplicadas.
- **Cómo lo cuenta:** con el código de salida, que deja de ser solo de error. `0` hecho sin exclusiones dentro de la home; `20` hecho y se quitaron exclusiones; `21` hecho, pero Defender no respondió (otro antivirus, por ejemplo). En los tres casos se quitó todo lo demás.
- **Si el ayudante no está instalado** no se pide UAC, y la lista da **lo que el usuario tiene que hacer a mano** (acordado con el propietario):
  - la regla NRPT y el bloque de `hosts` se pueden **ver** sin administrador (`SystemSetup::leftovers`). Si siguen ahí (un ayudante borrado a mano, por ejemplo), da el comando de PowerShell que quita la regla con el comentario `Crisol` y la ruta de `hosts` con sus marcas `# BEGIN CRISOL` y `# END CRISOL`;
  - las exclusiones de Defender no se pueden ver: da el comando para quitar la de `<home>\data` y cómo listarlas.
- `crisol system uninstall` hace lo mismo con Defender, porque usa la misma operación.
- Las entradas que usan variables (`%LOCALAPPDATA%\…`) o comodines se comparan tal como están escritas. El comando que sugiere `doctor` usa la ruta literal.

### 4. Quitar una CA puede recibir un «no»

`TrustStore::remove` devuelve `RemoveOutcome { Removed, Absent, Declined }` en vez de `bool`. En Windows, `ERROR_CANCELLED` al borrar es `Declined`. `crisol untrust` lo muestra también: «Windows was told no».

### 5. La prueba de aceptación

El plan pide comparar exportaciones del sistema antes y después, en una VM o en Windows Sandbox. Se usa **Windows Sandbox** (Windows 11 Pro), con los scripts de `src/scripts/sandbox/`:

- `crisol.wsb` abre el Sandbox con tres carpetas compartidas: los scripts y los binarios `release`, las dos de solo lectura, y `D:\scratch\sandbox-out` para las fotos.
- `preparar.ps1` corre solo al abrirse. Copia los binarios a `%USERPROFILE%\crisol-bin`, porque Crisol no ejecuta programas que otras cuentas puedan reemplazar. Añade una exclusión de Defender ajena (`C:\not-crisol`), que debe sobrevivir. Y saca la foto `1-antes`.
- `foto-sistema.ps1` exporta solo lo que Crisol puede tocar: el `Path` y la clave `Run` del usuario, las huellas de `CurrentUser\Root`, el contenido de `hosts`, las reglas NRPT, las exclusiones de Defender y la carpeta del ayudante. `meta.txt` dice si se sacó como administrador.
- `comparar-fotos.ps1` compara dos fotos archivo por archivo.

Se sacan tres fotos: antes de instalar, con todo instalado (para comprobar que la prueba ve los cambios) y después de `uninstall --all --purge`. La primera y la última deben ser idénticas.

## Consecuencias

- `SystemSetup::uninstall` recibe la home. `SetupOp::UninstallSelf` lleva la home, y el ayudante instalado por una versión anterior no sirve (ya lo exigía el manifiesto).
- `SystemSetup::installed` dice si hay ayudante, para la lista.
- `crisol uninstall` acepta `--all` sin servicio ni versión.
- `Context` del CLI gana `confirm`, que lee una línea de la consola. Las pruebas la sustituyen.
- La sección 3 de la ADR-0021 queda corregida por esta: Crisol sigue sin **crear** exclusiones, pero al desinstalar quita las que encuentre dentro de su home.

## Verificación manual (propietario, en Windows Sandbox)

Nada de esto toca tu equipo: todo pasa dentro del Sandbox, que se borra al cerrarlo. Solo las fotos salen a `D:\scratch\sandbox-out`.

**En tu equipo, una vez:**

1. En `src/`: `cargo build --release` (deja los cuatro `.exe` en `src\target\release`). Se compilan con el runtime de C estático (`src/.cargo/config.toml`): el Sandbox no trae el Visual C++ Redistributable, y sin él no arrancan.
2. Crea la carpeta `D:\scratch\sandbox-out` (vacía; si ya tiene fotos de otra prueba, bórralas).
3. Si el repositorio no está en `D:\Documents\CodingMX\Proyectos\Crisol`, corrige las rutas de `src\scripts\sandbox\crisol.wsb`.

**La prueba:** los pasos están en `src\scripts\sandbox\COMANDOS.txt`, que `preparar.ps1` copia al escritorio del Sandbox (dentro de él sí funciona copiar y pegar; hacia el equipo, no). En resumen:

1. Doble clic en `src\scripts\sandbox\crisol.wsb`, que abre el Sandbox sin red ni GPU virtual: con ella se cerraba solo (`0x80370106`), y con red Windows añadía por su cuenta certificados raíz y una entrada de Edge en `Run`. `preparar.ps1` corre sin ventana: copia Crisol, espera a que Defender responda (tarda minutos), añade la exclusión ajena y saca la foto `1-antes`.
2. Cuando exista `C:\sandbox\out\1-antes\meta.txt`, en una terminal **de administrador**: `powershell -ExecutionPolicy Bypass -File C:\sandbox\scripts\prueba.ps1`. Hace todo y guarda cada salida en `C:\sandbox\out` (`D:\scratch\sandbox-out`): instala (`system install`, `path add`, `autostart on`, `trust`, la exclusión `C:\Crisol\data`), saca `2-instalado`, ejecuta `uninstall --all --purge --yes`, vuelve a listar (vacío) y saca `3-despues`.
3. `comparar-fotos.ps1` separa las diferencias que nombran a Crisol de las que Windows hizo solo. La prueba pasa con «Nothing of Crisol differs between 1-antes and 3-despues».

Con la terminal ya elevada no aparece el UAC. Los diálogos del certificado, al añadirlo y al quitarlo, sí aparecen.

4. Opcional, en otro Sandbox nuevo: repite hasta `uninstall --all` y di **no** en el diálogo de la CA o en el UAC. Debe decir `[declined]`, salir con código 1 y, con `--purge`, `[skipped]` sin borrar `C:\Crisol`. Volver a ejecutarlo y aceptar debe terminar.
5. Cierra la ventana del Sandbox: todo lo de dentro desaparece.

**Defender no funciona en el Sandbox** (comprobado el 2026-10-29): `Get-MpPreference` falla con `0x800106ba` (el servicio de Defender no corre), así que `defender.txt` dice `unavailable` en todas las fotos, `Add-MpPreference` falla y el ayudante termina con el código 21 («Defender did not answer»). Eso prueba el camino «no disponible». El de quitar exclusiones de verdad se prueba aparte en el equipo del propietario, con una home de prueba.

**Lo que no se sabe todavía:** si dentro del Sandbox aparece el UAC (su usuario es administrador).

### Resultado (propietario, 2026-10-29)

**Pasó**, en la tercera vuelta: «Nothing of Crisol differs between 1-antes and 3-despues». Las salidas están en `D:\scratch\sandbox-out`.

- **Con todo instalado** (`2-instalado`), la foto registró los 6 cambios de Crisol: `C:\Crisol\shims` en el PATH, la entrada `Crisol` en `Run`, la CA en `CurrentUser\Root`, la regla `.test -> 127.77.0.53 [Crisol]` y `crisol-elevate.exe` con su manifiesto en Program Files.
- **`uninstall --all --purge --yes`** dio `[done]` en los cinco pasos y salió con código 0. Al quitar la CA, Windows mostró su diálogo. Una segunda lista no tuvo pasos.
- **Después** quedaron idénticos el PATH, `Run` (salvo la entrada de Edge, que es de Windows), los certificados, `hosts`, la NRPT y Program Files.

Lo que encontraron las vueltas anteriores, ya corregido:

1. **Sin el Visual C++ Redistributable, los programas no arrancaban** y no imprimían nada (`0xC0000135`). Ahora se compilan con el runtime de C estático (`src/.cargo/config.toml`); los `.exe` solo importan DLL del sistema.
2. **El ayudante seguía en Program Files justo después de `uninstall`:** su carpeta se borra un momento después de que termina. `WindowsSetup::uninstall` espera hasta 15 s a que desaparezca antes de decir que lo quitó.
3. **Ruido de Windows:** con red, Windows añadía certificados raíz y una entrada de Edge. El Sandbox va sin red, y la comparación separa lo de Crisol de lo de Windows.
4. **El Sandbox se cerraba solo** (`0x80370106`). Sin GPU virtual y con un solo script, no volvió a pasar.

**Defender y el UAC, en el equipo del propietario (2026-10-29).** Dentro del Sandbox, Defender casi nunca responde (`0x800106ba`: el servicio no corre de forma estable; una vez respondió a los 7 minutos), así que ahí solo se probó el camino «no disponible». La prueba real se hizo en el equipo del propietario:

- Binarios copiados a `%LOCALAPPDATA%\crisol-prueba-bin` y home de prueba en `%LOCALAPPDATA%\crisol-defender`. Desde `src\target\release`, `system install` se negó con razón: otras cuentas pueden modificar esa carpeta.
- Exclusiones añadidas a mano: `<home>\data` y una ajena, `D:\scratch\no-es-de-crisol`.
- Desde una terminal **normal**: `system install` y `uninstall --all` mostraron el UAC una vez cada uno. La desinstalación dijo «Removed the Defender exclusions inside …\crisol-defender».
- Después, `<home>\data` ya no estaba. La ajena y las 17 exclusiones previas del propietario (Herd, Laragon, Devaron…) seguían intactas.

**F11.2 queda verificada por completo.**

## Corrección: desinstalación segura (propietario, 2026-10-29)

Al responder la revisión, el propietario cambió lo que hace `uninstall --all` **sin** `--purge`. Ahora es una **desinstalación segura**: deja solo lo que es del usuario y borra lo demás de Crisol.

- **Se queda** (`teardown::kept_entries`):
  - `data/` (menos `data/node/`) y `secrets/`: las bases y sus contraseñas, que van juntas, porque sin las contraseñas los datos quedan inaccesibles;
  - `crisol.toml`, `sites.toml` y los `overrides` de `etc/<servicio>/`: la configuración del usuario;
  - `www/`: los proyectos.
- **Se borra** (paso `clean`):
  - `bin/`, `logs/`, `certs/`, `cache/`, `shims/` y `state/`;
  - lo que Crisol generó en `etc/`;
  - `data/node/`: los programas globales de npm de cada versión de Node, que npm puede volver a instalar y no son datos del usuario (propietario, revisión de la Fase 11, hallazgo 4). `crisol uninstall node <versión>` los sigue dejando.

  La lista de lo que queda lo dice: «your databases (mysql…) and their passwords, your settings and your projects». Solo nombra servicios con datos propios (`DbKind`), no `node`.
- **Se borran los programas** (paso `programs`): `crisol.exe`, `crisold.exe`, `crisol-shim.exe` y `crisol-elevate.exe`, los que haya junto a `crisol.exe`.
  - Un programa no puede borrarse mientras corre, así que un `cmd` separado los borra unos segundos después de que termina el comando (`crisol_platform::schedule_delete`).
  - Una ruta con `%`, `!`, `^` o comillas se rechaza: `cmd` la leería como otra cosa. En ese caso, el usuario los borra a mano.
- **`--purge`** sigue borrando la home entera, y también los programas.
- **Las mismas protecciones de la purga:**
  - con algo ajeno a Crisol en la home, no se borra nada de ella;
  - si algo anterior se rechazó o falló, ni la home ni los programas se tocan («skipped»), para poder volver a ejecutarlo con los mismos programas.
  - **Lo mismo si la lista no pudo comprobar algo** (revisión de la Fase 11, hallazgo 1): un almacén de certificados que no se abre, no saber si el ayudante está instalado, la entrada de inicio de sesión o el PATH ilegibles, o algo ajeno en la home. Eso puede seguir en el sistema, y quitarlo necesita los programas.
- **Una sola confirmación** también con `--purge`: el aviso ya lo dice todo (propietario).
