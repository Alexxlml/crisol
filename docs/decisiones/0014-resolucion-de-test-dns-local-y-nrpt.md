# ADR-0014 — Resolución de `.test`: DNS local con NRPT, `hosts` de respaldo

- **Estado:** propuesta (pendiente de la verificación manual del propietario con administrador)
- **Fecha:** 2026-10-16

## Contexto

Los sitios necesitan que `*.test` llegue a `127.0.0.1`/`::1`. Windows no lo resuelve solo (medido: ni siquiera `*.localhost` con `getaddrinfo`). Las dos vías, con sus pruebas y riesgos, están en `analisis/dns-windows.md`: un servidor DNS local más una regla NRPT, o un bloque gestionado en `hosts`. Ambas necesitan administrador; difieren en cuándo.

## Decisión

1. **Primaria: DNS local + NRPT.** El daemon responde A=`127.0.0.1` y AAAA=`::1` para el sufijo configurado, escuchando en una IP de loopback dedicada (`127.77.0.53:53`, que se enlaza sin elevar; la NRPT solo acepta IP sin puerto). La regla `Add-DnsClientNrptRule -Namespace ".<tld>" -NameServers 127.77.0.53` se crea **una vez** en la instalación.
2. **Respaldo: bloque gestionado en `hosts`** entre `# BEGIN CRISOL` y `# END CRISOL`, con líneas IPv4 e IPv6 por nombre, escritura atómica, copia previa y sin tocar nada fuera del bloque.
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
