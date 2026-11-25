# Modo servidor (VPS, Proxmox, Docker)

SpoolGuard para **impresoras compartidas por red**, sin instalar la app en cada PC. Estado: plan aprobado, en implementación.

## Objetivo principal

Desde **cualquier equipo** de la red (PC, notebook, celular) se administra el control de una impresora conectada a la red, y se puede **bloquear a cualquier otro equipo**, sin instalar nada en los PC bloqueados.

- **Administrar desde cualquier equipo:** panel web del servidor (`https://servidor:8080`), con usuario y contraseña. Basta un navegador.
- **Bloqueo rápido por equipo:** botón "Bloquear equipo" (IP o nombre) y "Bloquear todo en esta impresora" / "Pausar impresora", con efecto inmediato y registro de quién lo hizo.
- **Bloqueo real:** los trabajos solo llegan a la impresora a través del servidor. Un equipo que imprima directo a la IP de la impresora se salta el control, por eso el paso obligatorio de la instalación es **restringir la impresora a la IP del servidor** (filtro de IP del panel de la impresora, o firewall/VLAN en 9100, 515 y 631). La guía incluye una comprobación: desde otro equipo, imprimir directo a `IP:9100` debe fallar.
- **Equipos que se agregan al servidor:** cada PC instala la cola del servidor (IPP `ipp://servidor:631/printers/<nombre>`); la guía incluye comandos para Windows, macOS y Linux.

## Cómo funciona

```
PC / celulares ──IPP──▶ Servidor (CUPS + control-server) ──9100/IPP──▶ Impresora de red
                              │
                              └── Panel web (HTTPS) para reglas y registro
```

- Los PC imprimen a la cola del servidor (IPP). La impresora solo acepta conexiones del servidor.
- Cada cola con control queda **retenida** (`job-hold-until-default=indefinite` y `Hold-New-Jobs`).
- El daemon lee cada trabajo, aplica las reglas y lo **libera** o lo **cancela**.
- Todo se registra (quién, desde qué IP, qué documento, decisión y motivo).

## Qué se puede evaluar (verificado en `core/src/cups.rs`)

CUPS entrega solo: usuario, IP/equipo de origen y nombre del documento.

**Windows (app de escritorio, spooler).** El spooler entrega usuario, nombre del documento y el *nombre* del equipo que envió el trabajo
(`pMachineName`), no su IP. La app resuelve ese nombre a IP (DNS/NetBIOS del sistema, 1 s de límite, con caché de 10 min si resolvió y 2 min si
no) al evaluar, nunca en el vigilante de la cola, y la regla "IP" admite ambas formas: una IP o CIDR (`192.168.1.0/24`) contra la IP resuelta, o
un nombre de equipo (`PC-VENTAS`, sin distinguir mayúsculas, nombre completo, no subcadena) contra el equipo de origen. Límites: los trabajos
enviados desde el propio PC no traen equipo remoto (no tienen IP de origen y no coinciden con reglas por IP ni por nombre); el nombre lo informa
el cliente y no está autenticado; un nombre que no resuelve (NetBIOS sin DNS) solo coincide con una regla por nombre, no por IP/CIDR.

| Regla | En servidor |
|---|---|
| IP / equipos | Sí |
| Usuario | Sí |
| Aplicación | Solo si aparece en el nombre del documento (por ejemplo `*.docx`) |
| Sitio web | No: la extensión corre en cada PC y no ve la cola del servidor |

## Cobertura por vía de conexión

No existe forma de interceptar absolutamente todo. Donde hay una cola del sistema operativo (spooler o CUPS) se intercepta; donde el trabajo viaja directo a la impresora, solo se controla restringiendo la red y las funciones de la propia impresora.

| Vía | La cubre | Cómo se salta |
|---|---|---|
| USB, desde el PC que la tiene | App de escritorio (cola del SO) | Software que habla USB directo (utilidades del fabricante, algunos drivers de etiquetas) |
| Red (IP, WiFi, Ethernet) compartida a otros PC | Servidor, con la impresora restringida a su IP | Cualquier equipo que alcance `IP:9100` si no hay filtro o firewall |
| Impresora compartida desde un PC con la app | App de escritorio de ese PC (filtro por IP de origen) | Imprimir directo a la IP de la impresora |
| Celulares, AirPrint, Mopria | Servidor, si la impresora solo acepta al servidor | Impresión directa si la impresora acepta conexiones directas |
| QZ Tray y similares | App con la extensión | Equipos sin la extensión |
| Panel de la impresora, memoria USB, correo a impresora, WiFi Direct | Nada de software: se desactivan en la propia impresora | — |
| Puerto serial o paralelo | Nada: no pasa por una cola del SO | — |

Lista de verificación por impresora: desactivar USB, WiFi Direct, impresión por correo y AirPrint si no se usan; poner contraseña al panel de la impresora; aceptar conexiones solo desde la IP del servidor; probar desde otro equipo que `IP:9100` rechaza.

## Inventario y auditoría

Estado: **implementado** en servidor y escritorio, con las limitaciones que se indican en cada punto. Objetivo: saber quién imprimió qué, por qué puerto y desde dónde.

### Datos por trabajo (puerto, conexión, páginas, bytes)

Cada evento de impresión guarda ahora la salida de la cola y, solo cuando el sistema lo entrega, el tamaño. Lo que no se sabe queda vacío (nunca se inventa).

| Campo | CUPS (servidor y escritorio macOS/Linux) | Windows (escritorio) |
|---|---|---|
| Puerto / URI de salida | `device-uri` de la cola, sin usuario, contraseña ni consulta (`socket://10.0.0.5:9100`, `usb://Marca/Modelo`). Se recuerda 60 s por cola. | Puerto del spooler (`USB001`, `IP_10.0.0.5`, `WSD-…`, `PORTPROMPT:`…) |
| Tipo de conexión | `usb`, `socket`, `ipp`, `lpd`, `red` (dnssd/mdns: no se sabe el protocolo), `compartida` (smb), `archivo` (file, cups-pdf), `local` (serie/paralelo), `desconocido` | `usb`, `wsd`, `ipp`, `compartida`, `archivo`, `local`; los puertos TCP/IP estándar se distinguen (`socket` 9100, `lpd` 515, `ipp` 631) solo si Windows informa el número de puerto, si no `red` |
| Páginas | `job-impressions`. **Con CUPS casi siempre llega vacío**: el trabajo está retenido y CUPS aún no lo ha filtrado, así que informa 0 (se trata como desconocido). | `TotalPages` del spooler (0 = desconocido) |
| Bytes | `job-k-octets` × 1024 (CUPS lo da en KiB: es aproximado y se redondea hacia arriba) | No se obtiene (`Size` solo está en `JOB_INFO_2`, no se consulta) |

Verificado con una cola CUPS real en macOS (`socket://127.0.0.1:…` hacia una impresora simulada y `file:///dev/null`): puerto, tipo y bytes se rellenan; las páginas quedan vacías. **No se probó en Windows** (la VM no se tocó en esta etapa): la clasificación de puertos y hosts de Windows está probada con datos simulados, pero el código específico de Windows (`TotalPages`, la consulta de `Get-PrinterPort`/`Get-Printer`) no se pudo compilar ni ejecutar en esta máquina (macOS) y queda sin verificar.

### Inventario de impresoras y puertos

Vista **Inventario** (servidor y escritorio) con tabla y exportación a CSV. Estructura estable por impresora: nombre, driver/modelo, puerto/URI, conexión, IP/host del dispositivo si es de red, compartida (sí/no/no informado), estado, retención soportada y control activo. Comandos: `get_inventory` (servidor y escritorio). La misma estructura alimenta la categoría **"inventario" de la copia de seguridad** (`name`, `kind`, `uri` como siempre, más los campos nuevos solo si se conocen; al importar sigue siendo informativa y los vectores de la copia no cambian). Fuentes: CUPS `Get-Printer-Attributes` (`device-uri`, `printer-make-and-model`, `printer-is-shared`); Windows, `Get-Printer` y `Get-PrinterPort`. El URI se muestra y exporta sin credenciales. El CSV del inventario, el de "Impresoras en la red" y el de "Quién imprimió qué" se generan en la interfaz con la misma neutralización de fórmulas (`=`, `+`, `-`, `@`) que el CSV de eventos.

### Descubrimiento de red (opcional)

Comando `discover_network` (servidor y escritorio), sección "Impresoras en la red" de la vista Inventario. **No hace nada por sí solo**: solo prueba las subredes que el administrador escribe y guarda (por defecto, ninguna) cuando pulsa "Buscar".

- Límites: IPv4; cada rango entre `/24` y `/32` (un `/16` o `/8` se rechaza); máximo 512 hosts por búsqueda, 16 rangos; 8 conexiones a la vez; 500 ms por conexión; solo conexión TCP a 9100, 515 y 631 (sin SNMP, sin enviar datos de impresión, sin exploits). Si el 631 responde, una consulta IPP `Get-Printer-Attributes` para leer modelo, nombre y estado.
- Rangos públicos (no privados, loopback ni link-local) y reservados: los públicos se rechazan salvo confirmación explícita (casilla en la interfaz, `confirm_public`); los reservados (0.x, multicast, 240+) se rechazan siempre.
- Se ejecuta en segundo plano (`discover_status` consulta el avance, `discover_cancel` lo interrumpe) y hay una sola a la vez; tiempo máximo 5 minutos en el servidor.
- Cada dispositivo se compara con las colas del sistema por IP (resolviendo nombres de host): **controlada** (cola con control activo), **cola sin control** (existe la cola pero el control está apagado) o **sin cola** (riesgo de impresión directa). Cada fila lleva una explicación en español. Incluso una impresora "controlada" sigue aceptando conexiones directas: por eso hay que aplicar la lista de verificación de arriba.
- Queda anotado en el Registro quién la inició (`admin@IP` en el servidor; usuario del sistema en escritorio) y el rango, al empezar y al terminar.
- Los rangos se guardan en `network.json` (carpeta de datos). Variable `CONTROL_DISCOVER_PORTS=raw,lpd,ipp` (solo servidor): **solo para pruebas locales**, cambia los tres puertos; por defecto `9100,515,631`.
- Probado con listeners falsos en `127.0.0.1` y una impresora IPP simulada; **no se probó contra impresoras reales ni contra una red real**. Una impresora que no responda IPP aparece sin identificar y el aviso pide verificarla (puede ser otro tipo de dispositivo).

### Detección de impresión directa (solo servidor, mejor esfuerzo, apagada por defecto)

Se activa en Inventario → "Detección de impresión directa" (intervalo configurable de 30 a 86400 s, por defecto 300; también `set_network_settings`). Consulta por IPP sin cifrar a las impresoras cuya cola apunta a un host de red (`socket`, `ipp`, `lpd`; `ipps` y `smb` no) y compara con lo que el servidor liberó:

1. Fuente 1: el atributo `printer-impressions-completed` (Get-Printer-Attributes). Fuente 2, si la impresora no lo expone: los trabajos terminados que recuerda (Get-Jobs `completed`, `job-impressions-completed`, trabajos nuevos desde la última consulta).
2. La primera lectura solo fija la línea base (en memoria: reiniciar el servidor o desactivar la detección la pierde). Si el contador baja se vuelve a fijar sin alertar.
3. Si sube y el servidor no liberó trabajos de esa cola desde 2 minutos antes de la consulta anterior, se registra un evento "Impresión directa sospechosa" (impresora, aumento, hora; `decision` `notice`). Si el servidor sí liberó trabajos, no se alerta cuando alguno no tiene páginas conocidas (no se puede comparar; con CUPS es lo normal) o cuando el aumento no supera las páginas liberadas.
4. Una impresora que no responde o no expone ninguna fuente figura como **no verificable** en la tabla de estado y no se acusa a nadie.

Honestamente: **no se probó con hardware real**. Se probó con un transporte IPP inyectable y una impresora simulada (aumentos con y sin trabajos del servidor, reinicio del contador, historial de trabajos, impresora caída) y con un servidor real con CUPS en macOS contra esa impresora simulada. No consta que `printer-impressions-completed` exista en cada modelo; es posible que la mayoría solo exponga una de las fuentes o ninguna. Falsos positivos esperables: copias hechas en el panel de la impresora, impresión desde la memoria USB, trabajos de otra cola o servidor hacia la misma impresora. Falsos negativos: impresión directa mientras el servidor también libera trabajos sin páginas conocidas. No identifica **quién** imprimió; para eso hace falta el registro del firewall o de la propia impresora.

### Vista "Quién imprimió qué"

En Registro, pestaña **Quién imprimió qué** (servidor y escritorio; comando `summarize_events`): agrupa por usuario, equipo, impresora o decisión con trabajos, permitidos, bloqueados y páginas, respetando los filtros de la vista (estado, impresora, fechas y texto). "Ver eventos" salta a la pestaña Eventos filtrada por ese usuario o equipo (chip quitable). Los avisos del sistema no cuentan como trabajos. Las páginas solo suman las que el sistema informó; si algún trabajo no las trae el total se marca "(parcial)", y si ninguno las trae se muestra "—". El CSV de eventos ya incluía puerto, tipo de conexión, páginas y bytes (y neutraliza fórmulas); la tabla de eventos muestra además la salida y las páginas.

### Lo que sigue sin existir

- Impresoras por serial/paralelo o que nunca pasan por una cola del SO: no aparecen en el inventario ni en los eventos.
- Windows: descubrimiento de red e inventario no se ejecutaron en un Windows real; el tamaño en bytes no se obtiene.
- Quién imprimió directo (solo hay cuándo y cuántas páginas, si la impresora lo informa).

## Registro de eventos y base de datos

Estado: **implementado** (local en escritorio, central en servidor y envío opcional). Lo que falta está en "No hecho todavía" al final de esta sección.

Principio: **los datos se quedan en cada equipo**; la base centralizada es opcional.

| Situación | Dónde se guardan los eventos |
|---|---|
| Equipo con la app de escritorio, **sin servidor** configurado | Solo en una base **local** del propio equipo (`events.db` en la carpeta de datos). Nada sale del equipo. |
| Equipo con la app de escritorio **y** servidor configurado | Base local (siempre, también sin conexión) **y** envío al servidor. Si el servidor no responde, los eventos se acumulan en una cola local y se reenvían después, sin duplicados. |
| Servidor (Docker/VPS) | Base **central** (`<datos>/events.db`): sus propios eventos (CUPS) más los que envían los equipos. |

- **Motor:** SQLite embebido en ambos lados (`rusqlite` con SQLite incluido en escritorio, `modernc.org/sqlite` sin CGO en el servidor, que sigue siendo un binario estático). Modo WAL. `log.jsonl` ya no se escribe: al primer arranque se importa a la base y se **renombra** a `log.jsonl.migrado` (nunca se borra; si aparece otro `log.jsonl`, por ejemplo de una versión anterior, se importa aparte como `log.jsonl.migrado-<hora>`). La importación es idempotente aunque el proceso se interrumpa. El historial migrado no se envía al servidor.
- **Tabla `events`** (esquema versionado con `PRAGMA user_version`; una base de versión futura no se abre): `id` (UUID v7 generado en el equipo), `ts`, `host`, `ip` (origen del trabajo), `user`, `printer`, `port`, `conn_type`, `document`, `site`, `app`, `pages`, `bytes`, `decision` (`allow` | `block` | `notice`), `reason`, `status`, `source` (`desktop` | `server`). Escritorio añade `sent` (cola de envío: 0 pendiente, 1 enviado, 2 no se envía, 3 rechazado); el servidor añade `device` (etiqueta del token que lo envió, la pone el servidor) y `recv`. Índices por hora, usuario, impresora y decisión.
- **Retención:** escritorio, el ajuste "Retención del registro (días)" (limpieza al arrancar y cada hora). Servidor, `CONTROL_LOG_RETENTION_DAYS` (por defecto 365; `0` = sin límite; valor inválido = 365); el campo de retención del panel web se oculta porque ya no aplica. El borrado manual ("Borrar") vacía el registro completo en ambos.
- **Exportación CSV:** conserva las siete primeras columnas anteriores y añade equipo, puerto, tipo de conexión, sitio, aplicación, páginas, bytes, decisión, estado final, origen del evento e id. Respeta los filtros de la vista. Las celdas que empiezan por `=`, `+`, `-` o `@` se prefijan con `'` para que una hoja de cálculo no las ejecute.
- **Envío al servidor (opcional):** Ajustes → "Enviar eventos al servidor" (apagado por defecto), con dirección y **token de equipo**. Un hilo envía lotes de 100 a `POST /api/events/ingest` con `Authorization: Bearer`, marca como enviados **solo los ids que el servidor devuelve en `accepted`**, saca de la cola los `rejected` (inválidos) y reintenta con espera creciente (5 s, 10 s… hasta 5 min; respeta `Retry-After`; con token inválido espera al menos 60 s). Solo se envían los eventos posteriores a activar la opción. Cliente HTTP: `ureq` con `rustls` (el `reqwest` del árbol de Tauri no trae TLS); no sigue redirecciones para no reenviar el token a otro sitio. Con `http://` la app avisa que el token viaja sin cifrar.
- **Servidor, endpoint `POST /api/events/ingest`:** token de equipo `sgm_…` (distinto del de extensión `sgx_…`; un token de un tipo no sirve para el otro), guardado como SHA-256 en `ext_tokens.json`, creado y revocado en `tokens.html` con una etiqueta de equipo. Acepta hasta 500 eventos y 1 MiB por lote, 60 peticiones por minuto por token, límites por campo (id 64, textos cortos 255, documento 1000, motivo 500, hora entre 2001 y mañana, `decision` válida). Responde `{"accepted":[ids],"rejected":[ids]}`; guardar es idempotente por `id` (el primero que llega se conserva y los repetidos se responden como aceptados). Un token de equipo **no puede leer** eventos (el panel exige sesión de administrador).
- **Privacidad:** el envío está apagado por defecto y se activa con un interruptor en Ajustes que lista los campos enviados. Sin servidor configurado no se hace ninguna conexión de red: el cliente HTTP ni siquiera se construye mientras el envío esté apagado o no haya nada pendiente.
- **Seguridad de la base:** `events.db` (y sus `-wal`/`-shm`) con permisos 0600 en Unix; en Windows hereda la ACL de la carpeta de datos del usuario (no se verificó en esta etapa). `config.json` también se guarda 0600 en Unix porque puede contener el token. El servidor usa solo parámetros enlazados (los filtros de texto escapan `%` y `_`), con límites de tamaño por lote y por campo. El token de equipo se guarda en claro en `config.json` del equipo (hace falta para enviarlo); la interfaz nunca lo muestra de vuelta.
- **Interfaz:** la vista Registro lee de la base con filtros y paginación en el motor (estado, impresora, fechas desde/hasta, texto en documento, usuario, impresora, equipo, IP, sitio, aplicación y motivo). En el servidor muestra además la columna "Equipo" (etiqueta del token; en eventos propios, el nombre del servidor). Comandos nuevos: `query_events` (escritorio y servidor) y `get_sync_status` (escritorio); `get_activity` sigue existiendo (últimos 2000). Ver también "Inventario y auditoría" para `get_inventory`, `summarize_events` y el descubrimiento de red.
- **Pruebas:** Rust (`core/src/events.rs`): migración (incluida la interrumpida), idempotencia, cola offline y marcado de aceptados, retención, permisos 0600, filtros e inyección, CSV, backoff y que sin servidor no se construye ningún transporte. Go (`internal/app/events_test.go`): ingest idempotente, límites, token inválido/revocado/de otro tipo, frecuencia, filtros, retención, migración y permisos. Integración real (`core/tests/server_integration.rs`, se activa con `SPOOLGUARD_SERVER_BIN`): servidor Go local + cliente Rust, lote de 250 eventos, reenvío sin duplicados, token inválido y servidor caído con recuperación.

### No hecho todavía

- **Borrado manual protegido por contraseña en el servidor** y registro de quién borró; hoy "Borrar" exige solo la sesión de administrador (en escritorio, haber desbloqueado la app).
- **HTTPS con CA privada o certificado autofirmado:** el cliente usa los certificados raíz públicos incluidos (webpki); con una CA interna hay que usar un proxy con certificado público o `http://` en red de confianza.
- **Envío del historial anterior** a activar la opción y reintento manual desde la interfaz.
- **ACL de Windows** del archivo `events.db` y prueba en Windows (esta etapa se probó en macOS).
- **Rotación de `events.db`** (VACUUM): la retención borra filas, el archivo no se compacta solo.

## Exportar e importar

Una sola función de copia de seguridad, en escritorio y en el panel web del servidor, con **formato compartido** (`.spoolguard.json`). Ajustes → tarjeta **Copia de seguridad** → **Exportar…** / **Importar…**.

- **Formato** (`core/src/backup.rs` en Rust e `internal/app/backup.go` en Go, mismo esquema y validación): `{format:"spoolguard-backup", version:1, created, host, app_version, categories, reglas:{global, printers}, parametros, bloqueo_rapido, registro, inventario}`. Solo salen las categorías elegidas con casillas.
  - **Reglas y restricciones:** por impresora y globales (aplicaciones, sitios, IP/CIDR, modo).
  - **Parámetros (lista blanca):** `unknown_policy`, `interval_secs`, `notify_block`, `notify_allow`, `theme`, `retention_days`. La URL y el envío de eventos al servidor son del equipo y no viajan.
  - **Bloqueo rápido** (solo servidor): bloqueos y impresoras pausadas con quién y cuándo.
  - **Registro de eventos:** con rango de fechas y máximo de eventos (por defecto 10 000, tope 50 000).
  - **Inventario:** nombre, tipo y puerto de cada impresora (sin usuario ni clave del URI ni parámetros de consulta). Es informativo: al importar solo avisa de impresoras que no existen aquí.
- **Qué NO se exporta nunca:** contraseña, hash y sal, token de envío al servidor, tokens de extensión y de equipo, sesión. El archivo se arma desde una lista blanca de campos, no volcando la configuración. Tras importar en otro equipo hay que definir la contraseña y crear los tokens.
- **Importar con vista previa:** `preview_import(json, modo)` valida (formato, versión ≤ 1, tamaño máx. 10 MiB, anidamiento, campos conocidos y tipos, patrones IP/CIDR, dominios, extensiones, nombres de impresora) y devuelve un resumen sin aplicar nada: reglas nuevas/modificadas/eliminadas por impresora, ajustes que cambian, eventos a importar/duplicados/descartados, errores y avisos con motivo. Los errores bloquean aplicar (todo o nada); los campos desconocidos, sensibles (`password_*`, `*token*`, `session`…) o peligrosos (`__proto__`) se **ignoran** y se avisan.
- **Aplicar:** `apply_import(json, modo)` con modo `"combinar"`, `"reemplazar"` o un objeto por categoría (`{"reglas":"combinar","registro":"omitir"}`). *Combinar* une las listas y toma activación y modo del archivo; *reemplazar* sustituye (lo que no está en el archivo se elimina; los parámetros ausentes vuelven a su valor por defecto). Antes de aplicar guarda `config.json.bak` (y `quick.json.bak`); si algo falla deja todo como estaba. Los eventos se importan con los ids del archivo (idempotente: reimportar no duplica) y no entran en la cola de envío. Impresoras que no existen en el equipo: se importan sus reglas con aviso y no se activa retención. Después se reaplica la retención (`apply_hold` / `syncHolds`) y se devuelven los avisos de permisos como `set_rules`. Nota: `config.json.bak` lo sobrescribe el siguiente guardado normal.
- **Seguridad:** escritorio exige `guard()` (contraseña si existe) al exportar e importar y solo lee/escribe rutas elegidas con el diálogo del sistema (fuera del directorio de datos); servidor: solo administrador con sesión (sin sesión no se lee el cuerpo), cuerpo máx. 12 MiB y archivo máx. 10 MiB, y deja un evento «Importación/Exportación de copia de seguridad» con quién, cuándo, modo y categorías. El panel web exporta como descarga e importa por subida de archivo.
- **Compatibilidad:** `export_rules`/`import_rules` siguen existiendo y usan el formato nuevo; importar un archivo de reglas antiguo (config.json completo) sigue funcionando y se ignoran en él los tokens y la contraseña.
- **Pruebas:** vectores compartidos `core/tests/vectors/backup.json` (ida y vuelta con comparación del archivo exportado entre Rust y Go, versión futura, archivo corrupto/enorme/anidado, tokens y contraseña inyectados, combinar vs. reemplazar, formato antiguo, eventos idempotentes) que corren en `cargo test` y `go test`; pruebas HTTP del servidor y de aplicar en escritorio.

**Pendiente / no hecho:** exportación del registro como CSV con filtros de fechas desde esta tarjeta (el CSV del Registro sigue donde estaba); aplicar inventario al importar (hoy solo informativo); probado en la interfaz solo el panel web (el escritorio comparte el mismo código pero no se ejecutó con Tauri aquí) y no en Windows.

## Liviano por diseño

- **Servidor en Go**, sin Tauri ni webview: un solo binario estático (~11 MB con SQLite incluido; era ~6,4 MB sin él), sin runtime, HTTP con la biblioteca estándar (`net/http`), sin frameworks. Única dependencia externa: `modernc.org/sqlite` (sin CGO).
- Interfaz web incluida **dentro del binario** (`go:embed`; HTML/CSS/JS sin framework, la misma de escritorio).
- Imagen Docker basada en **Alpine** con solo `cups` y el binario, sin interfaz ni filtros gráficos de CUPS (los trabajos se reenvían tal cual, RAW).
- Metas: imagen < 40 MB, < 15 MB de RAM en reposo. Se medirán y se publicarán en la guía.
- Sondeo adaptativo igual al de escritorio (200 ms con actividad, intervalo configurado en reposo).

## Plan de implementación (prioridad: servidor antes que mejoras de escritorio)

1. **Vectores de prueba compartidos.** Casos de reglas en JSON (`core/tests/vectors/rules.json`: trabajo, reglas globales y por impresora, decisión y motivo esperados). Rust (`core/src/rules.rs`) y Go los ejecutan, para que ambas implementaciones no diverjan.
2. **`server/` en Go (`control-server`).**
   - Reglas (`explain_both`, bloqueo gana siempre), configuración y registro (`config.json` y `managed.json`: mismo formato que escritorio; el registro vive en `events.db`).
   - Adaptador CUPS con `lpstat`, `lpadmin`, `lp`, `cancel` e `ipptool`, igual que `core/src/cups.rs`.
   - Worker con sondeo adaptativo.
   - API `POST /api/<comando>` con los mismos nombres que usa la app de escritorio; login con contraseña que entrega un token de sesión, HTTPS detrás de un proxy inverso.
2b. **Bloqueo rápido:** acciones de API `block_host`, `unblock_host` y `pause_printer`, visibles en la vista Reglas; se guardan como regla global de IP en modo bloqueo y se registran con usuario y hora.
3. **Interfaz web.** `src/shim.js` reemplaza `window.__TAURI__` por llamadas HTTP cuando no corre dentro de Tauri y oculta lo exclusivo de escritorio (inicio automático, extensión, menú). Sin rediseño.
4. **Docker.** `Dockerfile` multi-etapa (Go → Alpine + CUPS mínimo + binario) y `docker-compose.yml` con puertos 631 (IPP) y 8080 (panel), volúmenes para configuración y registro, ejecución sin root con permiso `lpadmin`.
5. **Proxmox / VPS.** Guía para un LXC con Docker y script de instalación directa en Debian/Ubuntu. Guía para restringir la impresora al servidor (filtro de IP de la impresora o firewall en 9100, 515 y 631).
6. **CI.** Pruebas Go con los vectores, compilación de la imagen y prueba CUPS real.

## Actualización de la extensión

La extensión se carga "sin empaquetar" desde una carpeta (`<datos>/extension` en escritorio) y una extensión no puede reemplazar sus propios archivos. Por eso el botón **"Buscar actualización"** de la tarjeta "Conexión" del popup funciona por modo (estados: "Al día · vX", "Hay una versión nueva: vY", "Sin conexión"):

- **Modo escritorio:** el host nativo responde `get_version` con la versión de la extensión empaquetada en los recursos de la app (y la copiada en `<datos>/extension`). Si es mayor que la que corre, el popup muestra **"Actualizar"**: el host recibe `update_extension`, copia los archivos de la app a `<datos>/extension` (misma copia que usa `browser::ensure`; la ruta de origen la deja la app en `<datos>/ext_source` al instalar) y el popup llama `chrome.runtime.reload()`. Una sola pulsación. Requiere haber abierto la app nueva al menos una vez.
- **Modo servidor:** `GET /ext/version` (token de extensión) devuelve `{version, sha256, size, url}` y `GET /ext/package.zip` el zip de la carpeta `extension`. El popup compara versiones y, si hay una mayor, muestra **"Descargar vY"**: baja el zip con el token, **comprueba su SHA-256 contra `/ext/version`** (si no coincide lo descarta), lo guarda y muestra el hash con instrucciones: descomprimir sobre la carpeta de la extensión y pulsar Recargar en `chrome://extensions` (Firefox: `about:debugging`). Reemplazar la carpeta no se puede automatizar desde el navegador.
  - El servidor arma el zip al vuelo (orden y fechas fijos: mismo contenido, mismo SHA-256) desde la carpeta `extension` y lo cachea mientras no cambien los archivos. Carpeta: `CONTROL_EXT_DIR`, si no `/app/extension`, `extension` junto al ejecutable o `./extension`. La imagen Docker la trae (`COPY extension /app/extension` en `docker/Dockerfile`; no hay paso de build aparte). Sin Docker, copiar la carpeta `extension` junto al binario o apuntar `CONTROL_EXT_DIR`. La versión sale de su `manifest.json`: para publicar una nueva, subir `version` ahí y reconstruir la imagen.
  - Límites por token: 30/min en `/ext/version`, 10/min en `/ext/package.zip`.
- **Actualización sin pasos manuales (opcional, para organizaciones):** distribuir por política (`ExtensionInstallForcelist` con `update_url` que apunta a un `updates.xml` del servidor, y un `.crx` firmado con una clave propia) en Chrome/Edge administrados, y `update_url` en el manifiesto de Firefox con un `.xpi` firmado. Requiere navegador administrado y firmar los paquetes; se evalúa después.
- **Seguridad:** el paquete se descarga solo del servidor configurado (HTTPS recomendado); el SHA-256 protege la integridad de la descarga, no prueba el origen (lo anuncia el mismo servidor). El botón nunca ejecuta código descargado: solo ofrece el archivo o recarga la carpeta ya instalada.

## Fuera de alcance (por ahora)

- Conectar la app de escritorio a un servidor remoto.
- Alta disponibilidad (varios servidores).

## Limitaciones a comunicar

- Quien pueda imprimir directo a la IP de la impresora se salta el control: hay que restringirla a la IP del servidor.
- El servidor debe estar encendido para imprimir.
- Un administrador del servidor puede cambiar las reglas: proteger el panel con contraseña fuerte y HTTPS.
- **Protección previa a la impresión (Docker):** las colas se crean con el backend `spoolguard` (`device-uri` `spoolguard:socket://…`), que espera el veredicto de control-server en `/var/lib/spoolguard/verdicts` antes de enviar datos a la impresora; así un trabajo con `job-hold-until=no-hold` ya no se imprime antes de evaluarse. Sin veredicto en 10 s se imprime (servidor caído no debe parar la impresión).
- `job-hold-until=no-hold`: con `Hold-New-Jobs` ya no salta la retención un `Print-Job` (Windows, navegadores), pero sí un cliente CUPS (`lp`, cola `ipp://` de otro cupsd) que lo pida explícitamente (ver «Límites conocidos»).

## Uso rápido

### 1. Levantar el servidor (Docker Compose)

```bash
export CONTROL_ADMIN_PASSWORD='una-clave-larga'          # obligatoria (primer arranque)
export PRINTERS='Oficina=socket://192.168.1.50:9100'     # Nombre=uri;Nombre2=uri2
docker compose up -d --build
```

- Panel: `http://127.0.0.1:8080` desde el propio servidor (usuario único, contraseña de administrador). Para usarlo desde otros equipos: `PANEL_BIND=0.0.0.0` (o una IP) y un proxy HTTPS delante; el panel habla HTTP plano.
- Cola IPP para los PC: `ipp://SERVIDOR:631/printers/Oficina`.
- `PRINTERS` ejecuta en cada arranque `lpadmin -p NOMBRE -E -v socket://IP:9100 -m raw -o printer-is-shared=true` (cola RAW, sin filtros). A mano: `docker compose exec control lpadmin -p NOMBRE -E -v socket://IP:9100 -m raw`.
- Datos en el volumen `/data`: `config.json`, `log.jsonl`, `managed.json` (mismo formato que escritorio) y `quick.json` (bloqueo rápido: quién y cuándo).

Sin Docker: `go build -ldflags="-s -w" -o control-server ./server` (solo biblioteca estándar; la interfaz va incrustada) y ejecutarlo con `CUPS` instalado (`lpstat`, `lpadmin`, `lp`, `cancel`, `ipptool` en el `PATH`).

### 2. Variables

| Variable | Efecto |
|---|---|
| `CONTROL_ADMIN_PASSWORD` | Contraseña de administrador. Solo se usa si aún no hay una guardada; también `CONTROL_ADMIN_PASSWORD_FILE` o el archivo `<datos>/admin_password`. Sin ninguna, se genera una y se imprime una vez en el registro del proceso. Después se cambia en Ajustes (mínimo 8 caracteres; no puede quedar vacía). |
| `CONTROL_IMPRESION_DIR` | Directorio de datos (`/data` en Docker; `./data` por defecto). |
| `CONTROL_LISTEN` | Dirección HTTP (`0.0.0.0:8080` por defecto; también `-listen`). |
| `CONTROL_RELEASE_ON_EXIT=1` | Libera las colas al detener el servidor. Por defecto quedan **retenidas** (los trabajos esperan al servidor; nada se imprime sin control). |
| `CONTROL_TRUST_PROXY=1` | Toma la IP del cliente de `X-Forwarded-For` (solo detrás de tu proxy). **No expongas el puerto 8080 con esta opción**: quien llegue directo puede falsificar la cabecera (verificado: `X-Forwarded-For: 1.2.3.4` enviado directo al 8080 quedó registrado como `admin@1.2.3.4`; a través del proxy se reemplaza). El contenedor debe escuchar solo en la red del proxy o en `127.0.0.1` (p. ej. `127.0.0.1:8080:8080` en el `ports` de Compose o solo la red interna de Docker compartida con el proxy). |
| `CONTROL_EXT_DIR` | Carpeta `extension` que publica `/ext/package.zip` (en la imagen: `/app/extension`). |

Sesión: `POST /api/login` con `{"password":"..."}` devuelve un token (cookie `HttpOnly` `SameSite=Strict`, también aceptado como `Authorization: Bearer`). Tras 5 intentos fallidos desde una IP, se bloquea 15 minutos. El resto de `POST /api/<comando>` exige sesión. Bloqueo rápido: `block_host {host}`, `unblock_host {host}`, `pause_printer {printer, paused}`, `get_quick`.

### 3. Agregar la impresora en cada equipo

Estado de la verificación: marcado **(probado)** lo ejecutado contra un servidor real en Docker; **(no probado)** lo que no se ha corrido.

**Linux / contenedor con CUPS (probado):** cola cliente que apunta al servidor, cola RAW. El equipo genera el formato de la impresora (controlador del fabricante); el servidor no convierte documentos.

```bash
lpadmin -p Oficina -E -v ipp://SERVIDOR:631/printers/Oficina -m raw
lp -d Oficina -o raw archivo
```

(Con un cupsd mínimo como el de Alpine, `lpadmin` exige que el usuario pertenezca al grupo `lpadmin`.) Los trabajos de estas colas llegan con `job-originating-user-name` duplicado; el servidor lo tolera. También se probó `lp -h SERVIDOR:631 -d Oficina` directo, sin cola local.

**Windows (probado en Windows 10, controlador "Generic / Text Only"):** `Add-PrinterPort` dio un error genérico, pero `Add-Printer` con el puerto de URL creó la impresora (puerto "Internet Port"; requiere la característica "Cliente de impresión de Internet"). Los envíos RAW y `Out-Printer` llegaron y se evaluaron.

```powershell
Add-Printer -Name "Oficina (controlada)" -DriverName "Generic / Text Only" -PortName "http://SERVIDOR:631/printers/Oficina"
# Para una impresora real, usar el controlador del fabricante en -DriverName.
```

**No probado:** `Add-Printer -ConnectionName "http://SERVIDOR:631/printers/Oficina"`, el controlador "Microsoft IPP Class Driver", el controlador real de un fabricante, y todo lo de macOS: `lpadmin -p Oficina -E -v ipp://SERVIDOR:631/printers/Oficina -m raw` es la misma orden de CUPS que en Linux y debería funcionar, pero no se corrió en un Mac cliente. `-m everywhere` tampoco se probó (la cola del servidor es RAW; no se verificó que ofrezca lo que ese modo necesita), por eso aquí se usa `-m raw`.

La IP de origen que ve el servidor es la del cliente solo si no hay NAT en medio: con Docker en Mac (o doble NAT) todos aparecen con la IP de la pasarela, y las reglas por IP no distinguen equipos; en un servidor Linux sí se conserva.

### 4. Paso obligatorio: restringir la impresora al servidor

En el panel de la impresora (filtro de IP) o en el firewall/VLAN, permitir 9100, 515 y 631 **solo desde la IP del servidor**. Comprobación: desde otro equipo, `nc -vz IP_IMPRESORA 9100` debe fallar. Sin esto, quien imprima directo a la impresora se salta el control.

### 5. Límites conocidos de esta versión

- `job-hold-until=no-hold`: un cliente que lo envíe evita la retención por defecto de la cola (`job-hold-until-default`). Mitigación: cada cola con control queda en **`Hold-New-Jobs`** (operación IPP; `docker/entrypoint.sh` la aplica al crear cada cola por el socket local de cupsd, porque por TCP exige autenticación y no sobrevive al reinicio de cupsd; `control-server` la reaplica en `Reconcile` al arrancar y al activar el control de una cola, y la quita con `Release-Held-New-Jobs` al quitar el control o en las colas sin control). Verificado con CUPS real (Docker): un `Print-Job` con `no-hold` (clientes IPP nativos de Windows, navegadores, `ipptool`) queda retenido, se evalúa y se libera o cancela. **Límite que sigue vigente:** los clientes CUPS que usan `Create-Job` + `Send-Document` (`lp`, y las colas `ipp://` de otro cupsd) con `-o job-hold-until=no-hold` siguen saltando la retención: el trabajo nace retenido ("job-held-on-create") pero `Send-Document` lo libera de inmediato (verificado: `lp -o job-hold-until=no-hold` imprimió sin evaluarse con la cola en "holding new jobs"). La política de cupsd no puede filtrar atributos; solo lo mitigan el acceso controlado a la red del servidor y revisar el registro. La liberación de trabajos sigue funcionando (la hace el servidor, `@SYSTEM`, con `lp -H resume`). La aplicación de `Hold-New-Jobs` es de mejor esfuerzo: si falla, queda la retención por defecto.
- Un trabajo retenido que el servidor no puede leer (tras 3 intentos) se registra como "No se pudo leer el trabajo" y se resuelve con la política de desconocidos (`unknown_policy`); nunca queda retenido en silencio.
- Justo después de activar el control en una cola, durante unos milisegundos CUPS puede no retener un trabajo recién enviado (verificado en CUPS real).
- Por IP solo se evalúa lo que entrega CUPS (usuario, IP, nombre del documento); no hay regla por usuario ni por sitio web, y la "aplicación" solo aparece si va en el nombre del documento.
- "Pausar impresora" y los bloqueos rápidos se ven en el registro y con `get_quick`; la interfaz de escritorio aún no tiene botones para ellos (los IP bloqueados sí aparecen en Reglas → global).
- La contraseña se guarda con PBKDF2-HMAC-SHA256 (600 000 iteraciones, sal aleatoria), el mismo formato que escritorio, para que `config.json` siga siendo intercambiable (ver «Seguridad y endurecimiento»); el límite de intentos y HTTPS en el proxy completan la protección.
- Medidas (equipo de desarrollo): binario `control-server` 7,1 MB (linux/amd64 estático), 7,3 MB (macOS); RAM en reposo 12 MB RSS (servidor) + ~6 MB (cupsd) en el contenedor; imagen Docker 54 MB descomprimida (~25 MB comprimida): la meta de 40 MB no se cumple sin recortar CUPS.


## Extensión con el servidor

La extensión SpoolGuard Web puede usar el servidor en lugar del host nativo de la app de escritorio, para aplicar reglas por **sitio web** sin instalar la app en cada PC.

1. Abre `https://servidor:8080/tokens.html` (con la sesión de administrador; no hay enlace desde Ajustes, porque `app.js` es compartido con escritorio) y crea un token por equipo. Se muestra **una sola vez**; el servidor guarda solo su hash SHA-256 en `ext_tokens.json`. También existen los comandos `create_ext_token`, `list_ext_tokens` y `revoke_ext_token` en `/api/`. Revocar corta el acceso de inmediato.
2. En el popup de la extensión elige **Servidor**, escribe la dirección y pega el token. El navegador pide permiso para comunicarse con ese servidor.
3. Rutas (solo con `Authorization: Bearer <token>`; no usan cookies; actualización de la extensión: ver arriba): `GET /ext/rules` devuelve `{"block":[...],"allow_only":[...]|null}` (igual que el host nativo) y `POST /ext/intent` `{url,title,kind?}` guarda en `intents.jsonl` (mismo formato que escritorio). El worker usa esas intenciones recientes (título del documento + tiempo, `find_site`) para asociar el sitio al trabajo, de modo que las reglas de **sitio** funcionan en modo servidor.
4. Seguridad: el token de extensión no abre `/api/` (admin), y la contraseña de administrador no vale como token. CORS solo acepta orígenes `chrome-extension://` y `moz-extension://`; cuerpo máx. 4 KB; límites por token de 30 peticiones/min en `/ext/intent` y 120/min en `/ext/rules` (la extensión consulta cada 15 s). Usa HTTPS (proxy): con http el token viaja sin cifrar.


## Seguridad y endurecimiento (auditoría 2026-11)

Cambios que alteran el comportamiento anterior, con su compatibilidad:

- **Formato de contraseña.** Antes: `SHA-256(sal + contraseña)` (64 hex) con `password_salt` aparte. Ahora: `password_hash = "pbkdf2-sha256$<iteraciones>$<sal hex>$<clave hex>"` (600 000 iteraciones, sal de 16 bytes aleatorios, `password_salt` vacío). **Migración transparente:** los hashes antiguos se siguen aceptando y se reescriben al formato nuevo en el siguiente inicio de sesión correcto (servidor y escritorio). Un hash nuevo no lo entiende una versión anterior del programa: si se vuelve atrás, hay que restablecer la contraseña (`password_hash` y `password_salt` vacíos y reiniciar con `CONTROL_ADMIN_PASSWORD`). El vector compartido (misma sal y contraseña → mismo hash) está en `internal/app/password_test.go` y `src-tauri/src/pw.rs`, y se verificó además con `hashlib.pbkdf2_hmac` de Python.
- **Docker: puertos.** `docker-compose.yml` publica el panel (8080) solo en `127.0.0.1` por defecto (`PANEL_BIND`), y el IPP (631) en todas las interfaces (`IPP_BIND`), porque es el servicio que usan los PC. Quien accedía al panel por `http://SERVIDOR:8080` debe definir `PANEL_BIND=0.0.0.0` (idealmente tras un proxy HTTPS) al volver a levantar el compose. Los contenedores ya creados no cambian hasta `docker compose up -d`.
- **Docker: privilegios.** `cap_drop: [ALL]`, `no-new-privileges` y `pids_limit`. El contenedor ya corría como usuario `control` (uid 1000), incluido `cupsd` (con `cups-files.conf` y política `@SYSTEM` = grupo `lpadmin`); no necesita root. No se usa `read_only` porque `cupsd` escribe en `/etc/cups`, `/var/spool/cups` y `/run/cups`.
- **Contraseña inicial.** Nunca hay una fija: `CONTROL_ADMIN_PASSWORD`, o el archivo `CONTROL_ADMIN_PASSWORD_FILE` (Docker secrets), o una aleatoria impresa una sola vez en el registro del proceso (`docker logs`). El compose exige la variable. Mínimo de 8 caracteres al cambiarla desde el panel.
- **Inicio de sesión.** El intento se cuenta *antes* de verificar la contraseña (antes, una ráfaga de peticiones simultáneas pasaba todas el control de bloqueo): 5 fallos por IP y bloqueo de 15 min; como mucho 4 comprobaciones de contraseña a la vez (el KDF es costoso a propósito). Con `CONTROL_TRUST_PROXY=1` se usa la **última** entrada de `X-Forwarded-For` (la que añade tu proxy), no la primera, que escribe el cliente.
- **Permisos.** Al arrancar, el servidor deja la carpeta de datos en `0700` y sus archivos en `0600` (instalaciones previas o volúmenes montados con otros permisos). En escritorio, `config.json`, su `.bak`, `managed.json` y `network.json` se crean ya con `0600` (antes `0644` y luego `chmod`) y la carpeta de datos queda en `0700`.
- **Cabeceras.** Además de CSP (`frame-ancestors 'none'`), `nosniff`, `X-Frame-Options: DENY` y `Referrer-Policy: no-referrer`: `Permissions-Policy` y `Cross-Origin-Opener-Policy: same-origin`; `Cache-Control: no-store` en `/api` y `/ext`. HSTS lo debe poner tu proxy HTTPS (el servidor no termina TLS). La cookie de sesión es `HttpOnly; SameSite=Strict` y lleva `Secure` solo si la petición llegó por HTTPS (`X-Forwarded-Proto`).
- **Exportación CSV.** Máximo 200 000 filas por exportación (acota la memoria); con más, acota el filtro.
- **Escritorio bloqueado.** Con contraseña activa y la app bloqueada, también los comandos de lectura sensibles (impresoras y reglas, registro, resumen, inventario, red) exigen desbloquear, no solo los de escritura. La interfaz carga los datos al desbloquear.
- **Capacidades de la ventana (Tauri).** Se retiraron `autostart`, `notification` y `window-state` del webview (la interfaz solo usa `core`, `dialog:open/save/ask` y dos permisos de ventana): el inicio automático y las notificaciones se gestionan desde Rust con `guard()`.
- **Nombres de cola.** Antes de pasarlos a `lpadmin`, `lpstat` o a una URI IPP se validan (sin `-` inicial, controles, espacios, `/`, `\`, `#`, `?`).
- **Resistencia a fallos.** Un pánico en el worker, la detección directa o el descubrimiento se registra y el proceso sigue (antes lo tumbaba).

Límites y riesgos residuales conocidos (no corregidos, con su motivo) están en `work/security/INFORME.md` (sección «No corregido / no verificado»).
