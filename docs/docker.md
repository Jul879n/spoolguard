# SpoolGuard con Docker

Guía práctica para levantar el servidor. Referencia completa de comportamiento, límites y seguridad: [modo-servidor.md](modo-servidor.md).

El contenedor corre CUPS mínimo (colas RAW) más `control-server`. Los PC imprimen a una cola IPP del servidor; el servidor retiene cada trabajo, lo evalúa y lo libera o lo cancela. Incluye la [protección previa](proteccion-previa.md) (backend `spoolguard`), así que no hay nada extra que activar.

## Requisitos

- Docker Engine con el plugin Compose v2 (`docker compose version`).
- Una máquina con IP fija (VPS, Proxmox, un PC siempre encendido). El contenedor corre sin root.
- Impresoras de red con puerto 9100 (`socket://`) o IPP. USB no aplica a este modo.
- Red: la impresora debe aceptar conexiones **solo desde el servidor** (ver [Restringir la impresora](#restringir-la-impresora)).

## Inicio rápido

La imagen se publica en `ghcr.io/jul879n/spoolguard` (`latest` y una etiqueta por versión). Los compose listos están en [`deploy/`](../deploy/).

```
curl -O https://raw.githubusercontent.com/Jul879n/spoolguard/main/deploy/compose.ghcr.yml
export CONTROL_ADMIN_PASSWORD='una-clave-larga'
export PRINTERS='Oficina=socket://192.168.1.50:9100'
docker compose -f compose.ghcr.yml up -d
curl -s http://127.0.0.1:8080/healthz      # responde 200 cuando está listo (el healthcheck espera hasta 15 s al arranque)
```

- Panel: `http://127.0.0.1:8080` (usuario único; contraseña = `CONTROL_ADMIN_PASSWORD`).
- Cola para los PC: `ipp://SERVIDOR:631/printers/Oficina`.

Para fijar una versión: `SPOOLGUARD_TAG=0.2.0 docker compose -f compose.ghcr.yml up -d`.

Para construir desde el código en lugar de bajar la imagen: `docker compose up -d --build` con el `docker-compose.yml` de la raíz del repositorio.

## Archivos compose

| Archivo | Uso |
|---|---|
| `deploy/compose.ghcr.yml` | Servidor con la imagen publicada. Punto de partida. |
| `deploy/compose.proxy.yml` | Igual, con Caddy delante para servir el panel por HTTPS. |
| `deploy/compose.secrets.yml` | Contraseña de administrador desde un archivo (Docker secrets). |
| `deploy/.env.example` | Variables de ejemplo: copiar a `deploy/.env` (no se versiona). |

Copia `.env.example` a `.env` y completa `CONTROL_ADMIN_PASSWORD` (vacío, `up` falla). Con un `.env`, `docker compose -f deploy/compose.ghcr.yml --env-file deploy/.env up -d` lo usa.

## Variables

| Variable | Efecto |
|---|---|
| `CONTROL_ADMIN_PASSWORD` | Contraseña de administrador. Obligatoria en los compose. Solo se usa si aún no hay una guardada; después se cambia en Ajustes (mínimo 8 caracteres). |
| `CONTROL_ADMIN_PASSWORD_FILE` | Ruta de un archivo con la contraseña (Docker secrets). Evita dejarla en `docker inspect`. |
| `PRINTERS` | Colas a crear al arrancar: `Nombre=uri;Nombre2=uri2`. Sin espacios, `/` ni `#` en el nombre. |
| `PANEL_BIND` | Interfaz donde se publica el panel (8080). Por defecto `127.0.0.1`. El panel habla HTTP plano: para abrirlo a la red usa HTTPS con proxy. |
| `IPP_BIND` | Interfaz donde se publica IPP (631). Por defecto `0.0.0.0`: es el servicio que usan los PC; limítalo con una IP o con el cortafuegos. |
| `CONTROL_TRUST_PROXY=1` | Toma la IP del cliente de `X-Forwarded-For`. Solo detrás de tu proxy; **no publiques el 8080** con esta opción. |
| `CONTROL_RELEASE_ON_EXIT=1` | Libera las colas al detener el servidor. Por defecto quedan retenidas: los trabajos esperan al servidor y nada se imprime sin control. |
| `SPOOLGUARD_VERDICT_TIMEOUT` | Segundos que el backend espera el veredicto antes de decidir sin él (1–600, por defecto 10; inválido = ignorado con aviso). |
| `SPOOLGUARD_STRICT=1` | Modo estricto: sin veredicto a tiempo el trabajo **no se imprime** (CUPS lo retiene y el servidor lo evalúa). Por defecto apagado (falla abierta). Ver [proteccion-previa.md](proteccion-previa.md). |
| `SPOOLGUARD_TAG` | Etiqueta de la imagen en los compose de `deploy/` (por defecto `latest`). |

## Agregar impresoras

Al arrancar, cada entrada de `PRINTERS` crea o actualiza una cola, equivalente a:

```
lpadmin -p NOMBRE -E -v spoolguard:socket://IP:9100 -m raw -o printer-is-shared=true
```

(el prefijo `spoolguard:` activa la protección previa). Las colas se reconstruyen en cada arranque: lo que no esté en `PRINTERS` se pierde salvo que persista en `/etc/cups`. Para agregar una a mano y probar:

```
docker compose -f deploy/compose.ghcr.yml exec control lpadmin -p Bodega -E -v spoolguard:socket://192.168.1.51:9100 -m raw -o printer-is-shared=true
docker compose -f deploy/compose.ghcr.yml exec control lpstat -p
```

Para dejarla fija, agrégala a `PRINTERS`. Luego, en el panel → Impresoras, activa el control de la cola.

## Conectar los PC

En cada PC se agrega una impresora que apunta al servidor: `ipp://SERVIDOR:631/printers/<cola>`.

- **Linux / macOS (CUPS):** `lpadmin -p Oficina -E -v ipp://SERVIDOR:631/printers/Oficina -m raw` (el controlador lo pone el PC; el servidor no convierte documentos).
- **Windows:** `Add-Printer -Name "Oficina (controlada)" -DriverName "Generic / Text Only" -PortName "http://SERVIDOR:631/printers/Oficina"`; para una impresora real, el controlador del fabricante. Requiere la característica «Cliente de impresión de Internet».

Qué está probado y qué no, por sistema: sección 3 de [modo-servidor.md](modo-servidor.md#uso-rápido).

## Restringir la impresora

Paso obligatorio: en el panel de la impresora (filtro de IP) o en el cortafuegos/VLAN, permitir 9100, 515 y 631 **solo desde la IP del servidor**. Comprobación desde otro equipo: `nc -vz IP_IMPRESORA 9100` debe fallar. Sin esto, quien imprima directo a la impresora se salta el control ([guia-red.md](guia-red.md)).

## HTTPS con proxy

`deploy/compose.proxy.yml` agrega Caddy, que obtiene el certificado solo y sirve el panel. Requiere un dominio que apunte al servidor y los puertos 80/443 abiertos.

```
export PANEL_DOMAIN=panel.ejemplo.cl
docker compose -f deploy/compose.proxy.yml up -d
```

En ese archivo el panel **no se publica**: solo Caddy lo alcanza por la red interna, y se activa `CONTROL_TRUST_PROXY=1` para registrar la IP real del cliente. IPP (631) se publica igual que en el compose base.

## Secretos

`deploy/compose.secrets.yml` lee la contraseña de `deploy/admin_password.txt` (no se versiona). El contenedor corre como uid 1000: el archivo debe ser legible por él (`644`, o `chown 1000`). Si no puede leerlo, el servidor genera una contraseña aleatoria en silencio: revisa `docker compose logs control`:

```
printf '%s' 'una-clave-larga' > deploy/admin_password.txt && chmod 644 deploy/admin_password.txt
docker compose -f deploy/compose.secrets.yml up -d
```

Sin variable ni archivo, el servidor genera una contraseña aleatoria y la imprime una sola vez en `docker compose logs control`.

## Respaldo

Todo el estado está en el volumen `control-data` (`/data`): `config.json`, `events.db`, `managed.json`, `quick.json`.

```
# respaldo
docker run --rm -v control-data:/data -v "$PWD":/backup alpine tar czf /backup/spoolguard-data.tgz -C /data .
# restauración (con el servidor detenido)
docker run --rm -v control-data:/data -v "$PWD":/backup alpine sh -c "cd /data && tar xzf /backup/spoolguard-data.tgz"
```

El nombre real del volumen lleva el prefijo del proyecto (`docker volume ls`; por ejemplo `deploy_control-data`). Las reglas también se exportan desde el panel (Ajustes → Datos).

## Actualizar

```
docker compose -f deploy/compose.ghcr.yml pull
docker compose -f deploy/compose.ghcr.yml up -d
```

Los datos se conservan en el volumen. Con `latest` se toma la última versión; con `SPOOLGUARD_TAG` fija, cambia la etiqueta.

## Solución de problemas

| Síntoma | Qué mirar |
|---|---|
| `CONTROL_ADMIN_PASSWORD` «define…» al hacer `up` | La variable es obligatoria en los compose: expórtala o ponla en `.env`. |
| El panel no abre desde otro equipo | Por defecto escucha solo en `127.0.0.1`. Usa el compose con proxy o define `PANEL_BIND`. |
| El contenedor queda `unhealthy` | `docker compose logs control`; `cupsd` debe arrancar (`entrypoint: cupsd no arrancó` indica un problema de permisos del volumen). |
| La cola no aparece | `docker compose logs control \| grep entrypoint`; revisa el formato `Nombre=uri` y que el nombre no tenga espacios. |
| Un PC imprime pero no se registra ni se controla | Comprueba que la impresora esté activada en Impresoras (control) y que imprima por la cola del servidor, no directo a la IP. |
| Todos los PC aparecen con la misma IP | Probable NAT entre ellos y el servidor (según [modo-servidor.md](modo-servidor.md), pasa con Docker en Mac): las reglas por IP no distinguen equipos. |
| Los trabajos esperan y no salen | Con el servidor detenido las colas quedan retenidas (por diseño). Arráncalo, o usa `CONTROL_RELEASE_ON_EXIT=1`. |
