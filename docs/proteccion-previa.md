# Protección previa a la impresión

Aplica a macOS y Linux (escritorio) y a la imagen Docker. **Windows no la necesita**: el spooler retiene los trabajos.

## Por qué existe

SpoolGuard retiene la cola (`job-hold-until-default`) y evalúa cada trabajo antes de liberarlo. Pero algunas apps, como Microsoft Word, envían cada trabajo con `job-hold-until=no-hold`, que anula la retención de la cola. Medido: el trabajo nace directamente en estado `processing` y puede alcanzar la impresora antes de que la app lo evalúe.

La protección previa cierra ese hueco: hace que cada trabajo espere, **dentro de CUPS y antes de enviar datos a la impresora**, un veredicto de SpoolGuard.

## Cómo funciona

1. Cada trabajo nuevo pasa por una cadena de filtros de CUPS. En macOS/Linux, `scripts/guard-cups.sh` envuelve el filtro final de la cola con un filtro propio (`spoolguard-<filtro>`), modificando el PPD de la cola.
2. El filtro espera un archivo `<cola>-<id del trabajo>` en la carpeta de veredictos:
   - macOS: `/Library/Application Support/SpoolGuard/verdicts`
   - Linux y Docker: `/var/lib/spoolguard/verdicts`
3. SpoolGuard evalúa el trabajo contra las reglas y escribe `allow` o `deny` en ese archivo.
4. Con `deny`, el filtro descarta los datos (el trabajo termina sin imprimir y se anota «bloqueado por SpoolGuard»). Con `allow`, el trabajo continúa por el filtro original.
5. **Falla abierta:** si no hay veredicto en **10 segundos** (app cerrada, bloqueada o con error), el trabajo se imprime. Una app caída no debe parar la impresión de toda la oficina.

En Docker el mismo mecanismo lo cumple un backend de CUPS (`docker/spoolguard-backend`): las colas se crean con `device-uri` `spoolguard:socket://IP:9100`, el backend espera el veredicto y luego delega en el backend real. Ya viene incluido en la imagen; no hay nada que activar (ver [docker.md](docker.md)).

## Activarla

**Desde la app (recomendado):** asistente «Configuración de inicio», paso 1 «Permiso de administrador», o Ajustes → «Protección previa a la impresión» → **Activar**. Pide la contraseña de administrador una vez (macOS: diálogo del sistema; Linux: `pkexec`).

**A mano**, como administrador:

```
sudo scripts/guard-cups.sh install            # todas las colas que tienen PPD
sudo scripts/guard-cups.sh install MiCola     # solo esa cola
scripts/guard-cups.sh status                  # imprime «protegidas total», por ejemplo «3 3»
sudo scripts/guard-cups.sh uninstall          # restaura los PPD originales
```

El script (`install`):

- crea la carpeta de filtros propios (macOS: `/Library/Printers/SpoolGuard`; Linux: la carpeta de filtros de CUPS, porque AppArmor/SELinux solo dejan ejecutar filtros desde ahí) y la carpeta de veredictos (permiso `1777`);
- genera un envoltorio por cada filtro y reescribe la línea `*cupsFilter` del PPD de la cola para usarlo (`lpadmin -p COLA -P ...`);
- omite las colas sin PPD y las ya protegidas.

Variables:

| Variable | Uso |
|---|---|
| `SG_OWNER` | Usuario que ejecuta la app y escribe los veredictos. Por defecto `SUDO_USER`. |
| `SG_LPADMIN` | Ruta o comando de `lpadmin` a usar (por defecto `lpadmin`). |

Si agregas una impresora después, vuelve a activar la protección para incluirla.

## Límites

- **Colas sin PPD** (por ejemplo colas «raw» creadas a mano en escritorio) no tienen filtros que envolver: se omiten y quedan solo con la retención de cola. En Docker las colas son siempre raw y usan el backend, por lo que sí quedan cubiertas.
- No protege la **impresión directa** a la IP de la impresora (puerto 9100): eso se resuelve con firewall o VLAN ([guia-red.md](guia-red.md)).
- La falla abierta es deliberada: si necesitas el comportamiento contrario, no está disponible.
