# Resolver `.test` en Windows (F7.1)

Qué hay que hacer para que `shop.test` llegue a `127.0.0.1` sin que el usuario edite nada a mano, con qué permisos y con qué riesgos. Cada dato dice si se **midió** en este equipo (Windows 11 Pro, sin elevar), se **leyó** en la documentación de Microsoft o queda **por verificar** por el propietario con administrador.

## 1. Lo medido en este equipo (solo lectura y un experimento sin tocar el sistema)

- `hosts` ya tiene bloques de otras herramientas (Herd, Laragon, Local, Devaron) con `.test` y `.local`; es el caso previsto en el ADR-0007.
- No hay reglas NRPT; el servicio `Dnscache` corre; nada escucha en el puerto 53; la sesión no está elevada.
- **Un proceso sin elevar puede abrir un socket UDP en `127.77.0.53:53` y responder DNS.** Se probó con un respondedor desechable (A = `127.0.0.1`, AAAA = `::1`, sin dato para otros tipos). No hace falta administrador para *servir*.
- **Sin una regla NRPT, el sistema ignora ese respondedor** (como era de esperar: ningún adaptador lo tiene como DNS).
- `*.localhost` **no** resuelve en Windows con `getaddrinfo` (.NET: «Host desconocido»; `Resolve-DnsName`: «El nombre DNS no existe»; `curl`: no conecta). Los navegadores sí lo tratan como loopback por su cuenta, pero PHP, Node, `curl` y cualquier cliente no navegador no. Por tanto `localhost` no sirve como sufijo general.

## 2. Opción (a): servidor DNS local + regla NRPT 🔒

- La NRPT (Name Resolution Policy Table) dice «los nombres que terminan en `.test` se preguntan a este servidor». Se crea con `Add-DnsClientNrptRule -Namespace ".test" -NameServers <IP>` (leído en la documentación de `Add-DnsClientNrptRule`).
- **Necesita administrador, pero una sola vez** (al instalar). Después, dar de alta o baja sitios no pide permisos: el daemon responde `127.0.0.1`/`::1` a todo lo que termine en `.test` (o solo a los sitios que conoce).
- `NameServers` acepta **solo direcciones IP, sin puerto**. Por eso el respondedor tiene que estar en el puerto 53; el 53 de `127.0.0.1` suele ser terreno disputado (WSL, ICS, otros), así que se propone una IP de loopback dedicada (`127.77.0.53`, medida como libre y enlazable sin elevar). Todo `127.0.0.0/8` es loopback en Windows sin configurar nada.
- Ventajas: cubre cualquier subdominio (`api.shop.test`, `www.shop.test`) sin tocar archivos; no engorda `hosts`; un cambio de sitios no requiere UAC; las reglas locales son del usuario y se borran con `Remove-DnsClientNrptRule`.
- Inconvenientes y riesgos:
  - Con el daemon parado, los `.test` **no resuelven** y la consulta espera el tiempo de espera del DNS (segundos) antes de fallar. Mitigación: la regla solo existe mientras Crisol está instalado, y el respaldo de `hosts` (opción b) cubre el caso «sin daemon».
  - `nslookup` ignora la NRPT (se salta el cliente DNS); para probar hay que usar `Resolve-DnsName` o un cliente real. `crisol doctor` debe usar `getaddrinfo`, no `nslookup`.
  - Hay que **evitar el choque con otra suite**: si ya existe una regla para `.test` (Herd, Laragon, un VPN corporativo) la nuestra la pisa o compite. Se detecta con `Get-DnsClientNrptRule` y se avisa (ADR-0007: una suite a la vez).
  - Una regla NRPT impuesta por directiva de grupo (GPO) tiene prioridad sobre las locales: en un equipo corporativo puede no aplicar. Se detecta y se cae al respaldo.
  - Los navegadores con DNS propio o «DNS seguro» (DoH): Chromium, según su código, usa el resolvedor del sistema cuando hay reglas NRPT; **por verificar** a mano en Chrome, Edge y Firefox.
  - La regla es de la máquina, no del usuario: modifica la configuración del sistema, por eso es una operación aislada y opcional (CLAUDE.md).

## 3. Opción (b): bloque gestionado en `hosts` 🔒

- Un bloque delimitado `# BEGIN CRISOL` … `# END CRISOL` con una línea por sitio (`127.0.0.1 shop.test` y `::1 shop.test`). Conviene escribir también `::1`: un cliente que prueba IPv6 primero tarda o falla si falta. Después de escribir hay que vaciar la caché (`ipconfig /flushdns`).
- **Necesita administrador en cada cambio** del bloque (cada sitio nuevo o quitado), porque `C:\Windows\System32\drivers\etc\hosts` solo lo escribe un administrador. Eso rompe el flujo «`crisol park` y listo» si se pide UAC cada vez. Con la opción (a) el administrador es solo la instalación.
- No hay comodines: `api.shop.test` y `www.shop.test` son líneas distintas; hay que enumerar cada nombre.
- Ventajas: funciona en todas partes, sin daemon, sin GPO, sin puerto 53, y todos los clientes lo respetan (incluida `nslookup`, que lo ignora igualmente, y los navegadores).
- Riesgos: el archivo es compartido con otras herramientas y con antivirus que lo vigilan (algunos lo bloquean o lo restauran); un error al escribir lo puede dejar roto. Reglas: leer, reemplazar **solo** lo que hay entre los delimitadores, conservar el resto byte a byte (finales de línea y codificación), escribir en un temporal de la misma carpeta y reemplazar con `ReplaceFileW` (conserva ACL y atributos, y deja la versión anterior como `hosts.crisol-bak`, renovada en cada cambio y borrada al quitar el bloque), y releer el archivo para comprobar que quedó lo escrito.

## 4. Comparación

| | (a) DNS local + NRPT | (b) bloque en `hosts` |
|---|---|---|
| Administrador | una vez, al instalar | en cada cambio de sitios |
| Subdominios | todos, sin enumerar | uno por línea |
| Sin daemon | no resuelve (espera) | resuelve |
| Depende de GPO/VPN | sí | no |
| Toca archivos del sistema | no (registro de NRPT) | sí (`hosts`) |
| Reversible | `Remove-DnsClientNrptRule` | quitar el bloque |
| Estado de la verificación | respondedor medido; NRPT por verificar con admin | formato conocido |

## 5. Lo que no se puede probar sin administrador

Ninguna prueba automática toca la NRPT ni `hosts` reales. Quedan para el propietario, a mano y con administrador, con la regla creada y quitada en la misma sesión:

1. `Add-DnsClientNrptRule -Namespace ".test" -NameServers 127.77.0.53` con el respondedor de Crisol activo, y comprobar con `Resolve-DnsName shop.test`, `curl` y los tres navegadores.
2. Que con el respondedor parado la consulta falla en pocos segundos y no deja el equipo lento.
3. Que `Remove-DnsClientNrptRule` deja `Get-DnsClientNrptRule` como estaba.
4. El ejecutable mínimo con elevación (UAC) de F7.2.

## 6. Recomendación

Primaria: **(a)**, con el respondedor en `127.77.0.53:53` dentro del daemon y la regla NRPT creada **una sola vez** por un ejecutable/operación mínima que solo hace eso (F7.2). Respaldo: **(b)**, para equipos con GPO, sin daemon o donde (a) no funcione (F7.3). Valor por defecto sin ningún permiso: nada que resolver (los sitios se sirven por `localhost:<puerto>` hasta que el usuario elija). Registrado en el ADR-0014.
