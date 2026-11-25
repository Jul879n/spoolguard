# Funcionamiento de SpoolGuard

Detalle técnico movido desde el README: motor de reglas, persistencia, cierre de la app e interfaz.

## Cómo funciona el control

- Una impresora con control activo queda retenida. Un hilo de fondo revisa la cola, evalúa cada trabajo contra las reglas y lo libera o lo cancela. Cada decisión se anota en el registro.
- Sondeo adaptativo: mientras hay trabajos en la cola (y hasta 5 s después del último) revisa cada 200 ms; en reposo usa el intervalo configurado (`interval_secs`, 1 a 60 s, por defecto 1). No se escapa ningún trabajo porque la cola se retiene de antemano: los trabajos nacen retenidos (CUPS: `job-hold-until-default`; Windows: cola en pausa) y solo salen cuando se evalúan.
- Reglas globales y por impresora: en Reglas, la opción "★ Todas las impresoras (global)" define una regla que se suma a la de cada impresora. Si cualquiera de las dos bloquea, el trabajo se bloquea (bloqueo gana siempre). El interruptor "Controlar todas las impresoras" retiene todas las colas, también las impresoras nuevas. En el registro, el motivo de lo que bloquea la regla global empieza con `Global · ` (por ejemplo `Global · Sitio: facebook.com`).
- Con el control en pausa, todo se libera sin evaluar. La pausa se guarda y sobrevive a reinicios.
- "Trabajos sin datos suficientes" (Ajustes) decide qué pasa con los trabajos sin nombre de documento: permitir (por defecto) o bloquear.
- Reglas por sitio: las aporta la extensión de navegador (ver `extension/README.md`). Los sitios no distinguen mayúsculas, cubren sus subdominios (`ejemplo.cl` incluye `www.ejemplo.cl`) y aceptan `*.dominio`. La app también acepta una URL completa y la reduce al dominio al comparar con el sitio del trabajo; a la extensión llegan solo en minúsculas y sin `*.` inicial, por lo que conviene escribirlos como dominio.

## Persistencia

En el directorio de datos de la app (macOS: `~/Library/Application Support/cl.cohasa.controlimpresion/`):

- `config.json`: reglas y ajustes. Se escribe de forma atómica (archivo temporal y renombrado), así un corte no lo deja truncado.
- `log.jsonl`: registro de actividad. Al iniciar se reescribe sin las entradas más viejas que la retención (90 días por defecto, configurable).
- `config.json.bak`: copia de la última configuración válida, que se actualiza en cada guardado.
- `managed.json`: colas que la app tiene retenidas. Al salir de la app se liberan todas; al iniciar se liberan las que quedaron retenidas sin regla activa (regla borrada, configuración restablecida, cierre inesperado), para no dejar trabajos en pausa.
- `app.pid`: PID de la app en ejecución; lo usa el proceso guardián (ver Cierre de la app).
- La pausa del control se guarda en `config.json`.
- Si `config.json` está corrupto, al iniciar se renombra a `config.json.corrupto-<marca de tiempo>`, se restaura `config.json.bak` (o se parte con valores por defecto si no hay respaldo) y se anota un aviso en el registro ("Configuración dañada: ...").
- Variable de entorno `CONTROL_IMPRESION_DIR`: si se define, la app usa esa carpeta en lugar del directorio de datos habitual. Sirve para probar sin tocar la configuración real.

## Cierre de la app

- La X de la ventana y Cmd+W ("Cerrar ventana" del menú Ventana) cierran la aplicación, no la ocultan, y al salir se liberan las colas. Salir desde el menú o la bandeja hace lo mismo.
- Al iniciar, la app lanza un proceso guardián (el mismo ejecutable con `--guardian <pid> <directorio>`), desacoplado de ella. Si la app muere a la fuerza, el guardián detecta que el proceso desapareció y libera las colas de `managed.json`. Si mientras tanto se abrió otra instancia (`app.pid` apunta a un proceso vivo distinto), deja que esa instancia gestione las colas.
- Así no quedan trabajos en pausa sin la app en marcha. La excepción es un apagón o un corte que mate también al guardián: al abrir la app de nuevo se retoma el control (los trabajos retenidos se evalúan, y las colas sin regla activa se liberan).

## Interfaz

- Menú nativo con atajos: Cmd/Ctrl+1 Impresoras, +2 Reglas, +3 Registro, +4 Ajustes, Cmd/Ctrl+, Ajustes.
- Bandeja del sistema con el estado ("Activo" o "En pausa", impresoras controladas y bloqueos de las últimas 24 h), abrir, pausar o reanudar y salir. Con el inicio automático (`--minimized`) la app arranca oculta en la bandeja (en macOS sin ícono en el Dock); cerrar la ventana cierra la app (ver Cierre de la app).
- Instancia única: abrir la app otra vez muestra la ventana existente.
- Exportar e importar reglas (Ajustes > Datos) y exportar el registro a CSV usan el selector de archivos del sistema.
- Extensión de navegador: en Ajustes, "Descargar extensión (.zip)" e "Instalar en navegador…" (ver `docs/instalacion.md`).
- Guía paso a paso para instalar la extensión: `docs/instalar-extension.md`.
- Inicio con el sistema (`--minimized`, arranca oculta en la bandeja) y notificaciones de bloqueo o de permiso configurables.
- Contraseña de administrador opcional en Ajustes; no viaja en la exportación. Con contraseña definida, los comandos de la interfaz que modifican estado (reglas, ajustes, pausa, importar o exportar archivos, instalar la extensión, borrar el registro) exigen haberla verificado.
