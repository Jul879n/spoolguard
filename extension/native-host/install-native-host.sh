#!/bin/sh
# Registra el host nativo (usuario actual) para Chrome, Chromium, Edge y Firefox.
# Uso: install-native-host.sh /ruta/a/native_host ID_EXTENSION_CHROMIUM
set -eu
[ $# -eq 2 ] || { echo "Uso: $0 /ruta/absoluta/native_host ID_EXTENSION_CHROMIUM" >&2; exit 1; }
HOST=$1; EXT_ID=$2
case $HOST in /*) ;; *) echo "La ruta del host debe ser absoluta" >&2; exit 1;; esac
[ -x "$HOST" ] || { echo "No existe o no es ejecutable: $HOST" >&2; exit 1; }
NAME=cl.cohasa.controlimpresion
AQUI=$(cd "$(dirname "$0")" && pwd)

if [ "$(uname)" = Darwin ]; then
  AS="$HOME/Library/Application Support"
  CHROMIUM_DIRS="$AS/Google/Chrome/NativeMessagingHosts|$AS/Chromium/NativeMessagingHosts|$AS/Microsoft Edge/NativeMessagingHosts"
  FIREFOX_DIR="$AS/Mozilla/NativeMessagingHosts"
else
  CHROMIUM_DIRS="$HOME/.config/google-chrome/NativeMessagingHosts|$HOME/.config/chromium/NativeMessagingHosts|$HOME/.config/microsoft-edge/NativeMessagingHosts"
  FIREFOX_DIR="$HOME/.mozilla/native-messaging-hosts"
fi

generar() { # plantilla destino
  mkdir -p "$(dirname "$2")"
  sed -e "s|/RUTA/ABSOLUTA/native_host|$HOST|" -e "s|ID_DE_LA_EXTENSION|$EXT_ID|" "$1" > "$2"
  chmod 644 "$2"; echo "Instalado: $2"
}

OLD_IFS=$IFS; IFS='|'
for d in $CHROMIUM_DIRS; do generar "$AQUI/$NAME.chrome.json" "$d/$NAME.json"; done
IFS=$OLD_IFS
generar "$AQUI/$NAME.firefox.json" "$FIREFOX_DIR/$NAME.json"
