# ADR-0028: Salida de la CLI con formato en la terminal

- **Estado:** aceptada (propietario, 2026-11-03)
- **Contexto de fase:** Fase 13, tarea F13.5b (después de F13.5)
- **Archivos:** `src/crisol-cli/src/render.rs` y `render/`, `src/crisol-platform/src/console.rs`

## Contexto

La CLI escribe texto plano sin formato (`render.rs`: «Plain text, no colors, no cleverness: it is read by people and pasted into issues»). En la verificación de F13.4, el propietario notó que `crisol doctor` cuesta leerlo, y pidió un diseño como el de los instaladores de Laravel o Astro, o al menos las comprobaciones en tabla.

El texto plano tiene dos virtudes que conviene conservar: se pega tal cual en un issue y se puede procesar con otros programas. Además, algunas salidas se copian a archivos (`crisol db credentials` da líneas `CLAVE=valor` para un `.env`), y `--json` ya existe para los programas.

## Decisión

1. **Formato solo cuando la salida va a una terminal.** Hay colores y bordes si stdout es una consola (`IsTerminal`), `NO_COLOR` no está definida y `TERM` no es `dumb`. Si la salida se redirige (`> archivo`, `| clip`, `| findstr`) o se pide `--plain`, sale el texto plano de hoy, sin cambios, para que sigan sirviendo los issues y los scripts. `--json` no cambia.
2. **Las comprobaciones y las listas van en tabla con bordes** (elección del propietario), por ejemplo:

   ```
   ┌─────────┬────────────────────┬─────────────────────────────┐
   │ Status  │ Check              │ Detail                      │
   ├─────────┼────────────────────┼─────────────────────────────┤
   │ ✓ ok    │ home permissions   │ only you can write to it    │
   │ ! warn  │ Visual C++ runtime │ not installed               │
   │         │                    │ fix: https://aka.ms/vs/17/  │
   │         │                    │ release/vc_redist.x64.exe   │
   │ ✗ fail  │ php                │ 8.4.26 cannot start         │
   └─────────┴────────────────────┴─────────────────────────────┘
   5 ok · 1 warning · 1 problem
   ```

   - El estado lleva una marca y un color: verde para bien, amarillo para aviso y rojo para problema. La marca basta sin color, para quien no distingue colores.
   - La última columna se ajusta al ancho de la ventana y corta por palabras, con las filas que haga falta. Una dirección o una ruta que no cabe se parte sin perder caracteres. El arreglo (`fix`) va dentro de la misma celda.
   - Si la ventana es demasiado angosta para la tabla (menos de unas 60 columnas), se usa la forma de lista de hoy, con marcas y color.
   - Los resúmenes («5 ok · 1 warning · 1 problem») van debajo.
3. **Alcance: toda la CLI de una vez** (propietario). Tablas para `doctor`, `status`, `sites`, `list` y las versiones; marcas y color para los mensajes de `Done`, avisos y errores; y el progreso de `install` en una sola línea que se redibuja, solo en terminal. Quedan **sin formato** las salidas hechas para copiarse: `db credentials`, los scripts de shell que da la CLI y `logs`, que es el texto del programa tal cual.
4. **Sin dependencias nuevas, salvo justificarlas.** Los colores ANSI y los bordes de caja se escriben a mano en `render`. En la consola clásica de Windows hay que activar el modo de terminal virtual (`ENABLE_VIRTUAL_TERMINAL_PROCESSING`), y eso va en `crisol-platform` (`console.rs`), igual que el ancho de la ventana (`GetConsoleScreenBufferInfo`). Si activar el modo falla, se escribe sin colores. El ancho de cada carácter se cuenta por carácter; si hiciera falta medir caracteres anchos, se justifica `unicode-width` en la bitácora.
5. **Sigue siendo presentación.** Armar la tabla y cortar las líneas es dibujar, y por eso va en el CLI (`render`), con pruebas a un ancho fijo. Qué se comprueba y qué se arregla sigue viniendo del núcleo. Los textos siguen en inglés.

## Consecuencias

- Las pruebas del CLI que leen la salida corren sin terminal y siguen viendo el texto plano. La forma con formato se prueba llamando a `render` con un ancho y un «con color» fijos.
- Un issue pegado desde la pantalla trae bordes y marcas. Se lee bien igual, y `--plain` o `| clip` dan el texto de siempre.
- La GUI no cambia: dibuja lo mismo desde los datos, no desde este texto.
