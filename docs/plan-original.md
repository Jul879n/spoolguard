# Control de impresión (app de escritorio multiplataforma, cualquier impresora)

> Este es el plan inicial (solo escritorio). El plan vigente y completo está en [PLAN.md](PLAN.md).

## Contexto
Un cliente compró una TSC TH240 y necesita controlar quién imprime en ella. La app debe servir para **cualquier impresora** instalada en el equipo, no solo la TSC. Requisitos de la app:
- App de escritorio **muy liviana y rápida**, fácil de usar y con **UI moderna**.
- **Arranca con el sistema**, se minimiza y queda en la **barra de tareas o bandeja del sistema** (Windows: bandeja; macOS: barra de menús; Linux: bandeja/AppIndicator).
- Lista todas las impresoras del sistema y permite activar el control y definir reglas **por impresora**.

Las reglas se configuran:
- **Modo bloqueo** (lista negra) o **modo permitir** (lista blanca).
- Criterios: **aplicación** (p. ej. Word), **sitio web** (URL/dominio) e **IP/equipo de origen**.
- Cualquier conexión: USB, Ethernet o WiFi.
- Equipos Windows, macOS y Linux; desde un solo PC hasta varios PC, solo con la app de escritorio (sin servidor).

**Respuesta corta: sí es posible.** La clave es interceptar en la **cola de impresión del sistema operativo** (spooler en Windows, CUPS en macOS/Linux) y no en el cable. Todo trabajo pasa por la cola, sea USB, red o WiFi, así que la conexión deja de importar.

## Arquitectura
Una sola app de escritorio (UI + servicio en segundo plano) instalada en cada PC:

1. **Servicio de intercepción** (con privilegios, en segundo plano)
   - La cola de cada impresora controlada se configura para **retener** los trabajos por defecto.
   - El agente lee cada trabajo retenido (nombre del documento, aplicación, usuario, equipo/IP de origen), aplica las reglas y **libera** el trabajo o lo **cancela**.
   - **Windows**: API del spooler (`FindFirstPrinterChangeNotification`, `GetJob`/`SetJob`) con la cola en pausa o retenida; el agente reanuda o elimina cada trabajo.
   - **macOS/Linux**: CUPS con `job-hold-until-default=indefinite`; el agente lee los atributos IPP y libera o cancela (`lp -H resume` / `cancel`).
2. **Solo app de escritorio, sin servidor ni consola central.** Cada equipo se configura en su propia app, y las reglas se pueden exportar e importar en un archivo para copiarlas entre PC.
   - **Filtro por IP de origen**: aplica cuando la impresora está compartida desde un PC que tiene la app. Ese PC ve la IP de cada equipo que le envía trabajos.
   - Para que nadie se salte el control, la impresora se restringe para aceptar trabajos **solo desde el PC que la comparte**, mediante el filtro de IP de la impresora (si lo tiene) o con firewall/VLAN en los puertos 9100, 515 y 631. Así nadie se salta el control imprimiendo directo a la IP.
3. **Extensión de navegador** (Chrome/Edge/Firefox), complemento para los sitios web
   - El spooler solo recibe el título de la página, no la URL. Para bloquear un sitio de forma confiable, la extensión anula `window.print`, Ctrl+P y `@media print` en los dominios configurados. La app local le envía las reglas (native messaging).
4. **App de escritorio (UI)**
   - Ventana única y moderna, con tres vistas: **Impresoras** (lista del sistema, interruptor de control por cada una), **Reglas** (modo bloqueo/permitir; listas de apps, sitios e IP; agregar apps desde una lista de las instaladas) y **Actividad** (registro en vivo de trabajos permitidos y bloqueados).
   - Al cerrarla se minimiza a la bandeja; el menú de la bandeja muestra el estado, "Abrir", "Pausar control" y "Salir". Se notifica cada trabajo bloqueado.
   - Inicio automático configurable (plugin `autostart` de Tauri). El servicio sigue aplicando las reglas aunque la UI esté cerrada.
   - El acceso a la configuración queda protegido con contraseña de administrador.
   - Antes de codear la UI se presentará un mockup para aprobarlo.

## Stack recomendado
- **Tauri 2 (Rust)**: instaladores de unos 5 a 10 MB y bajo consumo de RAM, porque usa el webview del sistema en vez de Chromium como Electron. Bandeja, autostart y notificaciones vienen como plugins oficiales. La UI va en HTML/TS (Svelte o vanilla) y el servicio en Rust (crate `windows` para el spooler, cliente IPP para CUPS).
- Impresoras del sistema: `EnumPrinters` en Windows; `lpstat`/IPP en CUPS.
- Instaladores: MSI (Windows), PKG (macOS), DEB/RPM (Linux). Todos registran el servicio y configuran la cola en modo retención.

## Pasos de implementación
1. PoC en Windows: retener y liberar o cancelar trabajos de una impresora elegida (probando con la TH240 y otra impresora común) según el nombre del documento (Word → `*.docx`/"Microsoft Word").
2. PoC en CUPS (macOS/Linux), con la misma lógica.
3. Motor de reglas común (bloqueo/permitir; app, sitio, IP), guardado en JSON firmado.
4. Extensión de navegador para los sitios.
5. Filtro por IP en el PC que comparte la impresora, exportación e importación de reglas, y guía de restricción de red de la impresora.
6. UI Tauri, registro de trabajos e instaladores.

## Por verificar antes de codear (no asumido)
- Qué campos expone realmente cada SO para identificar la **aplicación** de origen. Windows no entrega el proceso en `JOB_INFO`; el nombre del documento de Word puede variar según versión e idioma. Si no alcanza, habría que escribir un *print processor* en C++ (más pesado).
- Si CUPS en macOS entrega un atributo con la aplicación de origen o solo el `job-name`.
- Si la TH240 trae filtrado de IP en su panel web. Si no lo trae, se resuelve con firewall/VLAN del cliente.
- Cómo se comportan distintos drivers (TSC/Seagull, drivers genéricos, impresoras de red IPP/AirPrint) con la cola retenida.
- Soporte de bandeja en Linux: depende del entorno de escritorio (en GNOME requiere la extensión AppIndicator).

## Limitaciones a comunicar al cliente
- Un usuario con permisos de administrador puede desinstalar el agente o cambiar la cola. Se requieren cuentas estándar.
- Los dispositivos sin agente (celulares, PC externos) solo quedan controlados si imprimen a través del PC que comparte la impresora y la red bloquea el acceso directo. Ese PC debe permanecer encendido.
- Bloquear sitios por URL depende de la extensión de navegador.

## Verificación
- Imprimir desde Word, desde el sitio configurado, desde otra app (p. ej. BarTender o el Bloc de notas) y desde otra IP, en cada modo y en cada SO. Revisar que el registro y la impresora reflejen lo esperado.
- Repetir con la impresora conectada por USB, por Ethernet y por WiFi.
- Intentar imprimir directo a `IP:9100` desde un equipo sin la app, con la restricción de red activa; debe ser rechazado.
