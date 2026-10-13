# ADR-0001 — Rust y backend separado de la interfaz

- **Estado:** aceptada
- **Fecha:** 2026-10-13

## Contexto

Herd, Laragon y Devaron funcionan a medias, ponen funciones clave detrás de pagos, o dejan variables de entorno que se pisan entre sí. Devaron tiene una buena interfaz, pero está hecho en .NET 8 + Avalonia y se distribuye con el runtime completo sin recortar, lo que lo hace pesado y en la práctica lo ancla a Windows.

## Decisión

- Lenguaje: **Rust**, para tener un ejecutable pequeño, arranque inmediato, poca RAM y un camino real a Linux y macOS.
- El backend (núcleo + daemon + CLI) se construye **primero y por separado**. La interfaz gráfica es una carcasa que habla con el daemon por IPC.
- La elección de framework de GUI (candidatos: Slint y Tauri 2) se pospone hasta que el protocolo esté estable (Fase 12), y se decidirá con prototipos medidos (RAM en reposo, tiempo de arranque, comodidad de desarrollo).

## Consecuencias

- La GUI no puede contener lógica: todo lo que haga debe existir antes como operación del protocolo.
- Se puede usar Crisol solo desde la terminal desde el primer día.
