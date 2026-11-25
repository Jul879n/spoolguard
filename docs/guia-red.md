# Guía de red para el técnico

Objetivo: que la impresora solo reciba trabajos del PC que la comparte (el que ejecuta SpoolGuard), de modo que todo trabajo pase por la cola donde la app lo retiene y aplica las reglas.

Leyenda de verificación usada en esta guía:

- **[Verificado]**: comprobado en un Mac (macOS, `pfctl -h`, `cupsctl --help`, `cupsctl`, lectura del código de la app) o en CI (CUPS real en Linux, spooler en Windows).
- **[No verificado]**: comando o comportamiento tomado de la documentación habitual del sistema; probar en un equipo de pruebas antes de desplegar.

Reemplazar en los ejemplos:

- `192.168.1.50` = IP de la impresora.
- `192.168.1.10` = IP del PC que comparte la impresora (el "PC compartidor").
- `192.168.1.0/24` = red local.

## 1. Cómo funciona y por qué importa la red

La app retiene los trabajos en la cola del sistema del PC compartidor (CUPS en macOS/Linux, spooler en Windows) y los libera o cancela según las reglas. **[Verificado en el código]** En macOS/Linux la retención se activa con `lpadmin -p <impresora> -o job-hold-until-default=indefinite`, así los trabajos nacen retenidos y ninguno se escapa antes de evaluarse. Para cubrir todas las colas del PC con una sola regla, usar en Reglas la opción "★ Todas las impresoras (global)" con el interruptor "Controlar todas las impresoras": retiene todas las colas y la regla global se suma a la de cada impresora (si cualquiera bloquea, se bloquea; en el registro el motivo aparece como `Global · ...`).

Si un equipo imprime directo a la impresora (puerto 9100, 515 o 631 de la impresora), el trabajo nunca pasa por la cola del PC compartidor y las reglas no se aplican. Por eso hay que cerrar esos accesos directos.

Requisito previo recomendado: IP fija para la impresora (reserva DHCP en el router o IP estática en la impresora), para que las reglas de abajo no queden obsoletas.

Soporte por sistema: macOS y Linux usan CUPS (en Linux, probado contra un CUPS real en CI). En Windows el control de cola (`core/src/spooler.rs`) se probó en un runner real de CI con una impresora virtual (Microsoft Print to PDF), pero la interfaz no se ha probado en un Windows real; validar con la impresora del cliente antes de desplegar.

Impresión directa: la app solo ve lo que entra por la cola del PC donde corre. Un trabajo enviado por IP al puerto 9100 de la impresora no pasa por la cola y no se puede controlar desde la app; solo se evita con firewall o VLAN (secciones 3 y 4).

## 2. Filtro de IP en el panel web de la impresora

Muchas impresoras de red traen una página de administración con una lista de IP permitidas (a veces llamada "IP filter", "Access control" o "Allowed hosts"). Pasos generales:

1. Abrir `http://192.168.1.50` en un navegador.
2. Buscar la sección de red, seguridad o control de acceso.
3. Permitir solo la IP del PC compartidor y guardar.

Impresora TSC TH240: **no se afirma que tenga este filtro. Debe verificarse** en su manual o en su panel/utilidad de configuración. Si no existe, la restricción debe hacerse con firewall o VLAN (secciones 3 y 4). Si el panel pide contraseña, cambiar la de fábrica.

## 3. Reglas de firewall (puertos 9100, 515 y 631)

Puertos de impresión: 9100 (RAW/JetDirect), 515 (LPD), 631 (IPP).

Hay dos lugares donde aplicar reglas:

- **En cada PC cliente (con agente o sin él):** bloquear la salida hacia la impresora. Así no pueden imprimir directo.
- **En el router/firewall de la red** (lo más robusto, cubre también equipos sin agente): permitir hacia la impresora solo desde la IP del PC compartidor y bloquear el resto. La sintaxis depende del equipo; no está cubierta aquí.

No aplicar el bloqueo de salida en el PC compartidor: ahí debe quedar permitido.

### 3.1 macOS (pf)

pf viene desactivado por defecto. Se usa un anchor propio para no tocar el resto de `/etc/pf.conf`. **[Verificado]** `pfctl` existe en este Mac y acepta las opciones `-f`, `-e`, `-a`, `-s`; la aplicación real de reglas requiere root y no se probó aquí. El resto es **[No verificado]**.

Archivo `/etc/pf.anchors/cl.cohasa.impresion`:

```
block drop out quick proto tcp from any to 192.168.1.50 port { 9100, 515, 631 }
```

Agregar al final de `/etc/pf.conf`:

```
anchor "cl.cohasa.impresion"
load anchor "cl.cohasa.impresion" from "/etc/pf.anchors/cl.cohasa.impresion"
```

Cargar y activar:

```
sudo pfctl -n -f /etc/pf.conf     # solo valida la sintaxis, no aplica
sudo pfctl -f /etc/pf.conf
sudo pfctl -e
sudo pfctl -a cl.cohasa.impresion -s rules   # ver reglas cargadas
```

Limitaciones: pf no se activa solo tras reiniciar (se necesita un LaunchDaemon que ejecute `pfctl -e -f /etc/pf.conf`), y las actualizaciones de macOS pueden reemplazar `/etc/pf.conf`. Alternativa más simple en redes administradas: aplicar la restricción en el router.

### 3.2 Linux (ufw o iptables) **[No verificado: no hay ufw ni iptables en este Mac]**

Con ufw, en cada PC cliente:

```
sudo ufw deny out to 192.168.1.50 port 9100 proto tcp
sudo ufw deny out to 192.168.1.50 port 515 proto tcp
sudo ufw deny out to 192.168.1.50 port 631 proto tcp
sudo ufw status numbered
```

Con iptables:

```
sudo iptables -A OUTPUT -d 192.168.1.50 -p tcp -m multiport --dports 9100,515,631 -j REJECT
sudo iptables -L OUTPUT -n --line-numbers
```

Las reglas de iptables se pierden al reiniciar salvo que se persistan (por ejemplo con `iptables-persistent`/`netfilter-persistent` en Debian/Ubuntu).

En el PC compartidor, si usa ufw y comparte la cola por IPP, permitir la entrada solo desde la LAN:

```
sudo ufw allow from 192.168.1.0/24 to any port 631 proto tcp
```

### 3.3 Windows (Windows Defender Firewall con netsh) **[No verificado: no hay Windows en este equipo]**

En cada PC cliente, desde un símbolo del sistema como administrador:

```
netsh advfirewall firewall add rule name="Bloquear impresion directa" dir=out action=block protocol=TCP remoteip=192.168.1.50 remoteport=9100,515,631
netsh advfirewall firewall show rule name="Bloquear impresion directa"
```

Para quitarla:

```
netsh advfirewall firewall delete rule name="Bloquear impresion directa"
```

Estas reglas se pueden distribuir por directiva de grupo (GPO) si hay dominio.

## 4. VLAN

La opción más sólida para impedir la impresión directa de dispositivos sin agente:

1. Crear una VLAN de impresión y colocar en ella la impresora y el PC compartidor.
2. En el router o switch capa 3, permitir tráfico hacia la VLAN de impresión solo desde la IP del PC compartidor (puertos 9100/515/631) y bloquear el resto.
3. Los clientes imprimen contra la cola compartida del PC compartidor (sección 5), que sí está accesible desde la LAN (puerto 631 en CUPS; SMB en Windows).

Requiere switch administrable y router con ACL; el procedimiento depende de la marca.

## 5. Compartir la cola desde el PC con la app

### macOS / Linux (CUPS)

```
sudo cupsctl --share-printers
```

**[Verificado]** `cupsctl --share-printers` existe (en este Mac `cupsctl` muestra `_share_printers=0`, es decir, hoy desactivado). Para compartir una impresora concreta (**[No verificado]**):

```
sudo lpadmin -p NOMBRE_IMPRESORA -o printer-is-shared=true
lpstat -p -l
```

Para que solo la LAN llegue al puerto 631, combinar con las reglas de la sección 3 (entrada permitida desde `192.168.1.0/24`). **No** usar `cupsctl --remote-any` (permite acceso desde internet).

Los clientes agregan la impresora por IPP: `ipp://192.168.1.10:631/printers/NOMBRE_IMPRESORA` **[No verificado]**.

### Windows

Panel de control > Dispositivos e impresoras > clic derecho en la impresora > Propiedades de impresora > pestaña "Compartir" > marcar "Compartir esta impresora" y dar un nombre. Alternativa en PowerShell como administrador **[No verificado]**:

```
Set-Printer -Name "NOMBRE_IMPRESORA" -Shared $true -ShareName "NOMBRE_COMPARTIDO"
```

Los clientes se conectan a `\\192.168.1.10\NOMBRE_COMPARTIDO`. Recordar el aviso de la sección 1: la interfaz en Windows no se ha probado en un equipo real.

## 6. Limitaciones

- **Administradores locales:** quien sea administrador de su PC puede desinstalar el agente, cambiar el firewall o quitar la retención. Usar cuentas estándar para los usuarios y reservar la cuenta administradora al técnico. La app también permite definir una contraseña de administrador (Ajustes): con ella, los cambios hechos desde la interfaz exigen verificarla, pero no impide que un administrador del sistema la desinstale.
- **Dispositivos sin agente:** teléfonos, tabletas y PCs sin el agente no aplican reglas por aplicación o sitio. Solo se controlan si imprimen a través de la cola del PC compartidor; el resto se bloquea únicamente con firewall o VLAN (secciones 3 y 4).
- **PC compartidor encendido:** si está apagado, suspendido o sin sesión de usuario iniciada, nadie imprime. **[Verificado en el código]** en macOS el inicio automático usa un LaunchAgent (corre en la sesión del usuario, no como servicio de sistema), así que la app solo controla mientras haya una sesión iniciada. Cerrar la ventana de la app (X o Cmd+W) la cierra y libera las colas: para que el control siga activo, la ventana puede minimizarse o la app debe arrancar con el inicio automático (oculta en la barra de menús). Configurar inicio de sesión automático y desactivar la suspensión en ese PC.
- **Bloqueo de sitios:** depende de la extensión del navegador (el host de Native Messaging es la propia app y se registra solo; la extensión se carga una vez a mano en Chrome, ver `extension/README.md`); sin ella, la regla por sitio no tiene datos. Probada solo en Chrome; Edge y Firefox sin probar. **[Verificado en el código de la app]** CUPS no entrega la aplicación de origen, solo nombre del trabajo, usuario y host de origen, por lo que las reglas por aplicación o sitio pueden no tener datos suficientes. Con "Trabajos sin datos suficientes" en "Permitir", esos trabajos se imprimen; en "Bloquear" se cancelan.
- Las reglas de firewall locales se pueden quitar con privilegios de administrador; la restricción en router o VLAN no depende del PC del usuario.
