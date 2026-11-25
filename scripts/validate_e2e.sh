#!/bin/sh
# Validación end-to-end en macOS/Linux con una cola CUPS de prueba (no toca impresoras reales).
# Uso: scripts/validate_e2e.sh   (requiere la app instalada en /Applications)
D="$HOME/Library/Application Support/cl.cohasa.controlimpresion"
APP="/Applications/Control de impresión.app"
Q=CtrlValida; F="$(mktemp -d "${TMPDIR:-$HOME}/ctrlval.XXXX")"; echo x > "$F/a.txt"
fail=0
cleanup() { pkill -f "MacOS/control-impresion"; lpadmin -x $Q 2>/dev/null; rm -rf "$F"; rm -f "$D"/{config.json,log.jsonl,intents.jsonl}; }
trap cleanup EXIT
lpadmin -p $Q -E -v file:///dev/null -m drv:///sample.drv/generic.ppd -o job-hold-until-default=indefinite 2>/dev/null
arrancar() { pkill -f "MacOS/control-impresion"; sleep 1; rm -f "$D/log.jsonl"; echo "$1" > "$D/config.json"; open -a "$APP" --args --minimized; sleep 4; }
revisa() { # titulo esperado(true|false)
  lp -d $Q -t "$1" "$F/a.txt" >/dev/null; sleep 4
  got=$(grep -F "\"document\":\"$1\"" "$D/log.jsonl" | tail -1 | sed 's/.*"allowed":\([a-z]*\).*/\1/')
  if [ "$got" = "$2" ]; then echo "OK   $1 -> allowed=$got"; else echo "FALLA $1 -> esperado $2, obtenido '$got'"; fail=1; fi
}
echo "== Bloqueo por aplicación/extensión"
arrancar '{"printers":{"'$Q'":{"enabled":true,"mode":"block","apps":["Microsoft Word","*.docx"],"sites":[],"ips":[]}},"settings":{}}'
revisa "Microsoft Word - informe" false; revisa "carta.docx" false; revisa "notas.txt" true
echo "== Modo permitir"
arrancar '{"printers":{"'$Q'":{"enabled":true,"mode":"allow","apps":["*.txt"],"sites":[],"ips":[]}},"settings":{}}'
revisa "notas.txt" true; revisa "carta.docx" false
echo "== Sitio web (intención de la extensión)"
arrancar '{"printers":{"'$Q'":{"enabled":true,"mode":"block","apps":[],"sites":["facebook.com"],"ips":[]}},"settings":{}}'
python3 -I - "$D" <<'PY'
import subprocess,struct,json,sys,os
m=json.dumps({"type":"print_intent","url":"https://www.facebook.com/x","title":"Perfil Ana"}).encode()
subprocess.run([os.path.join(sys.argv[1],"native_host.sh")],input=struct.pack('<I',len(m))+m,capture_output=True,timeout=10)
PY
revisa "Perfil Ana - Chrome" false; revisa "Otra pagina - Chrome" true
echo "== Persistencia del registro"
n=$(wc -l < "$D/log.jsonl"); [ "$n" -ge 2 ] && echo "OK   log.jsonl con $n entradas" || { echo "FALLA log vacío"; fail=1; }
[ $fail -eq 0 ] && echo "TODO OK" || echo "HAY FALLAS"; exit $fail
