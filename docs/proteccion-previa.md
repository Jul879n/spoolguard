# Protección previa a la impresión

Aplica a macOS, Linux (escritorio), Windows y a la imagen Docker: **la misma garantía en todos los sistemas**.

## Por qué existe

SpoolGuard retiene la cola (CUPS: `job-hold-until-default`; Windows: cola en pausa) y evalúa cada trabajo antes de liberarlo. Pero hay huecos:

- **macOS/Linux:** algunas apps, como Microsoft Word, envían cada trabajo con `job-hold-until=no-hold`, que anula la retención de la cola. Medido: el trabajo nace directamente en estado `processing` y puede alcanzar la impresora antes de que la app lo evalúe.
- **Windows:** para liberar un trabajo hay que reanudar la cola entera (el spooler no libera trabajos sueltos con la cola en pausa), así que durante unos milisegundos cualquier trabajo no pausado podría salir; y un trabajo que ya está imprimiendo no se puede detener a tiempo.

La protección previa cierra ese hueco: hace que cada trabajo espere, **dentro del sistema de impresión y antes de enviar datos a la impresora**, un veredicto de SpoolGuard.

## Cómo funciona (modelo común)

1. Cada trabajo nuevo pasa por una pieza del sistema de impresión que SpoolGuard intercala: un filtro de CUPS (macOS/Linux), un backend de CUPS (Docker) o un **print processor** del spooler (Windows).
2. Esa pieza espera un archivo `<cola>-<id del trabajo>` en la carpeta de veredictos:
   - macOS: `/Library/Application Support/SpoolGuard/verdicts`
   - Linux y Docker: `/var/lib/spoolguard/verdicts`
   - Windows: `C:\ProgramData\SpoolGuard\verdicts`
3. SpoolGuard evalúa el trabajo contra las reglas y escribe `allow` o `deny` en ese archivo (escritura atómica: archivo temporal en la misma carpeta y renombrado).
4. Con `deny`, la pieza descarta los datos (el trabajo termina sin imprimir y se anota «bloqueado por SpoolGuard»). Con `allow`, el trabajo continúa por el camino original.
5. **Falla abierta (por defecto):** si no hay veredicto en **10 segundos** (app cerrada, bloqueada o con error), el trabajo se imprime. Una app caída no debe parar la impresión de toda la oficina. El plazo y el modo contrario («estricto») son configurables: ver la sección siguiente.

En Docker el mismo mecanismo lo cumple un backend de CUPS (`docker/spoolguard-backend`): las colas se crean con `device-uri` `spoolguard:socket://IP:9100`, el backend espera el veredicto y luego delega en el backend real. Ya viene incluido en la imagen; no hay nada que activar (ver [docker.md](docker.md)).

## Solo esperan las colas con control

Cada cola con control activo lleva una marca `controlled-<cola>` en la carpeta de veredictos (la escribe la app al activar el control y la borra al desactivarlo; el servidor la gestiona en `SetHold` y al arrancar). El backend de Docker y el filtro de macOS/Linux **solo esperan el veredicto en las colas marcadas**; una cola protegida pero sin control deja pasar los trabajos de inmediato, también con el modo estricto activo (sin esta marca, el estricto la bloqueaba para siempre). Verificado en Docker (cola sin control imprime a los 4 s con estricto; cola con control y la app detenida retiene, y al volver la app imprime lo permitido y descarta lo bloqueado) y con el filtro real en CUPS de Linux (sin marca pasa en 0 s; con marca y sin veredicto espera; con `allow` pasa en 0 s). «Reparar» en la app regenera el envoltorio de las colas ya protegidas, así que actualiza un filtro instalado por una versión anterior. En Windows el procesador ya usaba un latido por cola con el mismo efecto.

## Verificación de la app de Linux corriendo

App de escritorio de Linux (compilada en `release`, bajo Xvfb) con CUPS real y una cola con driver LaserJet, en un contenedor Debian. Se envían 5 trabajos `.docx` (bloqueados por regla) y 5 `.xlsx` (permitidos) por fase, y se cuentan las conexiones con datos al listener (un trabajo = 11 461 bytes; las de 0 bytes son sondeos de CUPS):

| Fase | `.docx` impresos | `.xlsx` impresos |
|---|---|---|
| Retención normal (la app libera o cancela) | 0 de 5 | 5 de 5 |
| `no-hold` sin filtro | 0 de 5 | 5 de 5 |
| `no-hold` con el filtro instalado | 0 de 5 | 5 de 5 |

Veredictos: 5 `deny` y 5 `allow`; 0 trabajos pendientes. Sin filtro tampoco hubo fuga en esta corrida porque la app cancela dentro de su intervalo (1 s), pero ahí la ventana existe y solo el filtro la cierra. No se probó con una sesión gráfica real ni con hardware.

## Espera del veredicto y modo estricto (configurables)

Por defecto el comportamiento es el de arriba: se espera **10 s** y, sin veredicto, **se imprime** (falla abierta). Dos ajustes lo cambian:

| Ajuste | Valores | Por defecto |
|---|---|---|
| Espera del veredicto | entero de **1 a 600** segundos | 10 |
| Estricto: no imprimir si la app no responde | activado / desactivado | desactivado |

**Dónde se configuran**

- **Escritorio (macOS, Linux, Windows):** Ajustes → *Protección previa a la impresión*: campo «Espera del veredicto (s)» e interruptor «Estricto». Un valor fuera de 1–600 se rechaza (no se guarda).
- **Docker:** variables de entorno `SPOOLGUARD_VERDICT_TIMEOUT` (1–600) y `SPOOLGUARD_STRICT` (`1`/`true` o `0`/`false`), definidas en el compose. Se aplican al arrancar y se pueden cambiar después desde los ajustes del servidor. Un valor inválido se ignora y se anota un aviso en el registro de avisos.
- **Copia de seguridad:** ambas opciones viajan en la categoría `parametros` (`verdict_timeout_secs`, `strict`) con la misma validación en el escritorio y en el servidor.

**Cómo llegan a la pieza de impresión.** La app escribe, de forma atómica (archivo temporal + renombrado) y solo cuando cambian, el archivo `config` en la carpeta de veredictos, con dos líneas:

```
timeout=10
strict=0
```

El procesador de Windows, el filtro de CUPS y el backend de Docker lo leen en cada trabajo. **Archivo ausente, ilegible a medias o con un valor inválido = valores seguros por defecto (10 s, falla abierta)**; en Windows, si el archivo existe pero no se puede leer en ese instante (bloqueo momentáneo), se sigue con la última configuración válida leída.

**Qué hace el estricto.** Sin veredicto a tiempo (o con la app cerrada) el trabajo **no se imprime**:

- **Windows:** el procesador no delega en `winprint` (no sale ningún dato) y le pide al spooler que **retenga** el trabajo (`SetJob` con `JOB_CONTROL_RETAIN`) y termina con éxito. El trabajo queda en la cola como «Complete, Retained» con un acuse `hold`. Al volver, la app lo evalúa: si lo permite, escribe `allow` y lo **reinicia** (`JOB_CONTROL_RESTART`); el procesador lo vuelve a recibir, ve el veredicto, suelta la retención (`JOB_CONTROL_RELEASE`) y el trabajo se imprime y sale de la cola. Si lo bloquea, lo borra (no salió nada: se anota «bloqueado»). Probado en Windows 10 (ver `work/winguard/INFORME.md`). Alternativa descartada con evidencia: devolver error desde el procesador **borra** el trabajo (se perdería).
- Con la app **cerrada**, un latido ausente o viejo no cuenta como «cola no controlada» (eso deja pasar en el modo normal): en estricto se espera y se retiene. Tras la primera retención sin veredicto, los trabajos siguientes se retienen tras 0,3 s (no otros 10–600 s cada uno).
- **Docker:** el backend sale con código 3 (`CUPS_BACKEND_HOLD`; el 4 es `CUPS_BACKEND_STOP` y detiene la cola entera, verificado en CUPS real): CUPS retiene el trabajo y el servidor lo evalúa. *Verificado en CUPS real (Docker)*: con la app detenida los trabajos quedaron retenidos (0 impresos, cola activa) y al volver la app se evaluaron.
- **macOS/Linux (filtro):** CUPS no puede retener un trabajo desde un filtro, y salir con error lo **aborta y lo pierde** (verificado en CUPS real de macOS: el trabajo quedó en `processing-to-stop-point` sin contenido impreso y sin forma de reevaluarlo). Por eso, con `strict=1` el filtro **no vence por tiempo**: espera el veredicto sin límite. No se pierde ni se imprime nada; la cola queda ocupada por ese trabajo hasta que la app escriba el veredicto o alguien lo cancele (riesgo operativo mayor que en Windows y Docker: los demás trabajos de la cola esperan detrás).
  - **Verificado en CUPS real (macOS 26, app cerrada):** con `strict=1` y `timeout=5` el trabajo esperó sin límite (0 impresos a los 30 s, 1 pendiente) y se imprimió al escribir `allow`. Precaución al probar: si hay otra instancia de la app corriendo (con control global), ella escribe `allow` y el trabajo sale a los pocos segundos; hay que cerrarla para medir el estricto.

**Riesgo operativo.** Con el estricto activado, **si la app está cerrada, nadie imprime en esas colas** hasta que vuelva (los trabajos quedan retenidos, no se pierden). Por eso va desactivado por defecto. En estricto conviene una espera larga (p. ej. 120 s) para no retener trabajos por una pausa breve de la app. El procesador bloquea un hilo del spooler hasta la espera configurada (máximo 600 s) por trabajo, nunca indefinidamente.

## Windows: el print processor `spoolguardpp`

### Diseño

Windows permite registrar un *print processor* por cola: una DLL que el spooler (`spoolsv.exe`, como SYSTEM) carga y llama para cada trabajo justo antes de enviar los datos al puerto. SpoolGuard trae `winguard.dll` (crate `winguard/`, Rust), que **envuelve a `winprint.dll`** (el procesador estándar de Windows):

| Exportación (winsplp.h) | Qué hace `winguard.dll` |
|---|---|
| `OpenPrintProcessor` | Abre el de `winprint` y guarda la cola y el id del trabajo (vienen en `PRINTPROCESSOROPENDATA`). |
| `PrintDocumentOnPrintProcessor` | **Espera el veredicto** (sondeo de 20 ms). `deny`: devuelve éxito sin enviar nada. `allow`, sin veredicto a los 10 s o un error propio: delega en `winprint`. |
| `ClosePrintProcessor`, `ControlPrintProcessor` | Reenvía a `winprint` (un `CANCEL`/`DELETE` del trabajo interrumpe la espera). |
| `GetPrintProcessorCapabilities`, `EnumPrintProcessorDatatypesW` | Reenvía a `winprint`: se aceptan exactamente los mismos tipos de datos (RAW, NT EMF, TEXT, XPS_PASS, ...). |

Detalles que importan:

- **El plazo cuenta desde que el trabajo termina de encolarse.** El spooler puede invocar el procesador mientras el cliente aún escribe los datos (`JOB_STATUS_SPOOLING`); la app no evalúa un trabajo en ese estado (su nombre es provisional: «Local Downlevel Document»). El procesador consulta el estado del trabajo y no cuenta los 10 s mientras siga encolándose (tope absoluto: 5 minutos).
- **El spooler llama al procesador de a un trabajo por cola.** Con la app cerrada, 90 trabajos tardarían 90 × 10 s. Por eso, tras un fallo abierto, los trabajos siguientes esperan solo 0,3 s hasta que vuelva a llegar un veredicto (máximo 60 s en ese modo, luego se vuelve a probar con 10 s completos).
- **Acuse.** Al decidir, el procesador deja `<cola>-<id>.ack` (`deny`, `allow` o `timeout`). La app lo usa para registrar «cancelado» solo cuando de verdad no salió nada (si no hay acuse, asume lo peor: «impreso antes de cancelar»).
- **Latido de la app.** Mientras corre, la app escribe cada segundo `verdicts\.alive` con las colas que controla (las que tienen reglas, o todas si hay reglas globales). Con latido reciente (≤ 5 s): en esas colas el procesador espera el veredicto con el plazo completo (sin modo degradado) y en las demás **no espera nada** (nadie escribiría un veredicto). Sin latido (app cerrada o colgada) espera con falla abierta. Sin esto, activar la protección en una cola sin reglas retrasaría 10 s cada trabajo.
- **Corre con la identidad del dueño del trabajo.** Medido: el spooler llama al procesador suplantando al usuario que envió el trabajo (`GetUserNameW` devuelve su nombre). Esa identidad no tiene acceso a la carpeta de veredictos, así que el procesador vuelve temporalmente a la identidad del servicio (`RevertToSelf`) para leer el veredicto, esperar y dejar el acuse, y restaura la suplantación antes de delegar en `winprint` (que sí necesita la identidad del usuario, p. ej. para escribir en un puerto de archivo).
- **Veredicto viejo.** Un archivo anterior al envío del trabajo (mismo id reutilizado tras reiniciar el spooler) se ignora comparando su fecha con la de envío.
- **Colas con driver V4 no son protegibles.** Medido en Windows 10: el spooler **no llama** al print processor de una cola con driver V4 (`Microsoft Print To PDF`, `Microsoft XPS Document Writer v4`), ni con datos RAW ni con XPS_PASS ni con EMF; el trabajo sale sin pasar por la DLL. Por eso esas colas no cuentan en «protegibles» y el instalador las omite: siguen solo con la retención de cola. Los drivers V3 (la mayoría de los de impresoras de etiquetas, `Generic / Text Only`, drivers de fabricante clásicos) sí pasan por el procesador con RAW, TEXT y EMF.
- **Sin carpeta de veredictos** (protección desinstalada a medias) el procesador no espera nada.

### Estabilidad y seguridad de la DLL

Corre dentro de `spoolsv.exe` como SYSTEM, así que:

- Sin dependencias externas; las llamadas a la API de Windows están declaradas a mano y verificadas contra `winsplp.h`/`winspool.h` del SDK 10.0.19041 y el crate `windows` 0.62.
- Nada de `unwrap`/`expect`/índices/`panic!` (lo impone `#![deny(clippy::...)]`) y cada exportación atrapa los panics con `catch_unwind`. Eso exige `panic = "unwind"`: **la DLL se compila con `cargo build -p winguard --profile dll`**, no con `--release` (el perfil `release` del proyecto usa `panic = "abort"`, con el que `catch_unwind` no atrapa nada; `--release` falla a propósito con un `compile_error!`).
- La cola y el id se sanean para el nombre de archivo (`\ / : * ? " < > |` y controles → `_`, 128 caracteres como máximo): sin separadores de ruta no hay traversal. Se ignora todo lo que no sea un archivo regular de ≤ 16 bytes (enlaces simbólicos/uniones incluidos).
- Falla abierta ante cualquier error propio; nunca bloquea más de 10 s tras terminar de encolarse el trabajo (salvo el tope de 5 min de un trabajo que no termina de encolarse).
- La DLL se carga a `winprint.dll` por ruta absoluta (`%SystemRoot%\System32\spool\prtprocs\x64`), nunca desde el `PATH`.

### Permisos de la carpeta de veredictos (modelo de amenaza)

`C:\ProgramData\SpoolGuard` y `verdicts` se crean con la herencia cortada y estas ACL:

| Principal | `SpoolGuard` | `verdicts` |
|---|---|---|
| SYSTEM (el spooler) | control total | control total |
| Administradores | control total | control total |
| El usuario que ejecuta SpoolGuard (su SID, fijado al activar) | lectura | modificar (crear, renombrar, borrar) |
| Resto de usuarios | nada | nada |

(El procesador lee y escribe como SYSTEM; ver «Corre con la identidad del dueño del trabajo». Por eso no hace falta dar acceso a los demás usuarios.)

Por qué así y no `1777` como en CUPS: en CUPS cualquiera podía escribir `allow` para su propio trabajo; en Windows eso permitiría a un usuario estándar saltarse la protección con solo crear un archivo. Con esta ACL solo puede falsificar un veredicto quien ya puede cambiar las reglas de SpoolGuard (el usuario de la app) o administrar el equipo. Limitaciones: (a) si la app la ejecuta otro usuario distinto del que activó, hay que pulsar **Reparar** desde esa cuenta (cambia el SID autorizado); (b) esto protege la integridad de los veredictos, no impide imprimir con la app cerrada (falla abierta, deliberado) ni la impresión directa a la IP de la impresora.

### Instalación y reversión

Todo lo hace `scripts/guard-windows.ps1` (la app lo ejecuta elevado, con una sola confirmación UAC; va dentro del comando, no en un archivo, para que otro proceso no pueda cambiarlo antes de correr como administrador):

1. Copia `winguard.dll` a la carpeta de print processors del sistema (`GetPrintProcessorDirectory`, `System32\spool\prtprocs\x64\spoolguardpp.dll`) y la registra con `AddPrintProcessor` con el nombre `spoolguardpp`. En el primer alta **no hace falta reiniciar el Spooler** (medido); al **actualizar** una DLL ya cargada el script detiene e inicia el servicio Spooler (los trabajos en cola se conservan; avisa en la salida).
2. Crea las carpetas con las ACL de arriba.
3. Por cada cola local que usa `winprint` (se omiten el fax y las colas con procesadores de otro fabricante): guarda el procesador original en `HKLM\SOFTWARE\SpoolGuard\Queues` y lo cambia a `spoolguardpp` (`Set-Printer -PrintProcessor`).
4. Es idempotente: repetirlo solo corrige lo que falte (colas nuevas, DLL actualizada).

Desactivar (`-Action uninstall`): restaura el procesador original de cada cola, y solo si ninguna cola lo usa ya, retira el procesador (`DeletePrintProcessor`), reinicia el Spooler para descargar la DLL y la borra. La tarjeta de Ajustes muestra «protegidas / total» y ofrece Activar, Reparar y Desactivar.

Si desinstalas SpoolGuard sin desactivar antes, las colas siguen apuntando a `spoolguardpp`: **desactiva la protección antes de desinstalar**. Si no lo haces, no se rompe la impresión (la DLL sigue en System32 y falla abierta), pero el primer trabajo de cada minuto espera 10 s.

## Activarla

**Desde la app (recomendado):** asistente «Configuración de inicio», paso 1 «Permiso de administrador», o Ajustes → «Protección previa a la impresión» → **Activar**. Pide la contraseña de administrador una vez (macOS: diálogo del sistema; Linux: `pkexec`; Windows: UAC).

**A mano en macOS/Linux**, como administrador:

```
sudo scripts/guard-cups.sh install            # todas las colas que tienen PPD
sudo scripts/guard-cups.sh install MiCola     # solo esa cola
scripts/guard-cups.sh status                  # imprime «protegidas total», por ejemplo «3 3»
sudo scripts/guard-cups.sh uninstall          # restaura los PPD originales
```

**A mano en Windows**, en PowerShell como administrador (con `winguard.dll` ya compilada):

```
scripts\guard-windows.ps1 -Action install -Owner <SID del usuario de la app> -Dll .\winguard.dll [-Queues Cola1,Cola2]
scripts\guard-windows.ps1 -Action status       # «protegidas total»
scripts\guard-windows.ps1 -Action uninstall
```

El script de CUPS (`install`):

- crea la carpeta de filtros propios (macOS: `/Library/Printers/SpoolGuard`; Linux: la carpeta de filtros de CUPS, porque AppArmor/SELinux solo dejan ejecutar filtros desde ahí) y la carpeta de veredictos (permiso `1777`);
- genera un envoltorio por cada filtro y reescribe la línea `*cupsFilter` del PPD de la cola para usarlo (`lpadmin -p COLA -P ...`);
- omite las colas sin PPD y las ya protegidas.

Variables (CUPS):

| Variable | Uso |
|---|---|
| `SG_OWNER` | Usuario que ejecuta la app y escribe los veredictos. Por defecto `SUDO_USER`. |
| `SG_LPADMIN` | Ruta o comando de `lpadmin` a usar (por defecto `lpadmin`). |

Si agregas una impresora después, vuelve a activar la protección (**Reparar**) para incluirla.

## Compilar la DLL (Windows)

```
cargo build -p winguard --profile dll        # produce target\dll\winguard.dll
```

El MSI la empaqueta como recurso (`src-tauri/tauri.windows.conf.json`), de modo que **hay que compilarla antes de cualquier compilación de la app en Windows** (`cargo test`, `tauri build`): `release.yml`, `windows-vm.yml` y las demás pruebas de CI ya lo hacen.

## Límites

- **Colas sin PPD** (macOS/Linux; por ejemplo colas «raw» creadas a mano) no tienen filtros que envolver: se omiten y quedan solo con la retención de cola. En Docker las colas son siempre raw y usan el backend, por lo que sí quedan cubiertas.
- **Windows:** solo colas locales con `winprint`. Las que usan el procesador de otro fabricante, el fax y las impresoras de red compartidas desde otro servidor (conexiones) no se tocan.
- No protege la **impresión directa** a la IP de la impresora (puerto 9100): eso se resuelve con firewall o VLAN ([guia-red.md](guia-red.md)).
- La falla abierta es deliberada: si necesitas el comportamiento contrario, no está disponible.
- La DLL no está firmada (Windows no lo exige para un print processor; sí conviene firmarla con Authenticode junto al MSI cuando exista certificado).
