package app

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"controlimpresion/src"
)

// Los binarios de CUPS se sustituyen por scripts que guardan su estado en $FAKE.
const fakeLpstat = `#!/bin/sh
case "$1" in
 -e) cat "$FAKE/printers";;
 -o) for f in "$FAKE"/jobs/$2-*; do [ -e "$f" ] || continue; echo "$(basename "$f") user 1024 date"; done;;
esac
`
const fakeLpadmin = `#!/bin/sh
echo "$@" >> "$FAKE/lpadmin.log"
`
const fakeIpptool = `#!/bin/sh
uri="$2"
if grep -qE "Hold-New-Jobs|Held-New-Jobs" "$3" 2>/dev/null; then
 echo "$(sed -n 's/.*OPERATION //p' "$3") $uri" >> "$FAKE/hnj.log"
 exit 0
fi
case "$uri" in
 */jobs/*)
  id="${uri##*/}"
  for f in "$FAKE"/jobs/*-"$id"; do
   [ -e "$f" ] || exit 1
   user=$(sed -n 1p "$f"); host=$(sed -n 2p "$f"); name=$(sed -n 3p "$f"); st=$(sed -n 4p "$f"); mode=$(sed -n 5p "$f")
   [ "$mode" = "broken" ] && { echo "ipptool: no se puede leer" >&2; exit 1; }
   if [ "$mode" = "dup" ]; then
    # Salida real de un trabajo de cola cliente (cupsd->cupsd): atributo duplicado y código 1.
    echo "    Get-Job-Attributes                                                   [FAIL]"
    echo "        status-code = successful-ok (successful-ok)"
    echo "        job-uri (uri) = ipp://localhost:631/jobs/$id"
    echo "        job-originating-user-name (nameWithoutLanguage) = $user"
    echo "        job-name (nameWithoutLanguage) = $name"
    echo "        job-originating-user-name (nameWithoutLanguage) = otro"
    echo "        job-originating-host-name (nameWithoutLanguage) = $host"
    echo "        job-state (enum) = $st"
    echo "        Duplicate \"job-originating-user-name\" attribute in job-attributes-tag group"
    exit 1
   fi
   echo "        job-uri (uri) = ipp://localhost:631/jobs/$id"
   echo "        job-originating-user-name (nameWithoutLanguage) = $user"
   echo "        job-name (nameWithoutLanguage) = $name"
   echo "        job-originating-host-name (nameWithoutLanguage) = $host"
   echo "        job-state (enum) = $st"
   k=$(sed -n 6p "$f"); imp=$(sed -n 7p "$f")
   if [ -n "$k" ]; then echo "        job-k-octets (integer) = $k"; fi
   if [ -n "$imp" ]; then echo "        job-impressions (integer) = $imp"; fi
  done;;
 *) echo "        device-uri (uri) = $(cat "$FAKE/uri.$(basename "$uri")" 2>/dev/null || echo socket://127.0.0.1:9100)"
    echo "        printer-state (enum) = idle"
    echo "        printer-state-reasons (keyword) = none"
    echo "        printer-make-and-model (textWithoutLanguage) = Fake Laser 9000"
    echo "        printer-is-shared (boolean) = false";;
esac
`
const fakeLp = `#!/bin/sh
if [ "$4" = "hold" ]; then
 echo "hold $2" >> "$FAKE/actions.log"
 for f in "$FAKE"/jobs/*-"$2"; do sed -i.bak 's/^pending$/pending-held/' "$f"; rm -f "$f.bak"; done
 exit 0
fi
echo "release $2" >> "$FAKE/actions.log"
for f in "$FAKE"/jobs/*-"$2"; do rm -f "$f"; done
`
const fakeCancel = `#!/bin/sh
echo "cancel $1" >> "$FAKE/actions.log"
for f in "$FAKE"/jobs/*-"$1"; do rm -f "$f"; done
`

type env struct {
	t    *testing.T
	fake string
	s    *Server
	ts   *httptest.Server
	c    *http.Client
}

func newEnv(t *testing.T) *env {
	t.Helper()
	root, _ := filepath.Abs("../../work")
	base, err := os.MkdirTemp(root, "gotest-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(base) })
	fake, data := filepath.Join(base, "fake"), filepath.Join(base, "data")
	os.MkdirAll(filepath.Join(fake, "jobs"), 0o755)
	os.MkdirAll(filepath.Join(fake, "bin"), 0o755)
	for n, body := range map[string]string{"lpstat": fakeLpstat, "lpadmin": fakeLpadmin, "ipptool": fakeIpptool, "lp": fakeLp, "cancel": fakeCancel} {
		if err := os.WriteFile(filepath.Join(fake, "bin", n), []byte(body), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	os.WriteFile(filepath.Join(fake, "printers"), []byte("Oficina\nTaller\n"), 0o644)
	t.Setenv("FAKE", fake)
	t.Setenv("PATH", filepath.Join(fake, "bin")+":"+os.Getenv("PATH"))
	t.Setenv("CONTROL_ADMIN_PASSWORD", "secreto-123")
	s, err := New(data)
	if err != nil {
		t.Fatal(err)
	}
	ts := httptest.NewServer(s.Handler(src.FS))
	t.Cleanup(ts.Close)
	jar, _ := cookiejar.New(nil)
	return &env{t: t, fake: fake, s: s, ts: ts, c: &http.Client{Jar: jar}}
}

func (e *env) post(cmd string, body any) (int, []byte) {
	e.t.Helper()
	b, _ := json.Marshal(body)
	resp, err := e.c.Post(e.ts.URL+"/api/"+cmd, "application/json", bytes.NewReader(b))
	if err != nil {
		e.t.Fatal(err)
	}
	defer resp.Body.Close()
	var buf bytes.Buffer
	buf.ReadFrom(resp.Body)
	return resp.StatusCode, buf.Bytes()
}

func (e *env) ok(cmd string, body any) []byte {
	e.t.Helper()
	code, out := e.post(cmd, body)
	if code != 200 {
		e.t.Fatalf("%s: %d %s", cmd, code, out)
	}
	return out
}

func (e *env) login() {
	e.t.Helper()
	e.ok("login", map[string]string{"password": "secreto-123"})
}

var jobSeq = 100

func (e *env) job(printer, user, host, name string) {
	jobSeq++
	f := filepath.Join(e.fake, "jobs", printer+"-"+itoa(jobSeq))
	os.WriteFile(f, []byte(user+"\n"+host+"\n"+name+"\npending-held\n"), 0o644)
}

// jobMode crea un trabajo retenido con un modo especial del ipptool falso ("dup" o "broken").
func (e *env) jobMode(printer, user, host, name, mode string) {
	jobSeq++
	f := filepath.Join(e.fake, "jobs", printer+"-"+itoa(jobSeq))
	os.WriteFile(f, []byte(user+"\n"+host+"\n"+name+"\npending-held\n"+mode+"\n"), 0o644)
}

// jobSized crea un trabajo retenido con tamaño (KiB) y páginas que informa el ipptool falso ("" = no informa).
func (e *env) jobSized(printer, user, host, name, kib, imp string) {
	jobSeq++
	f := filepath.Join(e.fake, "jobs", printer+"-"+itoa(jobSeq))
	os.WriteFile(f, []byte(user+"\n"+host+"\n"+name+"\npending-held\nok\n"+kib+"\n"+imp+"\n"), 0o644)
}

func itoa(n int) string { b, _ := json.Marshal(n); return string(b) }

func (e *env) actions() string {
	b, _ := os.ReadFile(filepath.Join(e.fake, "actions.log"))
	return string(b)
}

func (e *env) tick()    { e.s.tick(ptr(e.s.config()), map[string]bool{}) }
func ptr[T any](v T) *T { return &v }

func (e *env) lastLog(n int) []Activity {
	var a []Activity
	json.Unmarshal(e.ok("get_activity", nil), &a)
	if len(a) < n {
		e.t.Fatalf("registro: %d entradas, se esperaban %d", len(a), n)
	}
	return a[len(a)-n:]
}

func TestAuth(t *testing.T) {
	e := newEnv(t)
	if code, _ := e.post("list_printers", nil); code != 401 {
		t.Fatalf("sin sesión debe ser 401, fue %d", code)
	}
	if code, _ := e.post("login", map[string]string{"password": "mala"}); code != 401 {
		t.Fatalf("login erróneo: %d", code)
	}
	resp, err := e.c.Post(e.ts.URL+"/api/login", "application/json", strings.NewReader(`{"password":"secreto-123"}`))
	if err != nil || resp.StatusCode != 200 {
		t.Fatal("login correcto falló")
	}
	var ck *http.Cookie
	for _, c := range resp.Cookies() {
		if c.Name == cookieName {
			ck = c
		}
	}
	if ck == nil || !ck.HttpOnly || ck.SameSite != http.SameSiteStrictMode {
		t.Fatalf("cookie inválida: %+v", ck)
	}
	var tok struct{ Token string }
	json.NewDecoder(resp.Body).Decode(&tok)
	// Bearer sin cookie, token inválido y origen ajeno.
	do := func(auth, origin string) int {
		req, _ := http.NewRequest("POST", e.ts.URL+"/api/get_paused", strings.NewReader("{}"))
		req.Header.Set("Authorization", auth)
		if origin != "" {
			req.Header.Set("Origin", origin)
		}
		r, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		return r.StatusCode
	}
	if c := do("Bearer "+tok.Token, ""); c != 200 {
		t.Fatalf("bearer: %d", c)
	}
	if c := do("Bearer malo", ""); c != 401 {
		t.Fatalf("bearer inválido: %d", c)
	}
	if c := do("Bearer "+tok.Token, "http://evil.example"); c != 403 {
		t.Fatalf("origen ajeno: %d", c)
	}
	// GET no permitido; interfaz estática sin sesión.
	if r, _ := http.Get(e.ts.URL + "/api/get_paused"); r.StatusCode != 405 {
		t.Fatalf("GET api: %d", r.StatusCode)
	}
	for _, p := range []string{"/", "/app.js", "/shim.js", "/style.css"} {
		if r, _ := http.Get(e.ts.URL + p); r.StatusCode != 200 {
			t.Fatalf("%s: %d", p, r.StatusCode)
		}
	}
}

func TestLimiteDeIntentos(t *testing.T) {
	e := newEnv(t)
	for i := 0; i < maxFails; i++ {
		if code, _ := e.post("login", map[string]string{"password": "x"}); code != 401 {
			t.Fatalf("intento %d: %d", i, code)
		}
	}
	// Bloqueado incluso con la contraseña correcta.
	if code, _ := e.post("login", map[string]string{"password": "secreto-123"}); code != 429 {
		t.Fatalf("esperado 429, fue %d", code)
	}
}

func TestReglasYRetencion(t *testing.T) {
	e := newEnv(t)
	e.login()
	rules := map[string]any{"enabled": true, "mode": "block", "apps": []string{"*.docx"}, "sites": []string{}, "ips": []string{"10.0.0.5"}}
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
	if code, _ := e.post("set_rules", map[string]any{"printer": "-x", "rules": rules}); code != 400 {
		t.Fatalf("impresora inválida: %d", code)
	}
	lp, _ := os.ReadFile(filepath.Join(e.fake, "lpadmin.log"))
	if !strings.Contains(string(lp), "-p Oficina -o job-hold-until-default=indefinite") {
		t.Fatalf("no retuvo la cola: %q", lp)
	}
	// managed.json con el formato de escritorio.
	mb, _ := os.ReadFile(filepath.Join(e.s.dir, "managed.json"))
	if string(mb) != `{"Oficina":{"pausada_antes":false}}` {
		t.Fatalf("managed.json: %s", mb)
	}
	e.job("Oficina", "ana", "10.0.0.1", "carta.docx")  // app
	e.job("Oficina", "ana", "10.0.0.5", "notas.txt")   // ip
	e.job("Oficina", "ana", "10.0.0.9", "informe.pdf") // permitido
	e.job("Taller", "ana", "10.0.0.5", "otra.docx")    // Taller sin control: no se toca
	// Un trabajo que CUPS no retuvo (carrera con lpadmin) se retiene y se evalúa en el ciclo siguiente.
	jobSeq++
	os.WriteFile(filepath.Join(e.fake, "jobs", "Oficina-"+itoa(jobSeq)), []byte("ana\n10.0.0.9\nescapado.pdf\npending\n"), 0o644)
	e.tick()
	if !strings.Contains(e.actions(), "hold ") {
		t.Fatalf("no retuvo el trabajo pendiente: %q", e.actions())
	}
	e.tick()
	if got := e.actions(); strings.Count(got, "cancel") != 2 || strings.Count(got, "release") != 2 {
		t.Fatalf("acciones: %q", got)
	}
	l := e.lastLog(4)[:3]
	want := []struct {
		doc, reason string
		allowed     bool
	}{{"carta.docx", "Aplicación: *.docx", false}, {"notas.txt", "IP: 10.0.0.5", false}, {"informe.pdf", "Sin coincidencia", true}}
	for i, w := range want {
		if l[i].Document != w.doc || l[i].Reason != w.reason || l[i].Allowed != w.allowed || l[i].Printer != "Oficina" {
			t.Errorf("registro %d: %+v", i, l[i])
		}
	}
	if l[1].Origin == nil || *l[1].Origin != "10.0.0.5" || l[1].User == nil || *l[1].User != "ana" {
		t.Errorf("origen/usuario: %+v", l[1])
	}
	// config.json: mismos nombres de campo que escritorio.
	var raw map[string]json.RawMessage
	b, _ := os.ReadFile(filepath.Join(e.s.dir, "config.json"))
	json.Unmarshal(b, &raw)
	for _, k := range []string{"printers", "global", "settings"} {
		if _, ok := raw[k]; !ok {
			t.Errorf("config.json sin %q", k)
		}
	}
	// events.db: un evento por actividad (y ya no se escribe log.jsonl).
	if n := e.s.events.count(); n != 4 {
		t.Errorf("events.db: %d eventos", n)
	}
	if _, err := os.Stat(filepath.Join(e.s.dir, "log.jsonl")); err == nil {
		t.Errorf("no debe existir log.jsonl")
	}
	// Quitar el control libera la cola.
	rules["enabled"] = false
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
	mb, _ = os.ReadFile(filepath.Join(e.s.dir, "managed.json"))
	if string(mb) != `{}` {
		t.Fatalf("managed.json tras liberar: %s", mb)
	}
	// Datos insuficientes y política "block".
	e.ok("set_settings", map[string]any{"settings": map[string]any{"unknown_policy": "block", "interval_secs": 1}})
	rules["enabled"] = true
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
	e.job("Oficina", "ana", "10.0.0.9", "")
	e.tick()
	if l := e.lastLog(1)[0]; l.Reason != "Datos insuficientes" || l.Allowed {
		t.Errorf("sin datos: %+v", l)
	}
}

func TestBloqueoRapido(t *testing.T) {
	e := newEnv(t)
	e.login()
	var r struct {
		IPs []string `json:"ips"`
	}
	json.Unmarshal(e.ok("block_host", map[string]string{"host": "192.168.1.50"}), &r)
	if len(r.IPs) != 1 || r.IPs[0] != "192.168.1.50" {
		t.Fatalf("ips: %v", r.IPs)
	}
	var g struct {
		Enabled bool
		Mode    string
		IPs     []string
	}
	json.Unmarshal(e.ok("get_global_rules", nil), &g)
	if !g.Enabled || g.Mode != "block" || len(g.IPs) != 1 {
		t.Fatalf("global: %+v", g)
	}
	// La global retiene todas las colas.
	if mb, _ := os.ReadFile(filepath.Join(e.s.dir, "managed.json")); !strings.Contains(string(mb), "Oficina") || !strings.Contains(string(mb), "Taller") {
		t.Fatalf("managed: %s", mb)
	}
	e.job("Taller", "bob", "192.168.1.50", "foto.png")
	e.job("Taller", "bob", "192.168.1.51", "foto.png")
	e.tick()
	l := e.lastLog(2)
	if l[0].Allowed || l[0].Reason != "Global · IP: 192.168.1.50" || !l[1].Allowed {
		t.Fatalf("registro: %+v", l)
	}
	// Registro de quién/cuándo.
	var q Quick
	json.Unmarshal(e.ok("get_quick", nil), &q)
	if len(q.Blocks) != 1 || !strings.HasPrefix(q.Blocks[0].By, "admin@") || q.Blocks[0].Time == 0 {
		t.Fatalf("quick: %+v", q)
	}
	if code, _ := e.post("block_host", map[string]string{"host": "no es un host"}); code != 400 {
		t.Fatalf("host inválido: %d", code)
	}
	// Desbloquear.
	e.ok("unblock_host", map[string]string{"host": "192.168.1.50"})
	if code, _ := e.post("unblock_host", map[string]string{"host": "192.168.1.50"}); code != 400 {
		t.Fatalf("doble desbloqueo: %d", code)
	}
	e.job("Taller", "bob", "192.168.1.50", "foto.png")
	e.tick()
	if l := e.lastLog(1)[0]; !l.Allowed {
		t.Fatalf("tras desbloquear: %+v", l)
	}
	// Pausar impresora: todo se bloquea en esa cola.
	e.ok("pause_printer", map[string]any{"printer": "Taller", "paused": true})
	e.job("Taller", "bob", "192.168.1.9", "a.txt")
	e.tick()
	if l := e.lastLog(1)[0]; l.Allowed || !strings.HasPrefix(l.Reason, "Impresora pausada por admin@") {
		t.Fatalf("pausada: %+v", l)
	}
	e.ok("pause_printer", map[string]any{"printer": "Taller", "paused": false})
	e.job("Taller", "bob", "192.168.1.9", "b.txt")
	e.tick()
	if l := e.lastLog(1)[0]; !l.Allowed {
		t.Fatalf("reanudada: %+v", l)
	}
	// Global en modo permitir: el bloqueo rápido se rechaza.
	e.ok("set_global_rules", map[string]any{"rules": map[string]any{"enabled": true, "mode": "allow", "apps": []string{}, "sites": []string{}, "ips": []string{"10.0.0.0/8"}}})
	if code, _ := e.post("block_host", map[string]string{"host": "10.0.0.1"}); code != 400 {
		t.Fatalf("global en allow: %d", code)
	}
}

func TestContrasenaYDesktop(t *testing.T) {
	e := newEnv(t)
	e.login()
	if string(e.ok("has_password", nil)) != "true\n" || string(e.ok("verify_password", map[string]string{"password": "secreto-123"})) != "true\n" {
		t.Fatal("has/verify_password")
	}
	if code, _ := e.post("set_password", map[string]string{"current": "mala", "new": "nuevaclave1"}); code != 400 {
		t.Fatal("set_password con actual errónea")
	}
	if code, _ := e.post("set_password", map[string]string{"current": "secreto-123", "new": ""}); code != 400 {
		t.Fatal("el servidor no admite contraseña vacía")
	}
	e.ok("set_password", map[string]string{"current": "secreto-123", "new": "nuevaclave1"})
	if !e.s.checkPassword("nuevaclave1") || e.s.checkPassword("secreto-123") {
		t.Fatal("la contraseña no cambió")
	}
	// get_settings no filtra el hash.
	var st Settings
	json.Unmarshal(e.ok("get_settings", nil), &st)
	if st.PasswordHash != "set" || st.PasswordSalt != "" {
		t.Fatalf("get_settings: %+v", st)
	}
	if code, _ := e.post("set_autostart", map[string]any{"enabled": true}); code != 404 {
		t.Fatalf("set_autostart: %d", code)
	}
	if string(e.ok("get_autostart", nil)) != "false\n" || string(e.ok("extension_status", nil)) != "null\n" {
		t.Fatal("valores inertes")
	}
	// export_rules no incluye contraseña; import conserva la local.
	exp := e.ok("export_rules", nil)
	var s string
	json.Unmarshal(exp, &s)
	if strings.Contains(s, `"password_hash": "`) && !strings.Contains(s, `"password_hash": ""`) {
		t.Fatalf("export con hash: %s", s)
	}
	e.ok("import_rules", map[string]string{"json": s})
	if !e.s.checkPassword("nuevaclave1") {
		t.Fatal("import_rules cambió la contraseña")
	}
	if code, _ := e.post("comando_inexistente", nil); code != 404 {
		t.Fatal("comando desconocido")
	}
	e.ok("set_paused", map[string]bool{"paused": true})
	if string(e.ok("get_paused", nil)) != "true\n" {
		t.Fatal("pausa")
	}
	e.ok("clear_log", nil)
	if string(e.ok("get_activity", nil)) != "null\n" && string(e.ok("get_activity", nil)) != "[]\n" {
		t.Fatal("clear_log")
	}
}

func TestListPrinters(t *testing.T) {
	e := newEnv(t)
	e.login()
	var v []struct {
		Name  string
		Rules struct{ Apps []string }
		Info  struct{ Kind, URI, Status string }
	}
	json.Unmarshal(e.ok("list_printers", nil), &v)
	if len(v) != 2 || v[0].Name != "Oficina" || v[0].Rules.Apps == nil || v[0].Info.Kind != "red" || v[0].Info.URI != "socket://127.0.0.1:9100" {
		t.Fatalf("%+v", v)
	}
}

func TestContrasenaInicialDesdeArchivo(t *testing.T) {
	dir, _ := filepath.Abs("../../work")
	d, _ := os.MkdirTemp(dir, "gotest-")
	defer os.RemoveAll(d)
	t.Setenv("CONTROL_ADMIN_PASSWORD", "")
	os.WriteFile(filepath.Join(d, "admin_password"), []byte("desde-archivo\n"), 0o600)
	s, err := New(d)
	if err != nil || !s.checkPassword("desde-archivo") {
		t.Fatalf("archivo de contraseña: %v", err)
	}
	// Reiniciar no cambia la contraseña.
	t.Setenv("CONTROL_ADMIN_PASSWORD", "otra")
	s2, _ := New(d)
	if !s2.checkPassword("desde-archivo") || s2.checkPassword("otra") {
		t.Fatal("el segundo arranque cambió la contraseña")
	}
}

func TestManagedFormatoAntiguo(t *testing.T) {
	m := parseManaged([]byte(`["Fax","HP"]`))
	if len(m) != 2 || m["Fax"].PausadaAntes {
		t.Fatal("lista antigua")
	}
	m = parseManaged([]byte(`{"Fax":{"pausada_antes":true},"HP":{}}`))
	if !m["Fax"].PausadaAntes || m["HP"].PausadaAntes {
		t.Fatal("formato actual")
	}
	if len(parseManaged([]byte("basura"))) != 0 {
		t.Fatal("basura")
	}
}

// Trabajos de una cola cliente (otro cupsd): atributo duplicado y ipptool con código 1.
func TestTrabajoConAtributoDuplicado(t *testing.T) {
	e := newEnv(t)
	e.login()
	rules := map[string]any{"enabled": true, "mode": "block", "apps": []string{"*.docx"}, "sites": []string{}, "ips": []string{}}
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
	e.jobMode("Oficina", "ana", "10.0.0.1", "informe.docx", "dup")
	e.jobMode("Oficina", "ana", "10.0.0.1", "hoja.xlsx", "dup")
	e.tick()
	if got := e.actions(); strings.Count(got, "cancel") != 1 || strings.Count(got, "release") != 1 {
		t.Fatalf("acciones: %q", got)
	}
	l := e.lastLog(2)
	if l[0].Document != "informe.docx" || l[0].Allowed || l[1].Document != "hoja.xlsx" || !l[1].Allowed {
		t.Errorf("registro: %+v", l)
	}
	if l[0].User == nil || *l[0].User != "ana" {
		t.Errorf("debe tomar el primer valor duplicado: %+v", l[0].User)
	}
}

// Un trabajo ilegible nunca queda retenido en silencio: se registra y se aplica unknown_policy.
func TestTrabajoIlegibleAplicaPolitica(t *testing.T) {
	for _, policy := range []string{"allow", "block"} {
		t.Run(policy, func(t *testing.T) {
			e := newEnv(t)
			e.login()
			e.ok("set_settings", map[string]any{"settings": map[string]any{"unknown_policy": policy, "interval_secs": 1}})
			rules := map[string]any{"enabled": true, "mode": "block", "apps": []string{"*.docx"}, "sites": []string{}, "ips": []string{}}
			e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
			e.jobMode("Oficina", "ana", "10.0.0.1", "x.pdf", "broken")
			noHold := map[string]bool{}
			cfg := e.s.config()
			for i := 0; i < maxReadFails-1; i++ {
				e.s.tick(&cfg, noHold)
			}
			if e.actions() != "" {
				t.Fatalf("actuó antes de confirmar el fallo: %q", e.actions())
			}
			e.s.tick(&cfg, noHold)
			want, allowed := "cancel", false
			if policy == "allow" {
				want, allowed = "release", true
			}
			if !strings.Contains(e.actions(), want) {
				t.Fatalf("acciones: %q", e.actions())
			}
			l := e.lastLog(1)[0]
			if l.Allowed != allowed || !strings.Contains(l.Document, "no se pudo leer") || !strings.Contains(l.Reason, "No se pudo leer el trabajo") {
				t.Errorf("registro: %+v", l)
			}
		})
	}
}

// Retener una cola aplica Hold-New-Jobs por ipptool; quitar el control lo libera.
func TestHoldNewJobs(t *testing.T) {
	e := newEnv(t)
	e.login()
	rules := map[string]any{"enabled": true, "mode": "block", "apps": []string{"*.docx"}, "sites": []string{}, "ips": []string{}}
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
	b, _ := os.ReadFile(filepath.Join(e.fake, "hnj.log"))
	if !strings.Contains(string(b), "Hold-New-Jobs ") || !strings.HasSuffix(strings.TrimSpace(string(b)), "/printers/Oficina") {
		t.Fatalf("hnj.log: %q", b)
	}
	e.s.Reconcile()
	b, _ = os.ReadFile(filepath.Join(e.fake, "hnj.log"))
	if strings.Count(string(b), "Hold-New-Jobs") != 2 {
		t.Fatalf("Reconcile debe reaplicarlo: %q", b)
	}
	rules["enabled"] = false
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
	b, _ = os.ReadFile(filepath.Join(e.fake, "hnj.log"))
	if !strings.Contains(string(b), "Release-Held-New-Jobs") {
		t.Fatalf("no liberó la cola: %q", b)
	}
}

// Un trabajo liberado que sigue en la cola esperando turno (impresora ocupada) no se vuelve a retener ni a registrar.
func TestTrabajoLiberadoNoSeReevalua(t *testing.T) {
	e := newEnv(t)
	e.login()
	rules := map[string]any{"enabled": true, "mode": "block", "apps": []string{"*.docx"}, "sites": []string{}, "ips": []string{}}
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": rules})
	e.job("Oficina", "ana", "10.0.0.1", "hoja.xlsx")
	cfg := e.s.config()
	noHold := map[string]bool{}
	e.s.tick(&cfg, noHold)
	if strings.Count(e.actions(), "release") != 1 {
		t.Fatalf("acciones: %q", e.actions())
	}
	// CUPS lo deja "pending" detrás de otro trabajo: sigue apareciendo en `lpstat -o`.
	os.WriteFile(filepath.Join(e.fake, "jobs", "Oficina-"+itoa(jobSeq)), []byte("ana\n10.0.0.1\nhoja.xlsx\npending\n"), 0o644)
	for i := 0; i < 3; i++ {
		e.s.tick(&cfg, noHold)
	}
	if got := e.actions(); strings.Count(got, "release") != 1 || strings.Contains(got, "hold ") {
		t.Fatalf("reevaluó el trabajo liberado: %q", got)
	}
	if n := len(e.lastLog(1)); n != 1 || e.s.events.count() != 1 {
		t.Fatalf("registro duplicado")
	}
	// Cuando sale de la cola se olvida.
	os.Remove(filepath.Join(e.fake, "jobs", "Oficina-"+itoa(jobSeq)))
	e.s.tick(&cfg, noHold)
	if len(e.s.released) != 0 {
		t.Fatalf("no se limpió: %v", e.s.released)
	}
}

func mustRead(p string) []byte { b, _ := os.ReadFile(p); return b }
