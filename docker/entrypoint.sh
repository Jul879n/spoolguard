#!/bin/sh
# Arranca cupsd (sin root), da de alta las impresoras de la variable PRINTERS y ejecuta control-server.
#
# PRINTERS="Nombre=socket://IP:9100;Otra=ipp://IP/ipp/print"
#   Cada par crea (o actualiza) una cola RAW compartida, equivalente a:
#     lpadmin -p NOMBRE -E -v socket://IP:9100 -m raw -o printer-is-shared=true
#   Los nombres no pueden llevar espacios ni '/', '#'.
# Las colas se reconstruyen en cada arranque: lo que no esté en PRINTERS y no persista en /etc/cups se pierde.
set -eu

cupsd -f &
CUPSD=$!

# Hold-New-Jobs por el socket local de cupsd (por TCP exige autenticación): retiene también los trabajos
# enviados con job-hold-until=no-hold. No sobrevive al reinicio de cupsd, por eso se aplica aquí en cada arranque.
# control-server lo reaplica (Reconcile) a las colas con control y lo quita en las que no lo tienen.
hold_new_jobs() {
  cat >"$HNJ_TEST" <<EOT
{
	NAME "Hold-New-Jobs"
	OPERATION Hold-New-Jobs
	GROUP operation-attributes-tag
	ATTR charset attributes-charset utf-8
	ATTR language attributes-natural-language en
	ATTR uri printer-uri ipp://localhost/printers/$1
	ATTR name requesting-user-name root
}
EOT
  ipptool -t "ipp://%2Frun%2Fcups%2Fcups.sock/printers/$1" "$HNJ_TEST" >/dev/null 2>&1
  rc=$?
  rm -f "$HNJ_TEST"
  return $rc
}
HNJ_TEST="${CONTROL_IMPRESION_DIR:-/data}/hold-new-jobs-entrypoint.test"

# Espera a que cupsd responda.
i=0
until lpstat -r >/dev/null 2>&1; do
  i=$((i + 1))
  if [ "$i" -gt 50 ] || ! kill -0 "$CUPSD" 2>/dev/null; then
    echo "entrypoint: cupsd no arrancó" >&2
    exit 1
  fi
  sleep 0.2
done

if [ -n "${PRINTERS:-}" ]; then
  OLDIFS=$IFS
  IFS=';'
  for spec in $PRINTERS; do
    IFS=$OLDIFS
    name=${spec%%=*}
    uri=${spec#*=}
    name=$(printf '%s' "$name" | tr -d ' ')
    if [ -z "$name" ] || [ "$name" = "$spec" ] || [ -z "$uri" ]; then
      [ -n "$spec" ] && echo "entrypoint: PRINTERS: entrada inválida «$spec» (formato Nombre=uri)" >&2
      IFS=';'
      continue
    fi
    # cupsd puede responder a lpstat antes de aceptar lpadmin (arranque lento): reintenta unos segundos.
    made=1
    for _try in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
      if lpadmin -p "$name" -E -v "spoolguard:$uri" -m raw -o printer-is-shared=true 2>/dev/null; then made=0; break; fi
      sleep 0.5
    done
    if [ "$made" = 0 ]; then
      echo "entrypoint: cola $name -> $uri"
      if hold_new_jobs "$name"; then
        echo "entrypoint: cola $name con Hold-New-Jobs"
      else
        echo "entrypoint: no se pudo aplicar Hold-New-Jobs a $name" >&2
      fi
    else
      echo "entrypoint: no se pudo crear la cola $name" >&2
    fi
    IFS=';'
  done
  IFS=$OLDIFS
fi

# control-server queda como proceso principal (recibe SIGTERM); cupsd se detiene con el contenedor.
trap 'kill "$CUPSD" 2>/dev/null || true' EXIT
exec control-server "$@"
