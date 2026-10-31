# Instalar Crisol

Crisol es para Windows 10 y 11 (x64). No pide administrador para instalarse; solo lo pide, una vez y si tú quieres, para que los nombres `.test` funcionen.

> **Estado:** todavía no hay versiones publicadas. Esta guía describe cómo será la instalación cuando las haya (ADR-0024).

## Con winget (recomendado)

```powershell
winget install Alexxlml.Crisol
crisol setup
```

`winget install` descarga el ZIP, comprueba su SHA-256 y deja `crisol` disponible en tu terminal. No toca tu PATH: usa la carpeta de enlaces que winget ya mantiene. Abre una terminal nueva si `crisol` no se reconoce.

`crisol setup` hace, una vez, lo que winget no puede:

1. Pone los comandos `php`, `node`, `npm`, `composer`… en tu PATH (solo el tuyo), cada uno con la versión que pida tu proyecto.
2. Pregunta por separado, y puedes decir que no a cada una:
   - **Confiar en la CA local de Crisol**, para abrir tus sitios `https` sin avisos del navegador. Windows te lo vuelve a preguntar. Para comparar las huellas antes del diálogo, usa `crisol trust` en vez de `setup`.
   - **Instalar el ayudante** y la regla DNS que hace funcionar `.test` (un aviso de administrador).
   - **Arrancar Crisol al iniciar sesión.**

`crisol setup` solo hace lo que falta, así que puedes repetirlo cuando quieras. `crisol setup --yes` no pregunta (Windows sigue preguntando lo suyo).

## Avisos de Windows

Las primeras versiones **no están firmadas** con Authenticode (ADR-0024, sección 5). Windows puede mostrar SmartScreen («editor desconocido») y el aviso de administrador de `crisol setup` dirá lo mismo. La firma llegará más adelante.

## Actualizar

```powershell
crisol shutdown
winget upgrade Alexxlml.Crisol
crisol setup
```

Hay que parar el daemon antes: Windows no deja reemplazar un `.exe` que está en uso. `crisol setup` actualiza los comandos (`php`, `node`…), que son copias del programa anterior, y corrige el arranque al iniciar sesión si apuntaba a una ruta que cambió. `crisol doctor` avisa si el daemon que corre es de otra versión que el programa instalado.

## Desinstalar

```powershell
crisol uninstall --all
winget uninstall Alexxlml.Crisol
```

El primero lista todo lo que Crisol cambió en Windows, pregunta, y lo revierte (PATH, certificados, regla DNS, bloque de `hosts`, ayudante, arranque…). **Conserva tus bases de datos, sus contraseñas, tu configuración y tus proyectos**; `--purge` borra también eso, después de preguntarte si ya respaldaste. Como winget instaló los programas, `uninstall --all` no los borra: el segundo comando lo hace.

## Sin winget (avanzado)

1. Descarga de la página de releases `crisol-<versión>-windows-x64.zip`, `SHA256SUMS` y `SHA256SUMS.minisig`.
2. Comprueba el ZIP: `sha256sum -c SHA256SUMS` (o `Get-FileHash` y compara con la línea del archivo).
3. Comprueba la firma de `SHA256SUMS` con [minisign](https://jedisct1.github.io/minisign/) y la clave pública de Crisol:

   ```
   RWSTr/H7Nu+fV4OekRkO2WwGYtjLHsDjgo/61Nc9JW6FQP1lPVUkwJgJ
   ```

   `minisign -V -P RWSTr/H7Nu+fV4OekRkO2WwGYtjLHsDjgo/61Nc9JW6FQP1lPVUkwJgJ -m SHA256SUMS`
4. Descomprímelo **dentro de tu perfil** (por ejemplo `%LOCALAPPDATA%\Crisol\bin`) y usa la ruta completa de `crisol.exe` o añade esa carpeta a tu PATH. Crisol se niega a registrar o elevar programas que estén en una carpeta donde otra cuenta pueda escribir (la raíz de un disco, por ejemplo): `crisol setup` te dice qué comando cierra la carpeta.
5. `crisol setup`.

`crisol setup` no copia ni mueve los programas. Para desinstalar, `crisol uninstall --all` también los borra (no son de winget).
