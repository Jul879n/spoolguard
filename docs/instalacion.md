# Instalación de SpoolGuard

Identificador de la app: `cl.cohasa.controlimpresion`. Versión 0.2.0.

## Estado de verificación

- **macOS:** control de cola con CUPS real.
- **Linux:** CUPS real en CI (`.github/workflows/test.yml`). Los paquetes `.deb` y `.rpm` no se han instalado en un equipo real.
- **Windows:** el spooler se probó en un runner real de CI con una impresora virtual (Microsoft Print to PDF). La interfaz **no** se ha probado en un Windows real ni el `.msi` instalado; probar con la impresora del cliente antes de desplegar (requiere permisos de administrador).
- **Extensión:** probada en Chrome. Edge y Firefox sin probar.

## Descargas

Desde la [última release](https://github.com/Jul879n/spoolguard/releases/latest) (`<versión>`: por ejemplo `0.2.0`):

| Sistema | Archivo |
|---|---|
| macOS Apple Silicon (M1 o posterior) | `SpoolGuard-<versión>-macos-arm64.dmg` |
| macOS Intel | `SpoolGuard-<versión>-macos-x86_64.dmg` |
| Windows (x64) | `SpoolGuard-<versión>-windows-x64.msi` |
| Debian / Ubuntu | `SpoolGuard-<versión>-linux-amd64.deb` |
| Fedora / RHEL / openSUSE | `SpoolGuard-<versión>-linux-x86_64.rpm` |
| Extensión de navegador | `SpoolGuard-extension-<versión>.zip` |
| Servidor | imagen `ghcr.io/jul879n/spoolguard` (ver [docker.md](docker.md)) |

Los instaladores **no están firmados ni notarizados**, por eso el sistema muestra un aviso la primera vez. El workflow `.github/workflows/release.yml` los genera en CI al subir una etiqueta `v*`.

## 1. macOS

1. Abrir el `.dmg` que corresponda al procesador (menú Apple → «Acerca de este Mac»: «Apple M…» = `arm64`; «Intel» = `x86_64`) y arrastrar «SpoolGuard» a Aplicaciones.
2. Gatekeeper bloqueará la app por no estar firmada. Para abrirla: clic derecho sobre la app → **Abrir** → **Abrir**; o intentar abrirla una vez y luego Ajustes del Sistema → Privacidad y seguridad → «Abrir de todos modos».
3. Abrir la app, aceptar las notificaciones y seguir el asistente (ver [primeros-pasos.md](primeros-pasos.md)): en el primer paso se activa la [protección previa](proteccion-previa.md), que pide la contraseña de administrador.

Inicio automático: usa un LaunchAgent de usuario con `--minimized` (no es un servicio de sistema: solo funciona con sesión iniciada). Arranca oculta en la barra de menús. Cerrar la ventana (X o Cmd+W) cierra la app y libera las colas.

## 2. Linux

```
sudo apt install ./SpoolGuard-<versión>-linux-amd64.deb
# o
sudo dnf install ./SpoolGuard-<versión>-linux-x86_64.rpm
```

Requiere CUPS (`lpstat`, `lpadmin`, `lp`, `cancel`, `ipptool`). La bandeja usa `libayatana-appindicator3`. Nombre de producto «SpoolGuard» (el nombre exacto del paquete instalado no está verificado). Los paquetes no están firmados. La protección previa se activa desde el asistente (usa `pkexec`) o con `scripts/guard-cups.sh` ([guía](proteccion-previa.md)).

## 3. Windows

Archivo `.msi`. Doble clic, o silencioso como administrador:

```
msiexec /i "SpoolGuard-<versión>-windows-x64.msi" /qn
```

SmartScreen mostrará «Windows protegió su PC» por ser un instalador sin firmar: pulsar **Más información** → **Ejecutar de todas formas**. Windows no necesita protección previa: el spooler retiene los trabajos. Ver el estado de verificación arriba.

## 4. Extensión de navegador (opcional, para reglas por sitio)

El host de Native Messaging es la propia app (`--native-host`) y se registra solo al iniciar, en Chrome, Chromium, Edge y Firefox si están instalados. La extensión hay que cargarla una vez a mano. En Ajustes > Extensión de navegador hay dos caminos:

Guía detallada para usuarios: `docs/instalar-extension.md`.

**Instalar en navegador…** Elegir el navegador (Chrome, Edge, Chromium, Brave o Firefox, los detectados en el equipo) y, si hay más de un perfil, el perfil. Al pulsar "Abrir y copiar ruta" la app abre la página de extensiones de ese navegador y perfil, y copia la ruta al portapapeles (si no puede copiarla, la muestra para copiarla a mano). El diálogo lista los pasos:

- **Chrome, Edge, Chromium, Brave:** la app no puede instalarla sola. Es obligatorio el paso manual: activar "Modo de desarrollador" > "Cargar descomprimida" y pegar la ruta (`<directorio de datos>/extension`).
- **Firefox:** la carga es temporal. Se abre `about:debugging`; pulsar "Este Firefox" > "Cargar complemento temporal…" y elegir `manifest.firefox.json` (en la ruta copiada). Se pierde al cerrar Firefox.
- Si el navegador está administrado por la organización puede que no se abra la página por sí solo; en ese caso abrirla a mano y usar la ruta copiada.

**Descargar extensión (.zip).** Guarda un `.zip` con la carpeta de la extensión (por defecto `control-impresion-extension.zip`), para llevarla a otro equipo o descomprimirla donde se necesite. Es el mismo contenido que `SpoolGuard-extension-<versión>.zip` del release.

También se puede usar "Abrir carpeta de la extensión" y "Abrir chrome://extensions" y cargar la carpeta a mano.

Una vez cargada, el ícono de la extensión muestra un popup con el estado de la conexión, el sitio actual y las reglas activas (ver `extension/README.md`). El ID es fijo (`afjahbecgkkpbenpmgelpiceakiolikp`). Edge y Firefox sin probar.

## 5. Permisos

- **Administrador para la cola:** en macOS/Linux la app ejecuta `lpadmin -p <impresora> -o job-hold-until-default=...` para activar o quitar la retención. En CUPS, `lpadmin` normalmente exige privilegios de administrador (grupo `admin`/`lpadmin` o root). Con un usuario estándar esa llamada puede fallar **[No verificado: no se probó con usuario estándar]**. Opciones: agregar al usuario al grupo `lpadmin` (Linux) o hacer la configuración inicial con un administrador y comprobar luego que los usuarios estándar siguen pudiendo operar. En Windows la retención pausa la cola con la API del spooler y requiere permisos sobre la impresora.
- **Liberar y cancelar trabajos:** usa `lp` y `cancel`; para trabajos de otros usuarios CUPS puede requerir permisos adicionales (`cupsctl --user-cancel-any` existe como opción; evaluar el riesgo).
- **Notificaciones:** permitir las del sistema para ver los avisos de bloqueo.
- **Inicio automático:** activar "Iniciar con el sistema" en Ajustes y verificar tras reiniciar que aparece el ícono de la bandeja. Para usarlo como servicio, dejar el inicio de sesión automático en el PC compartidor.
- **Usuarios:** los usuarios finales deben ser cuentas estándar; ver limitaciones en `guia-red.md`.
- **Contraseña de administrador:** definirla en Ajustes para proteger los cambios en la app. Con contraseña, los comandos que modifican estado (reglas, ajustes, pausa, importar o exportar archivos, instalar la extensión, borrar el registro) exigen haberla verificado.

## 6. Persistencia y cierre

- `config.json` se guarda de forma atómica y en cada guardado se actualiza `config.json.bak` con la última configuración válida. `log.jsonl` se depura al iniciar según la retención (90 días por defecto); la pausa del control se conserva entre reinicios.
- **Configuración corrupta:** al iniciar, si `config.json` no se puede leer, se renombra a `config.json.corrupto-<marca de tiempo>`, se restaura `config.json.bak` y se anota un aviso en el registro. Sin respaldo, la app parte con valores por defecto (y lo avisa también).
- **Cierre:** la X de la ventana y Cmd+W cierran la app, y al salir se liberan todas las colas.
- **Proceso guardián:** al iniciar, la app lanza el mismo ejecutable con `--guardian` (desacoplado). Si la app muere a la fuerza (kill, fallo), el guardián libera las colas registradas en `managed.json`. Usa `app.pid` para no interferir si otra instancia tomó el relevo.
- Nunca quedan trabajos en pausa sin la app en marcha, salvo en un apagón que también termine el guardián: al abrir la app de nuevo se retoma el control (se evalúan los trabajos retenidos y se liberan las colas sin regla activa).
- Las colas que la app retiene se registran en `managed.json`; al iniciar se liberan las que ya no tengan una regla activa (regla borrada, configuración restablecida, cierre inesperado).
- Instancia única: abrir la app dos veces solo muestra la ventana existente.
- Para pruebas, la variable de entorno `CONTROL_IMPRESION_DIR` hace que la app use otra carpeta de datos y no toque la configuración real.

Directorio de datos: macOS `~/Library/Application Support/cl.cohasa.controlimpresion/`; Windows `%APPDATA%\cl.cohasa.controlimpresion\`; Linux `~/.local/share/cl.cohasa.controlimpresion/`. Los de macOS y Windows coinciden con los del host nativo (`core/src/bin/native_host.rs`); el de Linux es el valor habitual de Tauri y no se comprobó en un equipo real.

## 7. Exportar e importar reglas entre PCs

En Ajustes > Datos > Reglas:

1. **Exportar:** abre el selector de archivos del sistema; sugiere `reglas-impresion.json`.
2. Copiar el archivo al otro PC (USB, red).
3. **Importar** en el otro PC: elegir el `.json` en el selector del sistema (máximo 1 MB).

El registro también se exporta a CSV desde la vista Registro.

- El archivo contiene reglas por impresora, la regla global y Ajustes, sin la contraseña de administrador. Al importar se reemplazan reglas y ajustes del PC destino, se conserva su contraseña y se omiten las impresoras que ese PC no tenga.
- Las reglas se indexan por nombre de impresora. Para cada impresora del archivo que sí existe en el destino se aplica la retención; las demás se descartan sin error. Si falla `lpadmin` en alguna (permisos), la importación se detiene y no reemplaza la configuración.
- Importar requiere los mismos permisos de la sección 5.

## 8. Desinstalación

1. Quitar la protección previa (macOS/Linux), con la app aún instalada: Ajustes → «Protección previa a la impresión» → Desactivar; o `sudo scripts/guard-cups.sh uninstall` ([guía](proteccion-previa.md)).
2. En Ajustes, desactivar «Iniciar con el sistema». Salir de la app (libera las colas).
3. **macOS:** mover «SpoolGuard» de Aplicaciones a la Papelera. **Windows:** Configuración → Aplicaciones → SpoolGuard → Desinstalar (o `msiexec /x` con el `.msi`). **Debian/Ubuntu:** `sudo apt remove spoolguard` (nombre por confirmar con `dpkg -l | grep -i spoolguard`). **Fedora:** `sudo dnf remove spoolguard` (ídem con `rpm -qa | grep -i spoolguard`).
4. Opcional: borrar el directorio de datos (sección 6); contiene reglas y registro.
