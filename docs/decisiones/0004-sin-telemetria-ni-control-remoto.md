# ADR-0004 — Sin telemetría ni control remoto

- **Estado:** propuesta (pendiente de aprobación del usuario)
- **Fecha:** 2026-10-13

## Contexto

Una herramienta de desarrollo local no necesita hablar con un servidor para funcionar. Enviar datos del equipo o del usuario, consultar avisos remotos o poder bloquear su uso a distancia son riesgos sin beneficio para quien la usa, sobre todo si no se pueden desactivar.

## Decisión propuesta

- Crisol **no envía datos a ningún servidor** de forma predeterminada ni sin acción explícita del usuario.
- Crisol **no tiene mecanismo de bloqueo, desactivación ni aviso remoto**.
- Las únicas conexiones de red son las que el usuario provoca: descargas del catálogo (Fase 3) y comprobación de versión si el usuario la pide.
- Si algún día se añade telemetría o reporte de errores, será **opt-in**, documentada, con un comando que muestre exactamente qué se enviaría.

## Consecuencias

- No hay métricas de uso; los problemas se reportan manualmente.
- Las pruebas de integración pueden afirmar que el daemon no abre conexiones salientes salvo las de descarga.
