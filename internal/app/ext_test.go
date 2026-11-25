package app

import (
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"controlimpresion/internal/rules"
)

func (e *env) extToken(name string) (id, tok string) {
	e.t.Helper()
	var r struct{ ID, Token string }
	if err := json.Unmarshal(e.ok("create_ext_token", map[string]string{"name": name}), &r); err != nil {
		e.t.Fatal(err)
	}
	return r.ID, r.Token
}

func (e *env) extReq(method, path, tok, origin, body string) (int, string, http.Header) {
	e.t.Helper()
	req, _ := http.NewRequest(method, e.ts.URL+path, strings.NewReader(body))
	if tok != "" {
		req.Header.Set("Authorization", "Bearer "+tok)
	}
	if origin != "" {
		req.Header.Set("Origin", origin)
	}
	// Cliente sin cookies: las rutas /ext no deben depender de la sesión.
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		e.t.Fatal(err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	return resp.StatusCode, string(b), resp.Header
}

func TestExtTokenCicloDeVida(t *testing.T) {
	e := newEnv(t)
	if code, _ := e.post("create_ext_token", map[string]string{"name": "x"}); code != 401 {
		t.Fatalf("sin sesión: %d", code)
	}
	e.login()
	if code, _ := e.post("create_ext_token", map[string]string{"name": " "}); code != 400 {
		t.Fatalf("nombre vacío: %d", code)
	}
	id, tok := e.extToken("PC Bodega")
	if !strings.HasPrefix(tok, "sgx_") {
		t.Fatal(tok)
	}
	// Solo se guarda el hash.
	raw, _ := os.ReadFile(filepath.Join(e.s.dir, extTokensFile))
	if strings.Contains(string(raw), tok) || !strings.Contains(string(raw), hashExtToken(tok)) {
		t.Fatalf("el archivo debe guardar solo el hash: %s", raw)
	}
	if strings.Contains(string(e.ok("list_ext_tokens", nil)), tok) {
		t.Fatal("la lista no debe incluir el token")
	}
	if code, _, _ := e.extReq("GET", "/ext/rules", tok, "chrome-extension://abc", ""); code != 200 {
		t.Fatalf("token válido: %d", code)
	}
	// La contraseña de administrador y la cookie no sirven como token de extensión.
	if code, _, _ := e.extReq("GET", "/ext/rules", "secreto-123", "", ""); code != 401 {
		t.Fatalf("contraseña como token: %d", code)
	}
	if code, _, _ := e.extReq("GET", "/ext/rules", "", "", ""); code != 401 {
		t.Fatalf("sin token: %d", code)
	}
	// El token de extensión no abre la API de administración.
	req, _ := http.NewRequest("POST", e.ts.URL+"/api/get_activity", strings.NewReader("{}"))
	req.Header.Set("Authorization", "Bearer "+tok)
	if resp, _ := http.DefaultClient.Do(req); resp.StatusCode != 401 {
		t.Fatalf("token de extensión en /api: %d", resp.StatusCode)
	}
	// Sobrevive a un reinicio y revocar corta el acceso.
	s2, _ := New(e.s.dir)
	if _, ok := s2.ext.lookup(tok); !ok {
		t.Fatal("el token debe persistir")
	}
	e.ok("revoke_ext_token", map[string]string{"id": id})
	if code, _, _ := e.extReq("GET", "/ext/rules", tok, "", ""); code != 401 {
		t.Fatalf("revocado: %d", code)
	}
	if code, _ := e.post("revoke_ext_token", map[string]string{"id": id}); code != 404 {
		t.Fatalf("revocar dos veces: %d", code)
	}
}

func TestExtReglas(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("a")
	allow, block := "allow", "block"
	e.s.mu.Lock()
	e.s.cfg.Printers["Oficina"] = rulesOf(true, &block, "Ejemplo.cl", "*.otro.cl")
	e.s.cfg.Printers["Taller"] = rulesOf(true, nil, "ejemplo.cl")
	e.s.cfg.Printers["Off"] = rulesOf(false, &block, "off.cl")
	e.s.cfg.Global = *rulesOf(true, &block, "Global.cl")
	e.s.mu.Unlock()
	_, body, _ := e.extReq("GET", "/ext/rules", tok, "", "")
	var r struct {
		Block     []string  `json:"block"`
		AllowOnly *[]string `json:"allow_only"`
	}
	json.Unmarshal([]byte(body), &r)
	if strings.Join(r.Block, ",") != "ejemplo.cl,otro.cl,global.cl" || r.AllowOnly != nil {
		t.Fatalf("%s", body)
	}
	if !strings.Contains(body, `"allow_only":null`) {
		t.Fatalf("allow_only debe ser null: %s", body)
	}
	e.s.mu.Lock()
	e.s.cfg.Printers["Taller"] = rulesOf(true, &allow, "Ok.cl")
	e.s.mu.Unlock()
	_, body, _ = e.extReq("GET", "/ext/rules", tok, "", "")
	if !strings.Contains(body, `"allow_only":["ok.cl"]`) {
		t.Fatalf("%s", body)
	}
}

func TestExtIntentYFindSite(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("a")
	o := "chrome-extension://abc"
	for _, body := range []string{
		`{"url":"javascript:alert(1)","title":"x"}`,
		`{"url":"file:///etc/passwd","title":"x"}`,
		`{"url":"","title":"x"}`,
		`{"url":"https://a.cl","title":"` + strings.Repeat("x", 301) + `"}`,
		`{"url":"https://` + strings.Repeat("a", 300) + `.cl","title":"x"}`,
		`no es json`,
	} {
		if code, _, _ := e.extReq("POST", "/ext/intent", tok, o, body); code != 400 {
			t.Errorf("%.40s: %d", body, code)
		}
	}
	if code, _, _ := e.extReq("POST", "/ext/intent", tok, o, `{"url":"https://a.cl","title":"`+strings.Repeat("x", 5000)+`"}`); code != 413 {
		t.Errorf("más de 4 KB: %d", code)
	}
	if code, _, _ := e.extReq("POST", "/ext/intent", "", o, `{"url":"https://a.cl","title":"x"}`); code != 401 {
		t.Errorf("sin token: %d", code)
	}
	if code, _, _ := e.extReq("GET", "/ext/intent", tok, o, ""); code != 405 {
		t.Errorf("GET: %d", code)
	}
	n := now()
	if code, _, _ := e.extReq("POST", "/ext/intent", tok, o, `{"url":"https://www.Facebook.com/x?y=1","title":"Mi página <b>"}`); code != 200 {
		t.Fatalf("intención válida: %d", code)
	}
	if code, _, _ := e.extReq("POST", "/ext/intent", tok, o, `{"url":"https://Gesprod.catchup.cl","title":"Impresión y etiquetado","kind":"agent"}`); code != 200 {
		t.Fatal(code)
	}
	raw, _ := os.ReadFile(filepath.Join(e.s.dir, intentsFile))
	lines := strings.Split(strings.TrimSpace(string(raw)), "\n")
	if len(lines) != 2 {
		t.Fatalf("%s", raw)
	}
	var it map[string]any
	json.Unmarshal([]byte(lines[0]), &it)
	if it["host"] != "www.facebook.com" || it["title"] != "Mi página <b>" || it["kind"] != "" || len(it) != 4 {
		t.Fatalf("formato: %s", lines[0])
	}
	if !strings.Contains(lines[1], `"kind":"agent"`) {
		t.Fatalf("%s", lines[1])
	}
	d := e.s.dir
	if got := findSite(d, "Mi página <b> - Google Chrome", n+10); got != "www.facebook.com" {
		t.Errorf("por título: %q", got)
	}
	if got := findSite(d, "otra cosa", n+10); got != "gesprod.catchup.cl" {
		t.Errorf("respaldo agente: %q", got)
	}
	if got := findSite(d, "Untitled", n+intentAgentWin+5); got != "" {
		t.Errorf("fuera de la ventana del agente: %q", got)
	}
	if got := findSite(d, "Mi página <b>", n+intentMaxAge+5); got != "" {
		t.Errorf("intención vencida: %q", got)
	}
	if got := findSite(d, "", n); got != "" {
		t.Errorf("documento vacío: %q", got)
	}
}

func TestExtSitioEnWorker(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("a")
	block := "block"
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": map[string]any{"enabled": true, "mode": block, "sites": []string{"malo.cl"}, "apps": []string{}, "ips": []string{}}})
	e.extReq("POST", "/ext/intent", tok, "", `{"url":"https://app.malo.cl","title":"Informe mensual de ventas"}`)
	e.job("Oficina", "ana", "10.0.0.5", "Informe mensual de ventas")
	e.tick()
	l := e.lastLog(1)[0]
	if l.Allowed || !strings.HasPrefix(l.Reason, "Sitio: malo.cl") {
		t.Fatalf("%+v", l)
	}
	e.job("Oficina", "ana", "10.0.0.5", "Documento sin intención asociada")
	e.tick()
	if l := e.lastLog(1)[0]; !l.Allowed {
		t.Fatalf("%+v", l)
	}
}

func TestExtCORS(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("a")
	for _, o := range []string{"chrome-extension://abcdefghijklmnop", "moz-extension://1234-abcd"} {
		code, _, h := e.extReq("OPTIONS", "/ext/intent", "", o, "")
		if code != 204 || h.Get("Access-Control-Allow-Origin") != o || !strings.Contains(h.Get("Access-Control-Allow-Headers"), "Authorization") || !strings.Contains(h.Get("Access-Control-Allow-Methods"), "POST") || h.Get("Access-Control-Allow-Credentials") != "" {
			t.Errorf("preflight %s: %d %v", o, code, h)
		}
		code, _, h = e.extReq("GET", "/ext/rules", tok, o, "")
		if code != 200 || h.Get("Access-Control-Allow-Origin") != o || h.Get("Vary") != "Origin" {
			t.Errorf("GET %s: %d %v", o, code, h)
		}
	}
	for _, o := range []string{"https://evil.example", "http://localhost:8080", "null", "chrome-extension://", "chrome-extension://a/b", "file://"} {
		if code, _, h := e.extReq("OPTIONS", "/ext/rules", "", o, ""); code != 403 || h.Get("Access-Control-Allow-Origin") != "" {
			t.Errorf("preflight %q: %d", o, code)
		}
		if code, _, _ := e.extReq("GET", "/ext/rules", tok, o, ""); code != 403 {
			t.Errorf("GET %q con token: %d", o, code)
		}
	}
}

func TestExtLimiteDeFrecuencia(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("a")
	_, tok2 := e.extToken("b")
	clock := time.Now()
	e.s.ext.now = func() time.Time { return clock }
	body := `{"url":"https://a.cl","title":"t"}`
	for i := 0; i < intentPerMin; i++ {
		if code, _, _ := e.extReq("POST", "/ext/intent", tok, "", body); code != 200 {
			t.Fatalf("petición %d: %d", i, code)
		}
	}
	code, _, h := e.extReq("POST", "/ext/intent", tok, "", body)
	if code != 429 || h.Get("Retry-After") == "" {
		t.Fatalf("debe limitar: %d", code)
	}
	if code, _, _ := e.extReq("POST", "/ext/intent", tok2, "", body); code != 200 {
		t.Fatalf("otro token no se ve afectado: %d", code)
	}
	if code, _, _ := e.extReq("GET", "/ext/rules", tok, "", ""); code != 200 {
		t.Fatalf("otra ruta no se ve afectada: %d", code)
	}
	clock = clock.Add(61 * time.Second)
	if code, _, _ := e.extReq("POST", "/ext/intent", tok, "", body); code != 200 {
		t.Fatalf("tras un minuto: %d", code)
	}
}

func rulesOf(on bool, mode *string, sites ...string) *rules.Rules {
	r := &rules.Rules{Enabled: on, Mode: mode, Sites: sites}
	r.Normalize()
	return r
}
