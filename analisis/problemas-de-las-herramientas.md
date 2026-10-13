# Problemas que Crisol debe resolver

Lista de requisitos derivados de la experiencia con Herd, Laragon y Devaron.

| Problema | Requisito en Crisol | Dónde se resuelve |
|---|---|---|
| Funcionan a medias | Menos funciones, cada una con prueba E2E en Windows | Criterios de aceptación de cada fase |
| Funciones clave detrás de pago | Todo abierto y completo | ADR-0001 / licencia |
| Variables de entorno que no se limpian | Una sola entrada en PATH; desinstalar revierte todo | ADR-0002, F4.4, F11.2 |
| Herramientas que se interfieren entre sí | Detectar otros PHP/Node en el PATH y puertos ocupados; no tocar los ajenos | F2.4, F4.4, F11.1 |
| Lo que tiene una no lo tiene otra | Cubrir la unión: multi-versión PHP por sitio, HTTPS, `.test`, MySQL/MariaDB/PostgreSQL/Redis/Mailpit/Meilisearch, Node | Fases 4–10 |
| Configuración que se revierte sola | La config del usuario es la fuente de verdad | ADR-0003, F1.3 |
| Extensiones PHP que hay que habilitar a mano | `php.ini` generado con extensiones comunes activas | F4.1 |
| Aplicaciones pesadas | Rust, daemon mínimo, GUI que se puede cerrar sin detener servicios | ADR-0001 |

## Pendiente de completar

Agregar aquí, conforme aparezcan, problemas concretos de Herd y Laragon (con versión y pasos para reproducirlos).
