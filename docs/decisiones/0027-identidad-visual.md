# ADR-0027: Identidad visual de Crisol

- **Estado:** aceptada (propietario, 2026-10-31)
- **Contexto de fase:** antes de F13.3 (esqueleto de la GUI, ADR-0026)
- **Archivos:** `assets/brand/` (`tokens.css`, `symbol.svg`, `app-icon.svg`, `tray/`). Propuestas en el lienzo «Identidad de Crisol» (claude.ai, privado del propietario).

## Contexto

F13.3 dibuja la bandeja y la ventana por primera vez. Sin una identidad mínima, cada pantalla mete sus colores y sus iconos provisionales, y luego hay que limpiarlos uno por uno. Además, el propietario quiere una web de documentación que comparta la identidad.

La metáfora la dio el propietario: un crisol es sencillo a la vista y potente cuando trabaja. Aguanta el calor, funde metales distintos y es la parte fundamental del proceso. En reposo es color crema; trabajando, se pone al rojo vivo.

Se propusieron tres direcciones: **A · Refractario** (crema y naranja fundido), **B · Al rojo vivo** (hierro oscuro; el calor como única luz) y **C · Ceniza y ámbar** (neutra, un solo acento).

## Decisión

**La dirección B, con las fuentes de A, los iconos de estado de A y palabras de siempre.**

### Color: tres capas que no se mezclan

1. **El recipiente:** fondos, superficies, texto y líneas en neutros cálidos. Ocupan casi toda la pantalla.
2. **El calor:** amarillo, naranja y rojo cereza son **solo de Crisol**: que algo trabaja, la acción principal y la marca. La escala va de parado a corriendo (`--heat-0` a `--heat-4`).
3. **Los metales:** cada tecnología conserva un color aproximado al de su proyecto, **solo como un punto junto a su nombre**. Nunca es fondo, botón ni estado, y **no se usan logotipos** (son marcas registradas). En el tema oscuro, los colores oscuros (MariaDB, PostgreSQL) se aclaran para que el punto se vea.

El estado siempre lo dice Crisol; la tecnología solo se identifica. Un fondo verde de Node se leería como «todo bien».

### Temas

- **Oscuro principal:** hierro `#141110`, superficie `#1E1A17`, texto crema `#F2E9DC`, acción `#FF8A3D`.
- **Claro:** crema `#F4EEE3`, superficie blanca, texto `#1E1A17`, acción `#C2410C`.
- La GUI sigue el tema de Windows. `data-theme="light|dark"` en `<html>` lo fuerza, por si después se ofrece la opción.
- **Contraste comprobado:** todo texto pasa 4.5:1 sobre las superficies a las que va, y los puntos de los metales pasan 3:1. El peor caso es el enlace claro sobre `--surface-raised`, con 4.54:1.

### Tipografía (la de A)

- **Space Grotesk** para títulos y la marca;
- **Inter** para la interfaz;
- **JetBrains Mono** para comandos, rutas y logs.

Las tres tienen licencia SIL OFL 1.1 y **van dentro de la GUI**: es una aplicación local y no descarga fuentes al ejecutarse. Sus licencias se añaden a `licenses/` y a `THIRD-PARTY-NOTICES.md` en F13.3.

### Formas

- **Esquinas casi cuadradas, como hierro fundido:** chips de estado de 4 px, botones de 6 px y paneles de 10 px.
- **Objetivos de clic de al menos 44 px.**
- **Espaciado** en pasos de 4 px.

### Símbolo e iconos

- **El símbolo** es el crisol de perfil (borde y cuerpo que se estrecha) con metal fundido en degradado (`assets/brand/symbol.svg`).
- **Bandeja, cinco estados**, cada uno para barra de tareas oscura (`-dark`) y clara (`-light`), en `assets/brand/tray/`:

| Estado de la bandeja | Icono | Archivo |
|---|---|---|
| Corriendo | lleno de metal fundido | `running-*.svg` |
| Arrancando | a medio llenar | `starting-*.svg` |
| Parado | vacío | `stopped-*.svg` |
| Algo falló | vacío con insignia roja y «!» | `failed-*.svg` |
| Sin daemon | contorno punteado gris | `no-daemon-*.svg` |

La grieta de B se descartó: a 16 px se pierde.

### Palabras: las de siempre, no de fundidor

La metáfora vive en el dibujo, no en el texto. Los estados se nombran como en cualquier herramienta:

| Estado | Español | English |
|---|---|---|
| `running` | Corriendo | Running |
| `starting` | Arrancando | Starting |
| `stopping` | Deteniendo | Stopping |
| `stopped` | Parado | Stopped |
| `failed` | Falló | Failed |
| bandeja: algún servicio falló | Algo falló | Something failed |
| bandeja: no hay daemon | Sin daemon | Daemon not running |

Nada de «frío», «agrietado» ni «calentando».

### Chips de estado

Cuadrados (`--radius-sm`), con fondo y texto del estado (`--state-*`) y un signo que no depende del color: ● corriendo, ◐ arrancando y deteniendo, ○ parado y ✕ falló. En la GUI se dibujan con CSS (`.sign`), no con caracteres: los caracteres salían de fuentes de respaldo distintas y no quedaban centrados (verificación de F13.3).

## Lo que esto fija para F13.3

- La GUI importa `assets/brand/tokens.css` y **no escribe colores sueltos**: todo va por variables. Lo que falte se añade como token.
- **Los iconos de la bandeja salen de `assets/brand/tray/`.** Tauri necesita PNG o ICO: la conversión (por ejemplo con `tauri icon` o un script) se documenta y los resultados se versionan. Hay que elegir la variante `-dark` o `-light` según el tema de la barra de tareas de Windows (`SystemUsesLightTheme`), y cambiarla si el usuario cambia el tema.
- **El icono de la aplicación** (ventana, menú Inicio y `.exe`) sale de `app-icon.svg`: el símbolo sobre un cuadrado redondeado de `--bg`. Sin el fondo, el borde crema se perdía sobre un menú Inicio o un explorador claros (propietario, 2026-10-31, revisión de F13.3). La bandeja sigue con sus variantes `-dark` y `-light`.
- **Las fuentes** se empaquetan con sus licencias.
- **Los nombres de los estados** salen de la tabla de arriba, en el mecanismo de traducción que elija F13.3.

## La web de documentación

Usará los mismos tokens. Se planificará aparte de la Fase 13: guía de usuario en español e inglés, generador estático y despliegue en Cloudflare Pages. Para ella queda abierto el logotipo con el nombre: `CRISOL` en Space Grotesk, mayúsculas y espaciado amplio, como en el tablero «Elegida». Se confirmará al diseñar la web.

## Consecuencias

- **Fuera de la Fase 13**, la identidad cambia en un solo lugar (`tokens.css`) sin tocar pantallas.
- **Se añade `assets/` a la raíz del repositorio:** recursos compartidos por la GUI y la web, que no son código.
- **Los colores de los metales son aproximados.** Si un proyecto pide otro uso de su color, se cambia o se pasa a `--metal-other` sin afectar a nada más.
