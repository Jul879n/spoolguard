# Extensión de navegador - SpoolGuard

Bloquea `window.print()` y Ctrl/Cmd+P en los sitios definidos en las reglas de la app. Manifest V3 (Chrome/Edge y Firefox >= 128). Es opcional: solo hace falta para reglas por sitio web.

## Cómo funciona
- `background.js` pide las reglas al host nativo `cl.cohasa.controlimpresion` cada minuto (alarma) y las guarda en `chrome.storage.local`. Si el host no responde, se conserva la última caché.
- `content.js` (+ `page.js` en el mundo de la página) corre en `document_start` en todas las URLs. Si el dominio (o subdominio) está en `block`, o no está en `allow_only` (cuando no es `null`), anula `window.print`, intercepta Ctrl/Cmd+P, oculta la página al imprimir y muestra "Impresión bloqueada por política".
- Agentes locales de impresión (QZ Tray, Zebra Browser Print): `page.js` envuelve `WebSocket`, `fetch` y `XMLHttpRequest.open`. En sitios bloqueados corta las conexiones a `localhost`/`127.0.0.1`/`[::1]`/`localhost.qz.io` en los puertos de QZ Tray (wss 8181/8282/8383/8484, ws 8182/8283/8384/8485; `Constants.java` y `qz-tray.js` oficiales de qzind/tray) y de Zebra Browser Print (9100/9101), cierra los sockets ya abiertos y muestra el aviso. En cualquier sitio http(s), un `send()` de QZ con `call:"print"` informa al host solo origen y título con `kind:"agent"` (máx. 1 cada 2 s). Limitación: si la página abre el socket antes de que carguen las reglas, se cierra al activarse el bloqueo; no cubre service workers ni iframes de otro origen que la extensión no inyecte.
- Cuando un sitio permitido imprime, la extensión avisa al host (dominio y título, máximo 1 aviso cada 2 s) para asociarlo al trabajo de la cola.
- **Host de Native Messaging = el mismo ejecutable de la app** lanzado con `--native-host` (`src-tauri/src/main.rs`). No hay un binario aparte. Lee `config.json` del directorio de datos de la app; `CONTROL_IMPRESION_DIR` lo sobreescribe.

## Modos de conexión
Un solo código con dos proveedores en `background.js`; se elige en el popup.
- **Escritorio** (por defecto): host nativo de la app, como se describe arriba.
- **Servidor**: URL del `control-server` y token de extensión (se crea en `https://servidor/tokens.html`, ver `docs/modo-servidor.md`). Recibe las reglas al instante por un flujo SSE (`GET /ext/events`) y, de respaldo, consulta `GET /ext/rules` cada 15 s mientras el service worker esté activo (más una alarma de 30 s que lo despierta; Chrome anterior a 120 la lleva a 1 min) y envía `POST /ext/intent`. Si la conexión falla conserva la última caché y reintenta con espera creciente (hasta 2 min). Si hay servidor configurado y elegido se usa; si no, el host nativo.
- Permisos: `optional_host_permissions` (`https://*/*`, `http://*/*`); al pulsar "Guardar y conectar" el popup pide solo el host del servidor (`chrome.permissions.request`, requiere el gesto del usuario). La URL y el token quedan en `storage.local`.

## Sin interruptores
El popup solo informa: la política (bloqueo de `window.print`, Ctrl/Cmd+P, `@media print` y de QZ Tray/Zebra) la dictan las reglas de la app o del servidor. Si se desbloquea un sitio cuya conexión a un agente local se había cortado, la página muestra «Recargar» (qz-tray.js no reconecta solo).

## Instalación (uso normal)

El registro del host es automático: al iniciar, la app (`src-tauri/src/browser.rs`):

1. Copia la carpeta `extension/` al directorio de datos de la app (macOS: `~/Library/Application Support/cl.cohasa.controlimpresion/extension`).
2. Crea el envoltorio `native_host.sh` (macOS/Linux) o `native_host.bat` (Windows), que ejecuta la app con `--native-host`.
3. Registra el manifiesto del host, a nivel de usuario, en los navegadores que encuentra instalados: Chrome, Chromium, Edge y Firefox (macOS/Linux: archivo en `NativeMessagingHosts`; Windows: clave en `HKCU`).

Lo único manual, **una sola vez en Chrome/Chromium/Edge**, es cargar la extensión. Camino corto: en Ajustes > Extensión de navegador, "Instalar en navegador…" (elige navegador y perfil, abre la página de extensiones y copia la ruta; en Chrome/Edge falta igualmente el paso manual "Cargar descomprimida"; si el navegador está administrado por la organización puede no abrirse solo, entonces usar la ruta copiada). "Descargar extensión (.zip)" guarda la carpeta como `.zip`. Camino manual:

1. En Ajustes > Extensión de navegador, pulsar "Abrir carpeta de la extensión" y "Abrir chrome://extensions" (la app muestra la carpeta, los navegadores registrados y si el host ya respondió).
2. Activar "Modo de desarrollador" > "Cargar descomprimida" > elegir esa carpeta.

Los íconos (`icons/icon-16/32/48/128.png`) salen de `app-icon.png` (recortado al cuadrado azul, fondo transparente) y se referencian en `icons` y `action.default_icon` de ambos manifiestos.

El ID de la extensión es fijo, `afjahbecgkkpbenpmgelpiceakiolikp` (sale de la `key` de `manifest.json`), y es el que el manifiesto del host autoriza; no hay que copiarlo ni registrarlo a mano. Alternativa: `extension-navegador.zip` del release contiene la misma carpeta.

Instalación manual (solo desarrollo): `native-host/install-native-host.sh` (macOS/Linux) o `native-host\install-native-host.ps1` (Windows) registran un host indicando su ruta y el ID de la extensión.

## Firefox
Firefox usa `background.scripts` en lugar de `service_worker`: para cargarla allí hay que usar `manifest.firefox.json` en lugar de `manifest.json` (sin `key`). "Instalar en navegador…" abre `about:debugging#/runtime/this-firefox` y copia la ruta de `manifest.firefox.json`; en "Este Firefox" > "Cargar complemento temporal…" se elige ese archivo. La carga es temporal: se pierde al cerrar Firefox. ID de Firefox: `controlimpresion@cohasa.cl`. La app registra el host también para Firefox, pero esta ruta no está probada.

## Sitios en las reglas
No distinguen mayúsculas y `ejemplo.cl` cubre también sus subdominios. Al enviar las reglas a la extensión el host (`native_host.rs`) solo pasa a minúsculas, quita espacios, puntos de los extremos y un `*.` inicial, y elimina repetidos. La coincidencia del lado de la app (`core/src/rules.rs`) además acepta URLs completas (descarta esquema, ruta y puerto). Para que la extensión también las reconozca, escribir los sitios como dominio (`ejemplo.cl` o `*.ejemplo.cl`): una URL completa no se recorta antes de llegar a la extensión. La regla global se suma a las de cada impresora: sus sitios entran en la lista de bloqueo o de solo permitir (esta última existe si alguna regla activa está en modo permitir).

## Popup
El ícono de la barra abre un popup con el estado de la conexión con la app ("Conectada" o "Sin conexión", con la última comunicación; sin conexión usa las últimas reglas cacheadas), si el sitio de la pestaña activa permite o bloquea imprimir (misma lógica que `content.js`), las reglas activas y el botón "Actualizar reglas". Usa el permiso `activeTab` para leer la URL de la pestaña activa sin pedir `host_permissions`. El service worker guarda el resultado de cada intento en `storage.local.estado` y muestra una insignia en el ícono (verde "✓" conectada, roja "!" sin conexión).

## Limitaciones
- Es una barrera de cortesía: un usuario puede desactivar la extensión o imprimir con otras vías (p. ej. capturas, guardar como PDF desde otro lugar).
- Solo se instala a nivel de usuario.
- Probado en Chrome 157 (perfil aislado, cargada por CDP): bloqueo de `window.print`, aviso con Ctrl+P, `@media print`, aviso de intención al host y conservación de caché. No probado: Edge, Firefox, el atajo físico Cmd/Ctrl+P ni la actualización en caliente de páginas abiertas.
- Chrome moderno ignora `--load-extension`; para pruebas automatizadas se usa `--enable-unsafe-extension-debugging` y `Extensions.loadUnpacked`.
- Si se registra un host manual fuera de la app, su binario debe estar fuera de carpetas protegidas por macOS (Documents, Desktop, Downloads): Chrome lo lanza sin esos permisos y falla con "Operation not permitted".

## Actualización

El botón **Buscar actualización** de la tarjeta "Conexión" del popup compara la versión que corre con la de la app (modo escritorio: "Actualizar" copia los archivos y recarga la extensión) o con la del servidor (modo servidor: "Descargar vX" baja el zip, verifica su SHA-256 y hay que reemplazar la carpeta y pulsar Recargar). Detalle en `docs/modo-servidor.md`.
