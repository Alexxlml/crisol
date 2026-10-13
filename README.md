# Crisol

Suite de desarrollo web local para PHP (y Node), escrita en Rust.
Alternativa abierta a Herd, Laragon y Devaron, pensada para que **todo funcione completo, sin funciones de pago y sin ensuciar el sistema**.

> Estado: planificación. Fase actual en `claude/ESTADO.md`.

## Principios

1. **El backend es el producto.** La lógica vive en un núcleo sin interfaz; CLI y GUI son carcasas intercambiables.
2. **Cero contaminación del sistema.** Una sola entrada en el PATH (`<CRISOL_HOME>\shims`). Nada de variables por versión. Desinstalar deja el sistema como estaba.
3. **La configuración del usuario es sagrada.** Crisol nunca reescribe por su cuenta lo que el usuario configuró. Lo generado se regenera; lo del usuario solo cambia con un comando explícito.
4. **Rápido y ligero.** Un ejecutable pequeño, arranque inmediato y RAM mínima en reposo.
5. **Multiplataforma por diseño, Windows primero.** Lo específico del sistema operativo queda detrás de traits.
6. **Todo completo y gratis.** Sin funciones detrás de un botón de pago.

## Estructura del repositorio

| Carpeta | Contenido |
|---|---|
| `src/` | Código fuente (workspace de Cargo) |
| `claude/` | Memoria de trabajo para Claude Code: estado, bitácora de sesiones, prompts |
| `docs/` | Arquitectura, plan de fases y decisiones (ADR) |
| `analisis/` | Investigación de herramientas existentes (Devaron, Laragon, Herd) |

Archivo de entrada para agentes: `CLAUDE.md`.
