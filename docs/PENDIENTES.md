# Pendientes de SpoolGuard

Estado al 2026-11-27. Lo hecho y verificado está en [PLAN.md](PLAN.md) y [proteccion-previa.md](proteccion-previa.md). Aquí solo lo que falta, con el motivo.

## Verificación que requiere equipos reales

| Pendiente | Qué falta | Qué ya se probó |
|---|---|---|
| Windows: clic real en «Activar protección» (UAC) | Hacerlo con un usuario real y confirmar la elevación | Instalación y protección por script en VM Win10; MSI compilado |
| Windows: colas con driver V4 | Nada por medir: el límite está medido y documentado (el procesador no las cubre; solo retención de cola) | Medido 2026-11-27 en la VM (`Microsoft Print To PDF` con puerto de archivo, app elevada, trabajos reales con `PrintDocument`): la cola queda en pausa; 10 `.docx` bloqueados con 0 bytes de salida y 10 `.txt` permitidos con salida; 0 fugas. Repetido con un MSI compilado del árbol actual: mismo resultado en la segunda corrida; en la primera, los 10 `.txt` no dejaron salida (no se reprodujo y no se pudo explicar; los `.docx` tampoco fugaron). El e2e `scripts/e2e_windows.ps1` no sirve para V4: envía RAW por `PORTPROMPT:` (error 50 sin sesión interactiva) y requiere la app elevada |
| Windows 11 y Server | Repetir las pruebas | Solo Win10 (VirtualBox): estricto con app viva, 8 rondas, 3.360 trabajos, 0 fugas |
| Windows: latencia base y Word/Excel | Medir con aplicaciones reales | Trabajos sintéticos |
| Cliente remoto de Windows (impresora compartida por SMB) | Una impresora compartida y un segundo equipo | Regla por IP verificada con clientes remotos simulados en contenedores Docker |
| Impresión directa por IPP con hardware | Una impresora de red real | Detección simulada (macOS, Linux, Windows) |
| Impresoras físicas (todas las plataformas) | Probar con USB, Ethernet y WiFi reales | Colas virtuales y listeners de prueba |
| Linux con sesión gráfica real | Un equipo Linux con escritorio | App `release` bajo Xvfb con CUPS real: 0 de 5 `.docx` impresos, 5 de 5 `.xlsx` |
| Instaladores reales (.msi, .dmg, .deb, .rpm) | Instalar en equipos limpios y revisar el arranque con el sistema | Compilación de la app; MSI generado en la VM |
| macOS estricto: repetir la medición de la fuga a los ~10 s | Nada | **Medido 2026-11-27**, cola `SGMacTest` (backend de prueba por socket), app cerrada, `timeout=5`, `strict=1`, 3 rondas: a los 30 s sin veredicto 0 impresos en las 3; tras `allow` 1 (rondas 1 y 3); tras `deny` 0 (ronda 2); 0 pendientes. Sin fuga |

Script de la prueba de macOS: `~/Library/Application Support/spoolguard-prueba/estricto.sh` (ejecutado el 2026-11-27; elimina la cola temporal al terminar).

## Publicación (espera aprobación)

El usuario respondió «todavía no». No hacer commit, push ni tag hasta que lo pida.

1. Revisar el diff completo, incluidos los cambios de otras sesiones.
2. Commit, push y tag v0.3.0.
3. Verificar el CI de firma y las firmas de cada artefacto.
4. Firmar la imagen Docker con cosign (la imagen pesa 70,3 MB; la meta de 40 MB se descartó).
5. SmartScreen: Authenticode con SignPath o un certificado de pago, y respaldo de la clave privada de firma de las actualizaciones.

## Residuos de pruebas en el Mac del desarrollador

- Filtro de las 5 colas protegidas actualizado el 2026-11-27 con `guard-cups.sh install` (el filtro ya incluye la marca `controlled-`). `/Applications/SpoolGuard.app` reinstalada el 2026-11-27 con la compilación del árbol actual.
- Colas de prueba con `/dev/null` que pueden ser de otras sesiones (no se tocaron): `CtrlSrvTest`, `CtrlTest`, `InvArchivo`, `InvRed`, `SGDbg`.
- VM Win10 `win10-test` apagada; el runner de GitHub Actions está detenido.

## Hallazgos menores del code-review (2026-11-27, sin corregir)

Corregidos: purga de 24 h que borraba las marcas `controlled-*`, retención por defecto del servidor (vuelve a 365 días) y el texto del estricto en macOS/Linux (la espera no tiene límite; el plazo solo aplica en Windows y Docker).

- `setSettings` (Go) y `set_settings` (Rust) rechazan `verdict_timeout_secs` fuera de 1–600 en vez de ajustarlo.
- `SPOOLGUARD_STRICT` y `SPOOLGUARD_VERDICT_TIMEOUT` pisan lo guardado en cada arranque y la UI no lo indica.
- `docker/entrypoint.sh`: el reintento de `lpadmin` suma ~10 s por impresora inválida y oculta el error.
- `docker/Dockerfile`: borra bibliotecas compartidas sin validar con `ldd` los binarios de CUPS.

## Paridad

La regla por IP de Windows resuelve el nombre del equipo de origen a IP (límite de 1 s, con caché); falta probarla con un cliente remoto real.
