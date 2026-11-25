# Instalar la extensión de navegador

Guía paso a paso para instalar la extensión de navegador de **SpoolGuard**. No hace falta saber de informática: solo seguir los pasos en orden.

Las marcas **(no verificado)** indican datos que no se pudieron comprobar en el código ni en pruebas; están reunidas en la sección 11.

## 1. ¿Para qué sirve la extensión?

La extensión es opcional, pero necesaria si quiere controlar la impresión **por sitio web** (por ejemplo, impedir imprimir desde `ejemplo.cl`).

- **Con la extensión:** cuando alguien intenta imprimir desde un sitio bloqueado (con el botón de la página o con Ctrl+P / Cmd+P), el navegador muestra "Impresión bloqueada por política" y no se envía nada a la impresora. Además, cuando se imprime desde un sitio permitido, la extensión avisa a la app qué sitio fue, para anotarlo en el registro.
- **Sin la extensión:** la app solo puede reconocer el sitio si el **título de la página** incluye el dominio. Muchas páginas no lo incluyen, así que las reglas por sitio fallan con frecuencia.

Las reglas por impresora, por usuario u otras no necesitan la extensión.

## 2. Antes de empezar

1. SpoolGuard debe estar **instalado y abierto** en el equipo (ver `docs/instalacion.md`). Al abrirse, la app copia la extensión a su carpeta de datos y registra la conexión con Chrome, Chromium, Edge y Firefox si están instalados.
2. Abra la app y vaya a **Ajustes → Extensión de navegador**. Allí verá:
   - **Estado** de la extensión.
   - Los navegadores donde quedó registrada la conexión.
   - La carpeta de la extensión.
   - Los botones **Abrir carpeta de la extensión**, **Abrir chrome://extensions**, **Descargar extensión (.zip)** e **Instalar en navegador…**.
3. Cierre ventanas que no necesite del navegador y, si usa varios perfiles, sepa en cuál imprime (ver sección 5).

La carpeta de la extensión está dentro de los datos de la app:

| Sistema | Carpeta |
|---|---|
| macOS | `~/Library/Application Support/cl.cohasa.controlimpresion/extension` |
| Windows | `%APPDATA%\cl.cohasa.controlimpresion\extension` |
| Linux | `~/.local/share/cl.cohasa.controlimpresion/extension` |

El identificador `cl.cohasa.controlimpresion` está en `src-tauri/tauri.conf.json`; la app usa el directorio de datos estándar de su framework (Tauri). Las rutas de Windows y Linux salen de esa convención y **no se probaron en esos sistemas (no verificado)**. La ruta exacta siempre aparece en Ajustes → Extensión de navegador.

## Modo Servidor (sin la app de escritorio en el equipo)

La extensión tiene dos modos, que se eligen en su popup (ícono de la barra):

- **Escritorio**: usa la app instalada en el mismo equipo (todo lo anterior).
- **Servidor**: usa el servidor de SpoolGuard de la red (ver `docs/modo-servidor.md`). No requiere la app en ese equipo.

Para el modo Servidor:

1. El administrador abre `https://servidor:8080/tokens.html`, inicia sesión y pulsa **Crear token** (uno por equipo, con un nombre). El token se muestra **una sola vez**: copiarlo. Puede revocarse desde la misma página.
2. En el equipo, instalar la extensión (secciones siguientes; no hace falta que la app esté instalada), abrir su popup y elegir **Servidor**.
3. Escribir la dirección del servidor y pegar el token. Pulsar **Guardar y conectar**: el navegador pide permiso para comunicarse con ese servidor (solo ese sitio). Aceptar.
4. El popup debe mostrar "Conectado con el servidor". Si dice "Token inválido o revocado", crear otro token; si dice "No se pudo conectar de forma segura", el certificado HTTPS del servidor no es confiable para el navegador (instalar su CA en el equipo) o el servidor es inalcanzable.

El token no es la contraseña de administrador y solo permite leer las reglas de sitios y avisar intenciones de impresión. Con `http://` (sin HTTPS) viaja sin cifrar: usar HTTPS. Los tres interruptores del popup (bloquear impresión de la página, bloquear QZ Tray/Zebra, registrar intenciones) valen en ambos modos y se aplican al instante. Estado de la verificación: el flujo del servidor se probó con curl; la carga real en Chrome/Firefox del modo Servidor **no está verificada (no verificado)**.

## 3. Método A (recomendado): botón "Instalar en navegador…"

1. En **Ajustes → Extensión de navegador**, pulse **Instalar en navegador…**.
2. En el diálogo, elija el **Navegador** (aparecen solo los detectados: Chrome, Edge, Chromium, Brave o Firefox). Si el navegador tiene varios perfiles, elija también el **Perfil** (se muestra el nombre y el correo).
3. Pulse **Abrir y copiar ruta**. La app hace tres cosas:
   - Se asegura de que la carpeta de la extensión esté al día.
   - Abre la página de extensiones de ese navegador y perfil (`chrome://extensions`, `edge://extensions`, etc.).
   - **Copia la ruta** de la carpeta al portapapeles (aparece el aviso "Ruta copiada"). Si no puede copiarla, la muestra en un cuadro para que la copie a mano.
4. Siga los pasos que muestra el diálogo (son los de las secciones 4, 6 o 7 según el navegador). **La app no puede terminar la instalación sola**: el último paso, "Cargar descomprimida", es manual.
5. Si el navegador ya estaba abierto, la página puede abrirse en su último perfil activo (el diálogo lo avisa). Si dice que no pudo abrir el navegador, ábralo usted y vaya a la página de extensiones; la ruta ya está copiada.

Alternativas: **Abrir carpeta de la extensión** abre la carpeta en el explorador de archivos; **Abrir chrome://extensions** abre esa página en Chrome; **Descargar extensión (.zip)** guarda la carpeta comprimida (por defecto `control-impresion-extension.zip`) para llevarla a otro equipo; descomprímala y cargue la carpeta resultante.

## 4. Chrome (macOS, Windows y Linux)

1. Abra Chrome en el perfil donde imprime. Escriba `chrome://extensions` en la barra de direcciones y pulse Enter (o use el Método A).
2. Arriba a la derecha, active **Modo de desarrollador**.
3. Pulse **Cargar descomprimida** (aparece al activar el modo).
4. Se abre un selector de carpetas. Indique la carpeta de la extensión (la ruta de la sección 2, o la que copió el Método A):
   - **macOS:** en el selector pulse **Cmd+Shift+G**, pegue la ruta con **Cmd+V**, pulse Enter y luego **Abrir** o **Seleccionar**.
   - **Windows:** haga clic en la **barra de direcciones** del selector (arriba), pegue la ruta con **Ctrl+V**, pulse Enter y luego **Seleccionar carpeta**. Si copia `%APPDATA%\...` a mano, el selector también la entiende.
   - **Linux:** pulse **Ctrl+L** en el selector, pegue la ruta con **Ctrl+V**, pulse Enter y confirme. Las carpetas que empiezan con punto (`.local`) están ocultas; pegar la ruta evita buscarlas.
5. Debe aparecer la tarjeta **SpoolGuard** con el ID `afjahbecgkkpbenpmgelpiceakiolikp`, activada.
6. **Fije el ícono:** pulse el ícono de puzzle (**Extensiones**) de la barra de herramientas y luego el alfiler junto a "SpoolGuard".
7. **Verifique:** el ícono debe mostrar una insignia verde **✓**. Al pulsarlo, el popup muestra **"Conectada con la app"**. Si muestra una insignia roja **!** o "Sin conexión con la app", vea la sección 9.

La insignia puede tardar unos segundos; las reglas se actualizan cada minuto (el popup tiene el botón **Actualizar reglas**).

## 5. Varios perfiles de Chrome

Una extensión se instala **por perfil**. Si en el equipo se usan varios perfiles de Chrome para imprimir, repita la sección 4 **en cada uno**.

- Para saber en qué perfil está: mire el **ícono de avatar** (la foto o inicial) a la izquierda del menú de tres puntos, arriba a la derecha. Al pulsarlo se ven los demás perfiles y puede abrirlos.
- Con el Método A, elija cada vez el perfil correspondiente en el diálogo.
- Un perfil sin la extensión no tiene control por sitio web.

## 6. Edge, Brave y Chromium

Los pasos son los mismos de la sección 4; solo cambia la dirección:

| Navegador | Página de extensiones |
|---|---|
| Edge | `edge://extensions` |
| Brave | `brave://extensions` |
| Chromium | `chrome://extensions` |

En Edge, el botón de modo de desarrollador se llama **Modo de desarrollador** y está en el panel izquierdo; después pulse **Cargar descomprimida** (nombres en español: no verificado en Edge, que no se ha probado con esta extensión).

Brave: el registro automático de la conexión con la app (host nativo) que hace SpoolGuard cubre Chrome, Chromium, Edge y Firefox; en el código no aparece Brave en ese registro. **Es posible que en Brave el popup diga "Sin conexión" (no verificado)**; si ocurre, vea la sección 9 y avise al responsable.

## 7. Firefox

1. Abra Firefox y escriba `about:debugging` (o use el Método A, que abre `about:debugging#/runtime/this-firefox`).
2. En el menú izquierdo, pulse **Este Firefox**.
3. Pulse **Cargar complemento temporal…**.
4. En el selector, elija el archivo **`manifest.firefox.json`** dentro de la carpeta de la extensión (no `manifest.json`). El Método A copia su ruta completa.
5. Firefox necesita la versión 140 o superior (el manifiesto de Firefox así lo declara).

Limitaciones:

- La carga es **temporal**: se pierde al cerrar Firefox y hay que repetirla en cada sesión.
- Para que sea permanente, Firefox exige que la extensión esté **firmada** por Mozilla. Esa firma **está pendiente**.
- La instalación en Firefox **no está probada** (no verificado).

## 8. Navegador administrado por la organización

Si en `chrome://extensions` o en el menú de Chrome aparece **"Administrado por tu organización"**, la empresa o el colegio controla el navegador mediante políticas. Puede ocurrir que:

- el **Modo de desarrollador** esté bloqueado o no aparezca;
- no deje cargar la extensión, o la desactive;
- la página de extensiones no se abra desde la app (ábrala a mano y use la ruta copiada).

**Qué hacer:** pida al equipo de TI que permita la extensión por política, indicándole su ID: `afjahbecgkkpbenpmgelpiceakiolikp`. Las políticas de Chrome relevantes (para administradores) son `ExtensionInstallAllowlist` (permitir) y `ExtensionInstallForcelist` (instalar a la fuerza), además de `DeveloperToolsAvailability` / `ExtensionDeveloperModeSettings` si el modo de desarrollador está bloqueado. Referencia oficial: https://developer.chrome.com/docs/extensions/how-to/distribute/install-extensions

Por qué la instalación es manual: en equipos Windows y macOS **no gestionados**, Chrome ignora las extensiones instaladas desde fuera de la Chrome Web Store (según esa documentación). La extensión de SpoolGuard no se publica en la Web Store, por lo que se carga como "descomprimida". En equipos gestionados, la instalación forzada por política requiere que la extensión esté alojada en un servidor de actualizaciones o en la Web Store; **la app no incluye hoy ese servidor ni un archivo de política listo para usar (no verificado / pendiente)**.

## 9. Solución de problemas

**El popup dice "Sin conexión con la app".**
1. Abra SpoolGuard: la conexión se registra al abrir la app.
2. Cierre **todas** las ventanas del navegador y ábralo de nuevo (el navegador lee el registro al iniciar).
3. En `chrome://extensions` verifique que el ID de la extensión sea `afjahbecgkkpbenpmgelpiceakiolikp`. Si es otro, se cargó una copia distinta (por ejemplo, otra carpeta sin la `key` del manifiesto): quítela y cargue la carpeta de la extensión de la app.
4. En Ajustes → Extensión de navegador, compruebe que su navegador figure entre los registrados y que el estado indique que el host ya respondió.
5. Mientras no haya conexión, la extensión sigue usando las últimas reglas guardadas.

**Aviso "Esta extensión puede leer y cambiar datos del sitio".** Es normal. La extensión necesita ejecutar un pequeño script en todas las páginas (`http` y `https`) para poder bloquear la impresión antes de que la página lo haga. Solo compara el dominio con las reglas. No envía contenido de las páginas fuera del equipo: se comunica únicamente con la app instalada en el mismo equipo (dominio y título al imprimir desde un sitio permitido).

**La extensión se desactiva tras reiniciar.** En Windows, Chrome puede mostrar al iniciar "Desactivar las extensiones en modo de desarrollador". Pulse **Cancelar** (o **Mantener**, según la versión) para conservarlas. El texto exacto de esos botones puede variar entre versiones (no verificado). Si ya se desactivó, vuelva a activarla en `chrome://extensions`.

**Actualizar la extensión.** Al abrir una versión nueva de la app, esta copia la carpeta actualizada. Después, en `chrome://extensions`, pulse **Recargar** (flecha circular) en la tarjeta de SpoolGuard. Si instaló desde un `.zip` en otro equipo, reemplace la carpeta y recargue.

## 10. Comprobar que funciona

1. En SpoolGuard cree una regla que bloquee un sitio de prueba (por ejemplo `ejemplo.cl`), y espere hasta 1 minuto o pulse **Actualizar reglas** en el popup.
2. Abra ese sitio en el navegador.
3. Pulse **Ctrl+P** (Windows/Linux) o **Cmd+P** (macOS), o use el botón de imprimir de la página.
4. Debe aparecer **"Impresión bloqueada por política"**. En el popup, "Este sitio" indica "Impresión bloqueada".

Las páginas que no son `http` o `https` (por ejemplo `chrome://`) no se controlan. La extensión es una barrera de cortesía: un usuario con permisos puede desactivarla.

## Resumen por navegador

| Navegador | Página | Pasos | Estado |
|---|---|---|---|
| Chrome | `chrome://extensions` | Modo de desarrollador → Cargar descomprimida → carpeta | Probado en Chrome 157 (macOS) |
| Edge | `edge://extensions` | Igual que Chrome | Sin probar |
| Brave | `brave://extensions` | Igual que Chrome | Sin probar; registro de la conexión no confirmado |
| Chromium | `chrome://extensions` | Igual que Chrome | Sin probar |
| Firefox | `about:debugging` | Este Firefox → Cargar complemento temporal… → `manifest.firefox.json` | Temporal; sin probar; firma pendiente |

## Lista final de comprobación

- [ ] SpoolGuard está instalado y abierto.
- [ ] Cargué la carpeta de la extensión en cada navegador y perfil donde se imprime.
- [ ] En `chrome://extensions` el ID es `afjahbecgkkpbenpmgelpiceakiolikp` y la extensión está activada.
- [ ] El ícono está fijado y muestra la insignia verde ✓.
- [ ] El popup dice "Conectada con la app".
- [ ] Un sitio bloqueado muestra "Impresión bloqueada por política" al pulsar Ctrl/Cmd+P.
- [ ] En equipos administrados, TI permitió el ID de la extensión.
- [ ] En Firefox sé que debo volver a cargarla cada vez que se cierre.

## 11. Puntos no verificados

- Rutas de datos en Windows y Linux (deducidas del identificador y del directorio estándar de la app).
- Edge, Brave, Chromium y Firefox: sin pruebas con la extensión.
- Registro automático de la conexión en Brave (el código no lo incluye).
- Nombres exactos de menús y botones de Edge y de los avisos de Chrome en Windows ("Desactivar extensiones en modo de desarrollador").
- Instalación por política en equipos gestionados (sin servidor de actualizaciones ni plantilla en el repositorio).
- Firma de Firefox para instalación permanente: pendiente.
