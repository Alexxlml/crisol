# ADR-0007 — Una herramienta local a la vez

- **Estado:** aceptada
- **Fecha:** 2026-10-14

## Contexto

En el equipo del usuario conviven Herd, Laragon, Local y Devaron, y sus bloques en el archivo `hosts` ya comparten el sufijo `.test`. Además, MySQL y MariaDB usan el mismo puerto por defecto (3306). El usuario aclara que en la práctica nadie ejecuta dos suites locales a la vez, ni MySQL y MariaDB juntos.

## Decisión

- El sufijo por defecto sigue siendo **`test`**. Crisol no intenta repartirse nombres con otras herramientas.
- Crisol asume que **solo una suite local corre a la vez** (Herd, Laragon, etc.). `crisol doctor` (F11.1) **advierte** si detecta otra suite activa o dominios `.test` definidos fuera de su bloque, y recomienda usar una sola.
- **MySQL y MariaDB son alternativas excluyentes** que comparten el puerto 3306: se puede instalar ambas, pero solo una activa. Crisol avisa al intentar arrancar la segunda. MySQL (o MariaDB) con PostgreSQL sí pueden correr juntos: usan puertos distintos.
- Una colisión de puerto nunca se resuelve matando procesos ajenos (ya fijado en el plan, F2.4).

## Consecuencias

- F1.2 define un solo puerto por defecto por servicio, sin lógica de reparto.
- F9.1 incluye la comprobación de exclusión MySQL/MariaDB.
- F11.1 incluye la detección de otras suites y de nombres duplicados en `hosts`.
