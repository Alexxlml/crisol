# ADR-0030: Mensajes del núcleo traducibles

- **Estado:** propuesta (Opus, 2026-11-12). El propietario ya decidió tres puntos: `--json` idéntico, `CRISOL_LANG` para forzar el idioma y `--help` en inglés.
- **Contexto de fase:** Fase 13, tarea F13.10, antes del primer release
- **Enmienda:** ADR-0026 §4 (los mensajes del núcleo dejan de ir siempre en inglés)
- **Archivos:** `src/crisol-core/src/i18n.rs` (nuevo) e `i18n/` (nuevo), `src/crisol-core/build.rs` (nuevo), `src/crisol-protocol/src/text.rs` (nuevo), `src/crisol-client/src/language.rs`, `src/crisol-gui/build.rs`, `docs/protocolo.md`

## Contexto

En la verificación en Windows Sandbox (2026-11-08), la ventana en español mostraba en inglés los resultados de los pasos de `setup` y de `uninstall --all`, el detalle de Diagnóstico y los errores. La ADR-0026 §4 lo había aceptado para la primera versión. El propietario decidió que se traduzca **antes del primer release**.

Hoy el núcleo arma frases terminadas: unas 150 variantes de error con `#[error("…")]` y unas 600 llamadas a `format!`, entre `crisol-core`, `crisol-platform`, `crisol-daemon` y `crisol-cli`. Esas frases viajan como `String` en el protocolo (`RpcError.message`, `Check.detail`, `DoneInfo.message`, `warnings`) y en los structs que la CLI serializa con `--json` (`onboarding::Plan`, `teardown::Outcome`). La CLI imprime con `--json` la respuesta del protocolo tal cual (`serde_json::to_string(reply)`), así que **cualquier campo nuevo del protocolo saldría también en `--json`** si no se evita a propósito.

La ventana ya traduce sus propios textos: `ui/i18n/{en,es}.json`, el `Dictionary` de `crisol-gui`, `Language::from_preferences` en `crisol-client` con `GetUserPreferredUILanguages`, y `Words` (clave y datos) para las etiquetas de los botones de Diagnóstico.

## Decisión

### 1. Lo que dice el núcleo es una clave con datos, y el inglés sale del diccionario

- **Tipo `Message`** en `crisol_core::i18n`: `key: &'static str` más `args: Vec<(&'static str, String)>`. Se arma con una macro: `msg!("doctor.ca.stale", names = list)`. Los datos son siempre texto ya formateado: rutas con `display()`, versiones, nombres y números en decimal.
- **El inglés no se escribe dos veces.** `Message` implementa `Display` con el diccionario inglés incrustado. Los errores pasan de `#[error("texto")]` a `#[error("{}", self.message())]` con un método `message(&self) -> Message` por enum (trait `Localized`). Así el texto en inglés de hoy pasa a `en.json` tal cual, y `to_string()` sigue dando lo mismo que antes.
- **No se traducen:** comandos (`crisol untrust --stale`, `icacls …`), rutas, versiones, nombres de servicios y de sitios, direcciones ni códigos. Van como datos (`{command}`, `{path}`). Tampoco se traducen los logs de `tracing`, las líneas `CLAVE=valor` de `db credentials`, el contenido de los logs de los servicios ni `--help` (decisión del propietario: clap no trae traducción y duplicaría cada ayuda).
- **Plurales:** dos claves, `x_one` y `x_other`, elegidas por el dato `count` (`one` si `count` es `1`, `other` en los demás casos). Basta para el inglés y el español. Un idioma con más formas (el polaco, por ejemplo) necesitará reglas en el código: eso queda fuera de esta ADR.

### 2. Los diccionarios

- **Mensajes del núcleo:** `src/crisol-core/i18n/<código>.json`, un objeto plano de clave a frase con `{marcadores}`, igual que los de la ventana. El núcleo los incrusta todos. Son unos pocos cientos de líneas por idioma, y sin red ni archivos externos funcionan igual en la CLI, el daemon y la GUI.
- **Textos de la ventana:** siguen en `src/crisol-gui/ui/i18n/<código>.json`. No se mezclan, porque son cosas distintas (los de la ventana los usa solo la página), pero se escriben con el mismo vocabulario. Ese vocabulario es el de `es.json` de la ventana (Registros, Diagnóstico, CA, «configurar»), en español neutro con «tú».
- **Un idioma nuevo es agregar archivos.** `build.rs` de `crisol-core` lee `i18n/*.json` y genera la lista de idiomas (código y texto incrustado). `build.rs` de `crisol-gui` hace lo mismo con `ui/i18n/`. `Language` deja de ser un `enum` de dos valores y pasa a ser un código de la lista (`Language::code()`, `Language::ENGLISH`). Una prueba exige que los dos lugares tengan los mismos idiomas. Traducir es copiar `en.json` dos veces, traducirlos y abrir un PR, sin tocar código.
- `Language` y `from_preferences` se mudan de `crisol-client` a `crisol_core::i18n`, y `crisol-client` los reexporta para que la GUI no cambie sus `use`.

### 3. Qué idioma se usa

1. `CRISOL_LANG` (`en`, `es`, …), si nombra un idioma que existe. Así se fuerza el inglés para pegar algo en un issue (decisión del propietario). Un valor desconocido se ignora y se sigue con el punto 2.
2. El primero de `GetUserPreferredUILanguages` (`crisol_platform::ui_languages`) que tenga traducción.
3. Inglés.

Lo decide `crisol_core::i18n::choose(env, preferred) -> Language`, una función pura con pruebas. La CLI y la GUI la llaman al crear su `Context`, que gana un campo `language`. La salida redirigida no cambia de idioma, porque saldría distinto según dónde se mire; para un issue está `CRISOL_LANG=en`, que se documenta en `docs/` (y en la plantilla de issues, cuando la haya).

### 4. Quién traduce: el cliente, nunca el daemon

- **El daemon siempre contesta en inglés**, como hoy, y además manda la clave y los datos. No sabe ni le importa quién lo lee, y dos clientes con idiomas distintos pueden hablarle a la vez.
- **El protocolo gana el tipo `Text`** (`{ "key": "…", "args": { "name": "valor" } }`) y campos opcionales junto a cada texto traducible. Los campos de hoy siguen ahí y en inglés. **El protocolo sigue en la versión 3** (ADR-0025): solo se agregan campos opcionales, que un cliente viejo ignora y que un daemon viejo no manda.
  - `RpcError`: `text` y `fix_text`.
  - `Check`: `name_text`, `detail_text` y `fix_text`. `name` sigue siendo el identificador inglés de la fila.
  - `DoneInfo` y `StatusInfo`: `message_text` y `warnings_text`. `warnings_text` es `Vec<Option<Text>>`, vacío o del mismo largo que `warnings`; si el largo no coincide, el cliente lo ignora.
  - Los eventos no cambian en esta tarea.
- **`crisol-client` convierte cada `Text` en frase** con el idioma del `Context`, antes de dárselo a la CLI o a la página. La página recibe frases ya traducidas y su diccionario no cambia. Si falta la clave en ese idioma, se usa el inglés del diccionario; si falta también ahí (un daemon más nuevo), se usa el texto inglés que vino en el campo de siempre. Nunca se muestra una clave cruda.
- **Lo que corre en el proceso del cliente** (`setup`, `uninstall --all`, las acciones locales de Diagnóstico) usa `Message` directamente, sin pasar por el protocolo.

### 5. `--json` no cambia (decisión del propietario)

- Mismos campos y mismo texto en inglés, sin campos nuevos. Los scripts y los issues no notan nada.
- **En los structs del núcleo,** `Message` se serializa como su frase en inglés (un `String`). `Plan`, `Step` y `Outcome` salen igual que hoy sin hacer nada más.
- **En el protocolo,** la CLI imprime `--json` después de `strip_texts()`, un método de `Reply` y de `RpcError` que vacía los campos `*_text`. Esos campos llevan `skip_serializing_if`, así que no salen. Una prueba de la CLI compara la salida `--json` de una respuesta con textos contra la forma de hoy.

### 6. Por partes

- **F13.10a, la base:** `i18n.rs`, `msg!`, `Localized`, `build.rs` con la lista de idiomas, `Language` en el núcleo con `choose` y `CRISOL_LANG`, `Text` y sus campos en el protocolo, `strip_texts`, la traducción en `crisol-client` y `docs/protocolo.md`. Se convierten algunos mensajes de muestra para estrenar todo.
- **F13.10b, lo que se ve en la ventana:** los pasos de `setup` y de `uninstall --all` (`describe`, `question` y los mensajes de `Outcome`), todas las comprobaciones de `doctor` (las del motor, las de `crisol-platform/src/system.rs` y `diagnose.rs`, y la de la versión del daemon en `crisol-client`), y los errores que pueden salir de las órdenes de la ventana. Esos errores son los de `trust`, `ca`, `paths`, `install` y `manage`, `onboarding` y `teardown`, `shimsetup`, `autostart` y `sites`, además de los `RpcError` que arma el daemon para esas llamadas. Con esto la ventana en español ya no muestra frases del núcleo en inglés. La GUI deja de mostrar `row.name` y muestra el nombre traducido, pero sigue usando `name` como identificador de la fila.
- **F13.10c, el resto de la CLI:** los demás errores del núcleo, `DoneInfo` y `warnings` del daemon, las frases propias de `crisol-cli` (`writeln!` con texto) y los encabezados de las tablas de `render.rs` (ADR-0028). `render` recibe el idioma y no decide nada más.
- Mientras una parte no esté hecha, lo que falta sigue saliendo en inglés, como hoy. Ningún paso deja la salida rota.

## Pruebas (aceptación)

1. **Los diccionarios se corresponden:** cada idioma tiene exactamente las claves de `en.json` (ni más ni menos) y, por clave, los mismos `{marcadores}`. Los plurales tienen siempre los dos, `_one` y `_other`. Esto vale para los dos lugares, el núcleo y la ventana.
2. **Toda clave usada existe y recibe sus datos:** una prueba recorre el código de los crates (`src/**/*.rs`) buscando `msg!("clave", a = …, b = …)` y comprueba que la clave está en `en.json` y que los nombres de los datos son justo sus marcadores. Si falta un dato o sobra uno, falla.
3. **Nada en inglés en la ventana:** con los dobles del núcleo, cada `Check` que devuelve `doctor`, cada `Step` y cada `Outcome` de `setup` y `uninstall --all` llevan `Message` o `Text`. Una prueba falla si alguno sale con texto suelto.
4. **El idioma:** `choose` con `CRISOL_LANG` válido, desconocido y vacío; con `es-MX` y `fr-FR` primero; sin preferencias.
5. **La reserva:** una clave que no está en el idioma usa la inglesa, y una clave desconocida usa el texto del campo de siempre.
6. **`--json`:** la salida de `doctor`, `setup` y un error, con textos dentro, es idéntica a la de hoy (comparación con la forma esperada).
7. **Compatibilidad:** un `Check` y un `RpcError` sin los campos nuevos se leen bien, y un cliente que no los conoce ignora los que sí vienen. `docs/protocolo.md` documenta `Text` y la prueba del documento lo exige.
8. **A mano** (antes del empaquetado, con el resto de pruebas manuales): `crisol doctor` sale en español en un Windows en español y en inglés con `CRISOL_LANG=en`. Las tildes se ven bien en conhost y en Windows Terminal (en conhost, con `NO_COLOR` borrada, porque Claude Code la define). La ventana en español no muestra ninguna frase del núcleo en inglés en Instalación, Diagnóstico ni en los errores.

## Consecuencias

- Cada mensaje nuevo del núcleo lleva su clave en todos los diccionarios: la prueba 1 lo exige y la prueba 2 obliga a usar `msg!`. Escribir una frase lleva un poco más de trabajo, pero no se puede olvidar una traducción.
- La salida en español de la CLI deja de coincidir con la de los issues en inglés. Para eso está `CRISOL_LANG=en`, y `--json` no cambia.
- El protocolo crece con campos opcionales, en la misma versión. Un daemon viejo con un cliente nuevo da inglés, como hoy.
- **Riesgo:** con `crisol doctor > archivo` o `| findstr`, Windows PowerShell 5.1 lee la salida del programa con la página de códigos de la consola (`[Console]::OutputEncoding`, OEM) y no como UTF-8, así que las tildes llegan mal. No es nuevo (las rutas ya podían llevar tildes), pero ahora se nota más. F13.10c lo comprueba a mano y lo documenta junto a `CRISOL_LANG=en`. La terminal no se ve afectada, porque escribe con la API de consola.

## Alternativas descartadas

- **Traducir en el daemon** según el idioma del cliente (`hello` con un idioma). Se descartó porque obliga al daemon a llevar el idioma por conexión y a traducir eventos para varios clientes. Además, la traducción no se puede probar sin daemon.
- **Fluent o gettext** (`fluent`, `gettext-rs`). Hacen bien los plurales y la gramática, pero son otro formato y otra herramienta para quien traduce. Además, la ventana ya usa JSON plano. Con inglés y español, `_one`/`_other` alcanza. Si llega un idioma que lo necesite, se puede revisar.
- **Mandar claves en lugar del texto inglés.** Rompería a los clientes de hoy y `--json`.
- **Un solo diccionario para la ventana y el núcleo.** El daemon y la CLI cargarían textos de botones que nunca usan, y las claves chocarían (`doctor.*` existe en los dos).
