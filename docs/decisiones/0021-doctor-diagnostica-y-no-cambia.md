# ADR-0021 — `doctor` diagnostica y no cambia nada

- **Estado:** propuesta (2026-10-29, F11.1). Falta que el propietario confirme la sección 3 (Defender).
- **Fecha:** 2026-10-29

## Contexto

F11.1 pide que `crisol doctor` detecte, cada uno con su solución:

- puertos en conflicto;
- otros PHP/Node en el PATH;
- si la CA está confiada;
- el DNS de `.test`;
- servicios caídos;
- permisos.

La nota del plan añade:

- medir el arranque lento de las bases por Defender, «con opción de excluir solo `data/`»;
- colisiones de dominios en `hosts`;
- un diagnóstico de errores de arranque (runtime VC++ ausente, DLL faltante).

Tres cosas no son obvias:

1. **Quién sabe qué.** El motor (`Engine`) solo ve archivos y corre en el hilo de trabajo del daemon. El estado de los servicios lo tiene el actor (`Daemon`). Y los códigos NTSTATUS, el registro y Defender son de Windows.
2. **Qué es una colisión en `hosts`.** En este equipo, Herd y Laragon definen 46 nombres bajo `.test`, casi todos con `127.0.0.1`.
3. **La exclusión de Defender necesita administrador.** F11.1 no está marcada como «requiere admin».

## Decisión

### 1. Tres capas

- **El motor** comprueba lo que se ve en archivos:
  - versiones sin soporte, incluida una 7.4.x instalada con `--from`;
  - restos de `runtime.log.rotating*`;
  - **todas** las carpetas abiertas de la home, no solo la primera;
  - sitios que `hosts` manda a otra dirección.
- **El daemon** añade, con `Daemon::complete_doctor`, lo que solo él sabe. El actor lo llama cuando el hilo de trabajo termina un `doctor`, y `Daemon::handle` lo llama en el modo sin daemon del CLI. Añade:
  - `services`: los que fallaron, con su causa y el significado del código de salida;
  - `ports`: los puertos de los servicios activados que **no** están corriendo y que otro programa ocupa, con un puerto libre sugerido. Los puertos de los servicios que corren son suyos.
  - `database startup`: cuánto tardó cada base en estar lista.
- **La plataforma**, detrás de `SystemChecks`, aporta:
  - `explain_exit` (códigos NTSTATUS);
  - `hosts_content` (solo lee el archivo);
  - `slow_start_hint`;
  - comprobaciones nuevas: runtime VC++ por el registro (solo lectura) o por las DLL de `System32`, y `::1`;
  - la regla NRPT, que ahora se comprueba resolviendo `crisol-doctor.<tld>` con el resolvedor de Windows cuando el daemon corre. Si no corre, avisa de que los nombres no resuelven.

### 2. Colisiones en `hosts`

- Solo es un problema un nombre que **es un sitio de Crisol** (o un subdominio de uno), que está **fuera** del bloque de Crisol y que apunta a una dirección que **no** es de loopback. Windows lee `hosts` antes que cualquier DNS, así que ese sitio no llega a Crisol.
- Una línea con `127.0.0.1` de otra suite no estorba a la resolución: si otra suite tiene el puerto, eso lo dice `ports`.
- La línea `hosts block` cuenta los nombres ajenos bajo el sufijo, sin listarlos.

### 3. Defender: medir y sugerir, nunca ejecutar

- `DbStack` mide el tiempo desde el lanzamiento hasta `Running`.
  - Solo cuenta los arranques en los que los datos ya existían: el primero también los crea (`initdb`, `mysqld --initialize`) y su duración no dice nada de Defender.
  - Más de 10 s es «lento».
- Si es lento, `doctor` da como `fix` el comando exacto para un administrador, y también el que lo deshace:
  - `Add-MpPreference -ExclusionPath '<home>\data'`;
  - `Remove-MpPreference -ExclusionPath '<home>\data'`.
- **Crisol no lo ejecuta ni lo pide por el ayudante elevado.** Así no hay nada de Crisol que revertir en F11.2. Si más adelante se quiere que Crisol añada la exclusión, será una operación nueva del ayudante con su ADR, marcada «requiere admin», y F11.2 tendrá que revertirla.

### 4. Lo que queda sin solución sugerida

- **La CA no confiada** no tiene comando: `crisol trust` no existe todavía (Fase 8, «comandos que faltan»). `doctor` explica la consecuencia: los navegadores avisan, y los *streams* de PHP (`file_get_contents`) rechazan los sitios locales aunque el cURL de PHP los acepte.

## Consecuencias

- `doctor` sigue siendo de solo lectura. Lo único que «toca» es:
  - un *bind* efímero en `[::1]:0`;
  - el sondeo de puertos de `PortProbe::probe`, que puede reclamar un momento un puerto libre (por eso no se usa con los servicios que corren);
  - una consulta DNS a un nombre que no existe.
- `Catalog::is_end_of_life` cambia de significado: una versión que el catálogo no lista toma lo que dice de su línea (sin soporte si todas las versiones de esa línea lo están). Lo usan `list`, la resolución de versiones y el `Installed::end_of_life` de `--from`.
- Con el `hosts` real de este equipo (Herd y Laragon), la línea `hosts block` resume en una frase los 46 nombres ajenos en vez de listarlos. Todos apuntan a `127.0.0.1`, así que tampoco darían `site names` aunque coincidieran con un sitio de Crisol.
