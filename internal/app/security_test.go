package app

import (
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

// Regresión: el contador de intentos se aplica antes de verificar, así que una ráfaga paralela de
// contraseñas incorrectas no da más de maxFails intentos.
func TestLoginRafagaParalelaNoSuperaElLimite(t *testing.T) {
	e := newEnv(t)
	var mu sync.Mutex
	got := map[int]int{}
	var wg sync.WaitGroup
	for i := 0; i < 40; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			code, _ := e.post("login", map[string]string{"password": "mala-" + strings.Repeat("x", i)})
			mu.Lock()
			got[code]++
			mu.Unlock()
		}()
	}
	wg.Wait()
	if got[401] > maxFails {
		t.Fatalf("la ráfaga obtuvo %d intentos (máx. %d): %v", got[401], maxFails, got)
	}
	if code, _ := e.post("login", map[string]string{"password": "secreto-123"}); code != 429 {
		t.Fatalf("tras la ráfaga la contraseña correcta debe seguir bloqueada, fue %d", code)
	}
}

func TestClientIPUsaLaUltimaEntradaDelProxy(t *testing.T) {
	r := httptest.NewRequest("POST", "/api/login", nil)
	r.RemoteAddr = "10.0.0.5:1234"
	r.Header.Set("X-Forwarded-For", "1.1.1.1, 9.9.9.9")
	if got := clientIP(r); got != "10.0.0.5" {
		t.Fatalf("sin CONTROL_TRUST_PROXY se ignora X-Forwarded-For, fue %s", got)
	}
	t.Setenv("CONTROL_TRUST_PROXY", "1")
	if got := clientIP(r); got != "9.9.9.9" {
		t.Fatalf("debe usar la última entrada (la del proxy), fue %s", got)
	}
	r.Header.Set("X-Forwarded-For", "no-es-una-ip")
	if got := clientIP(r); got != "10.0.0.5" {
		t.Fatalf("valor no IP: usar RemoteAddr, fue %s", got)
	}
}

// Matriz sin sesión: todo comando de /api devuelve 401 y nada de /ext o /api/events/ingest sin token.
func TestMatrizSinSesion(t *testing.T) {
	e := newEnv(t)
	anon := &http.Client{}
	for name := range commands {
		resp, err := anon.Post(e.ts.URL+"/api/"+name, "application/json", strings.NewReader("{}"))
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		if resp.StatusCode != 401 {
			t.Errorf("/api/%s sin sesión = %d (esperado 401)", name, resp.StatusCode)
		}
	}
	for _, c := range []struct{ method, path string }{{"GET", "/ext/rules"}, {"POST", "/ext/intent"}, {"POST", "/api/events/ingest"}} {
		req, _ := http.NewRequest(c.method, e.ts.URL+c.path, strings.NewReader("{}"))
		resp, err := anon.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		if resp.StatusCode != 401 {
			t.Errorf("%s %s sin token = %d", c.method, c.path, resp.StatusCode)
		}
	}
	// Un token de equipo no sirve para la extensión ni la contraseña de administrador como token.
	req, _ := http.NewRequest("GET", e.ts.URL+"/ext/rules", nil)
	req.Header.Set("Authorization", "Bearer secreto-123")
	if resp, _ := anon.Do(req); resp.StatusCode != 401 {
		t.Errorf("la contraseña no debe valer como token de extensión: %d", resp.StatusCode)
	}
}

func TestNewEndureceLosPermisosHeredados(t *testing.T) {
	dir := t.TempDir()
	for _, n := range []string{"config.json.bak", "ext_tokens.json", "quick.json", "intents.jsonl"} {
		if err := os.WriteFile(filepath.Join(dir, n), []byte("{}"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	_ = os.Chmod(dir, 0o755)
	t.Setenv("CONTROL_ADMIN_PASSWORD", "secreto-123")
	if _, err := New(dir); err != nil {
		t.Fatal(err)
	}
	if st, _ := os.Stat(dir); st.Mode().Perm() != 0o700 {
		t.Errorf("carpeta de datos: %o", st.Mode().Perm())
	}
	ents, _ := os.ReadDir(dir)
	for _, e := range ents {
		if st, _ := os.Stat(filepath.Join(dir, e.Name())); st.Mode().IsRegular() && st.Mode().Perm()&0o077 != 0 {
			t.Errorf("%s con permisos %o", e.Name(), st.Mode().Perm())
		}
	}
}

func TestCabecerasDeSeguridad(t *testing.T) {
	e := newEnv(t)
	for _, p := range []string{"/", "/app.js", "/healthz"} {
		resp, err := http.Get(e.ts.URL + p)
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		for k, want := range map[string]string{"X-Content-Type-Options": "nosniff", "X-Frame-Options": "DENY", "Referrer-Policy": "no-referrer", "Cross-Origin-Opener-Policy": "same-origin"} {
			if got := resp.Header.Get(k); got != want {
				t.Errorf("%s %s = %q", p, k, got)
			}
		}
		if !strings.Contains(resp.Header.Get("Content-Security-Policy"), "frame-ancestors 'none'") {
			t.Errorf("%s sin frame-ancestors", p)
		}
	}
	resp, _ := http.Post(e.ts.URL+"/api/get_paused", "application/json", strings.NewReader("{}"))
	resp.Body.Close()
	if resp.Header.Get("Cache-Control") != "no-store" {
		t.Errorf("/api sin no-store: %q", resp.Header.Get("Cache-Control"))
	}
}
