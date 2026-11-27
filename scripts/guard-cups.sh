#!/bin/sh
# SpoolGuard — filtro previo a la impresión para colas de CUPS (macOS y Linux).
# Uso (como root):  guard-cups.sh install [cola ...]   (sin colas: todas las que tienen PPD)
#                   guard-cups.sh uninstall [cola ...]
#                   guard-cups.sh status [cola ...]    (imprime «protegidas total»)
# Cada trabajo espera, dentro de CUPS y antes de enviar datos a la impresora, el veredicto de la app
# (carpeta de veredictos). Sin veredicto en 10 s se imprime: la app cerrada no debe parar la impresión.
# Configurable desde Ajustes de la app (archivo «config» de la carpeta de veredictos: timeout=<1-600 s>, strict=<0|1>;
# inválido o ausente = 10 s y falla abierta). Con strict=1 el filtro NO vence por tiempo: espera el veredicto sin límite (CUPS no puede retener un trabajo desde un filtro,
# y salir con error lo aborta y lo pierde); la cola queda ocupada por ese trabajo hasta que la app responda o se cancele.
# Variable SG_OWNER: usuario que ejecuta la app (escribe los veredictos); por defecto SUDO_USER.
set -eu
cmd="${1:-}"; [ $# -gt 0 ] && shift
OS="$(uname -s)"
if [ "$OS" = Darwin ]; then
  FILTERS=/usr/libexec/cups/filter
  BIN="/Library/Printers/SpoolGuard"
  DATA="/Library/Application Support/SpoolGuard"
else
  FILTERS=""
  for d in /usr/lib/cups/filter /usr/libexec/cups/filter; do [ -d "$d" ] && FILTERS="$d" && break; done
  BIN="$FILTERS"          # AppArmor/SELinux solo dejan ejecutar filtros desde la carpeta de CUPS
  DATA=/var/lib/spoolguard
fi
PFX=spoolguard-
OWNER="${SG_OWNER:-${SUDO_USER:-root}}"
[ -n "$FILTERS" ] || { echo "No se encontró la carpeta de filtros de CUPS." >&2; exit 1; }

queues="$*"
if [ -z "$queues" ]; then queues="$(lpstat -e 2>/dev/null || true)"; fi

if [ "$cmd" != status ] && [ "$(id -u)" != 0 ]; then echo "Ejecuta como administrador (root)." >&2; exit 1; fi

mkwrapper() { # $1 = nombre del filtro original
  w="$BIN/$PFX$1"
  cat > "$w" <<WRAP
#!/bin/sh
# SpoolGuard: espera el veredicto de la app antes de pasar el trabajo al filtro original.
J="\$1"
V="$DATA/verdicts/\${PRINTER}-\${J}"
T=10; S=0
C="$DATA/verdicts/config"
if [ -f "\$C" ]; then
  t=\$(sed -n 's/^timeout=\\([0-9]\\{1,3\\}\\)\$/\\1/p' "\$C" 2>/dev/null | head -n 1)
  s=\$(sed -n 's/^strict=\\([01]\\)\$/\\1/p' "\$C" 2>/dev/null | head -n 1)
  if [ -n "\$t" ] && [ "\$t" -ge 1 ] && [ "\$t" -le 600 ]; then T="\$t"; fi
  [ "\$s" = 1 ] && S=1
fi
# Solo las colas con control (marca que escribe la app) esperan el veredicto; las demás pasan sin espera.
[ -f "$DATA/verdicts/controlled-\${PRINTER}" ] || exec "$FILTERS/$1" "\$@"
v=""; i=0
while [ "\$S" = 1 ] || [ \$i -lt \$((T * 10)) ]; do
  if [ -f "\$V" ]; then v=\$(cat "\$V" 2>/dev/null); break; fi
  sleep 0.1; i=\$((i+1))
done
if [ "\$v" = deny ]; then
  echo "INFO: bloqueado por SpoolGuard" >&2
  cat >/dev/null 2>&1
  exit 0
fi
exec "$FILTERS/$1" "\$@"
WRAP
  chmod 755 "$w"
  if [ "$OS" = Darwin ]; then chown root:wheel "$w"; else chown root:root "$w"; fi
}

protected() { grep -q "$PFX" "/etc/cups/ppd/$1.ppd" 2>/dev/null; }

case "$cmd" in
status)
  n=0; t=0
  for q in $queues; do
    [ -r "/etc/cups/ppd/$q.ppd" ] || continue
    t=$((t+1)); protected "$q" && n=$((n+1))
  done
  echo "$n $t"
  ;;
install)
  mkdir -p "$BIN" "$DATA/verdicts"
  [ "$OS" = Darwin ] && { chown root:wheel "$BIN"; chmod 755 "$BIN"; }
  chown "$OWNER" "$DATA" "$DATA/verdicts" 2>/dev/null || true
  chmod 755 "$DATA"; chmod 1777 "$DATA/verdicts"
  for q in $queues; do
    ppd="/etc/cups/ppd/$q.ppd"
    [ -r "$ppd" ] || { echo "· $q: sin PPD (cola sin filtros), se omite"; continue; }
    if protected "$q"; then
      # Ya protegida: se regenera el envoltorio (así «Reparar» actualiza un filtro instalado por una versión anterior).
      for prog in $(grep -o "$PFX[A-Za-z0-9_.-]*" "$ppd" | sed "s/^$PFX//" | sort -u); do [ -x "$FILTERS/$prog" ] && mkwrapper "$prog"; done
      echo "· $q: ya protegida (filtro actualizado)"; continue
    fi
    tmp="$(mktemp /tmp/sg-ppd.XXXXXX)"; chmod 644 "$tmp"
    changed=0
    while IFS= read -r line; do
      case "$line" in
        '*cupsFilter:'*|'*cupsFilter2:'*)
          # Último campo entre comillas = programa del filtro (nombre sin ruta, instalado en la carpeta de CUPS).
          prog="$(printf '%s' "$line" | sed -E 's/.*[[:space:]]([^[:space:]"]+)"[[:space:]]*$/\1/')"
          case "$prog" in
            /*|-|'') ;;
            *) if [ -x "$FILTERS/$prog" ]; then
                 mkwrapper "$prog"
                 newp="$PFX$prog"; [ "$OS" = Darwin ] && newp="$BIN/$PFX$prog"
                 line="$(printf '%s' "$line" | sed -E "s#([[:space:]])$prog\"([[:space:]]*)\$#\\1$newp\"\\2#")"
                 changed=1
               fi ;;
          esac ;;
      esac
      printf '%s\n' "$line" >> "$tmp"
    done < "$ppd"
    if [ $changed = 1 ]; then ${SG_LPADMIN:-lpadmin} -p "$q" -P "$tmp" && echo "✓ $q protegida"; else echo "· $q: sin filtro que envolver"; fi
    rm -f "$tmp"
  done
  ;;
uninstall)
  for q in $queues; do
    protected "$q" || continue
    tmp="$(mktemp /tmp/sg-ppd.XXXXXX)"; chmod 644 "$tmp"
    sed -E "s#(\"[^\"]* )[^ \"]*/?$PFX([A-Za-z0-9_.-]+)\"#\\1\\2\"#" "/etc/cups/ppd/$q.ppd" > "$tmp"
    ${SG_LPADMIN:-lpadmin} -p "$q" -P "$tmp" && echo "✓ $q restaurada"
    rm -f "$tmp"
  done
  ;;
*)
  echo "Uso: $0 install|uninstall|status [cola ...]" >&2; exit 2 ;;
esac
