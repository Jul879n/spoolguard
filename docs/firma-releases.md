# Firma y verificación de releases

Cada release de SpoolGuard publica, además de los instaladores, archivos que permiten comprobar que no fueron alterados ni reemplazados:

| Archivo | Para qué sirve |
|---|---|
| `<instalador>.minisig` | Firma [minisign](https://jedisct1.github.io/minisign/) (Ed25519) de ese instalador. |
| `SHA256SUMS` | SHA-256 de todos los instaladores y de la extensión. |
| `SHA256SUMS.minisig` | Firma minisign de `SHA256SUMS`. |
| Atestación de procedencia | Registro firmado con Sigstore (GitHub) de que el archivo salió del workflow `release.yml` de este repositorio. |

## Clave pública

```
untrusted comment: minisign public key: D32219095D4FB0F6
RWT2sE9dCRki0zPwG/uvsyUeU11Ckbk+/Y5E1NimghhGqIBAep88Sl+K
```

ID de clave: `D32219095D4FB0F6`. Es la misma que está embebida en la app (`PUBLIC_KEY` en `core/src/update.rs`) y en el workflow (`SPOOLGUARD_PUBLIC_KEY`); el workflow falla si dejan de coincidir. La clave privada no está en el repositorio.

## Qué hace la app al actualizar

Ajustes → Versión → **Buscar actualización** consulta `api.github.com` solo cuando se pulsa (nunca en segundo plano). Si hay una versión nueva y existe instalador para el sistema, **Descargar e instalar**:

1. Descarga solo de `https://github.com/Jul879n/spoolguard/releases/download/…` (con redirección únicamente a los CDN de GitHub Releases), con tope de 200 MiB.
2. Verifica la firma de `SHA256SUMS`, que el SHA-256 del archivo coincida con su línea y la firma `.minisig` del instalador (que debe nombrar exactamente ese archivo).
3. Si falla cualquier comprobación, borra el archivo y muestra el motivo. Un release sin firmas no se descarga.
4. Si todo pasa, abre el instalador con el sistema (Windows: `msiexec /i`; macOS: abre el `.dmg`; Linux: abre el `.deb`/`.rpm` con el gestor de paquetes). Nada se instala sin que la persona complete el instalador.

Los archivos quedan en la carpeta de datos de la app, subcarpeta `updates/`.

## Verificar a mano

Descargar el instalador, `SHA256SUMS`, `SHA256SUMS.minisig` y el `.minisig` del instalador en la misma carpeta.

```bash
# 1) Firma de un instalador (requiere minisign: https://jedisct1.github.io/minisign/)
minisign -V -P RWT2sE9dCRki0zPwG/uvsyUeU11Ckbk+/Y5E1NimghhGqIBAep88Sl+K -m SpoolGuard-X.Y.Z-macos-arm64.dmg

# 2) Firma de SHA256SUMS y suma del archivo
minisign -V -P RWT2sE9dCRki0zPwG/uvsyUeU11Ckbk+/Y5E1NimghhGqIBAep88Sl+K -m SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS          # macOS: shasum -a 256 -c --ignore-missing SHA256SUMS

# 3) Procedencia (requiere GitHub CLI)
gh attestation verify SpoolGuard-X.Y.Z-macos-arm64.dmg --repo Jul879n/spoolguard
```

`minisign -V` debe terminar con `Signature and comment signature verified` y un comentario `file:<nombre del archivo>`. En Windows, `minisign` y `gh` se descargan como ejecutables; en PowerShell: `(Get-FileHash archivo.msi -Algorithm SHA256).Hash` y comparar con la línea de `SHA256SUMS`.

## Limitación importante

La firma minisign la verifica **la app de SpoolGuard** (o quien siga los pasos de arriba), no el sistema operativo. Sin un certificado de firma de código, **Windows SmartScreen seguirá advirtiendo** al ejecutar el `.msi` y **macOS Gatekeeper seguirá bloqueando** la app no notarizada (ver [instalacion.md](instalacion.md)). La firma protege contra archivos alterados o reemplazados en la descarga y en la cadena de publicación, y no sustituye a Authenticode ni a la notarización de Apple.

El workflow incluye el paso «Firmar MSI (Authenticode)» preparado y **desactivado**: se activa solo si existen los secretos `WINDOWS_CERT_PFX_BASE64` y `WINDOWS_CERT_PASSWORD` (certificado `.pfx` en base64 y su contraseña). Alternativa gratuita para proyectos de código abierto: SignPath Foundation. No se ha probado con un certificado real.

## Cargar el secreto de firma (una vez, quien administra el repositorio)

La clave privada y su contraseña están en `~/Documents/herramientas/spoolguard-keys/` del equipo donde se generó (`spoolguard.key`, `clave-password.txt`; permisos 0600). Guardar copia en un gestor de contraseñas: **si se pierde, hay que generar otra clave, cambiar la pública en la app y publicar una versión nueva que la lleve** (las apps instaladas con la clave anterior no podrán verificar actualizaciones con la nueva).

```bash
cd ~/Documents/herramientas/spoolguard-keys
gh secret set SPOOLGUARD_SIGNING_KEY < spoolguard.key
gh secret set SPOOLGUARD_SIGNING_KEY_PASSWORD < clave-password.txt
```

O en la web: Settings → Secrets and variables → Actions → New repository secret, con esos dos nombres. El workflow `release.yml` usa los secretos solo en el job `sign`, que corre únicamente al subir una etiqueta `v*`.

Para publicar un release firmado: subir la etiqueta (`git tag vX.Y.Z && git push origin vX.Y.Z`); el job `sign` genera los `.minisig`, `SHA256SUMS` y la atestación (permisos `id-token: write` y `attestations: write`), y `publish` los adjunta al release.

## Generar otra clave

```bash
cargo install rsign2                      # o usar minisign: minisign -G
rsign generate -p spoolguard.pub -s spoolguard.key
```

Reemplazar la clave pública en `core/src/update.rs`, en `.github/workflows/release.yml` y en este documento, y volver a cargar los secretos. `*.key`, `*.pem`, `*.pfx` y `clave-password*` están en `.gitignore`.

## Dependencias de verificación

La app usa el crate [`minisign-verify`](https://crates.io/crates/minisign-verify) 0.3 (licencia MIT, compatible con Apache-2.0; sin dependencias propias) y `sha2`. El CI firma con `minisign` 0.12 (versión y SHA-256 fijados en el workflow).
