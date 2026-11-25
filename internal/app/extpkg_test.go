package app

import (
	"archive/zip"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func (e *env) fakeExt(version string) string {
	e.t.Helper()
	dir := filepath.Join(e.fake, "extension")
	os.MkdirAll(filepath.Join(dir, "icons"), 0o755)
	os.WriteFile(filepath.Join(dir, "manifest.json"), []byte(`{"name":"x","version":"`+version+`"}`), 0o644)
	os.WriteFile(filepath.Join(dir, "popup.js"), []byte("// popup"), 0o644)
	os.WriteFile(filepath.Join(dir, "icons", "icon-16.png"), []byte("png"), 0o644)
	os.WriteFile(filepath.Join(dir, ".oculto"), []byte("no"), 0o644)
	e.s.extDir = dir
	return dir
}

func TestExtVersionYPaquete(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("a")
	dir := e.fakeExt("0.1.1")

	// Sin token (o con la contraseña de administrador) no hay nada.
	for _, path := range []string{"/ext/version", "/ext/package.zip"} {
		if code, _, _ := e.extReq("GET", path, "", "", ""); code != 401 {
			t.Fatalf("%s sin token: %d", path, code)
		}
		if code, _, _ := e.extReq("GET", path, "secreto-123", "", ""); code != 401 {
			t.Fatalf("%s con la contraseña de admin: %d", path, code)
		}
	}
	if code, _, _ := e.extReq("POST", "/ext/version", tok, "", ""); code != 405 {
		t.Fatalf("POST: %d", code)
	}

	code, body, h := e.extReq("GET", "/ext/version", tok, "chrome-extension://abc", "")
	if code != 200 || h.Get("Access-Control-Allow-Origin") != "chrome-extension://abc" {
		t.Fatalf("version: %d %v", code, h)
	}
	var v struct {
		Version, SHA256, URL string
		Size                 int
	}
	if err := json.Unmarshal([]byte(body), &v); err != nil {
		t.Fatal(err)
	}
	if v.Version != "0.1.1" || v.URL != "/ext/package.zip" || len(v.SHA256) != 64 || v.Size == 0 {
		t.Fatalf("version: %s", body)
	}

	code, zb, h := e.extReq("GET", v.URL, tok, "", "")
	if code != 200 || h.Get("Content-Type") != "application/zip" || !strings.Contains(h.Get("Content-Disposition"), "0.1.1") {
		t.Fatalf("zip: %d %v", code, h)
	}
	sum := sha256.Sum256([]byte(zb))
	if hex.EncodeToString(sum[:]) != v.SHA256 || len(zb) != v.Size {
		t.Fatalf("sha256/size no coinciden: %x vs %s", sum, v.SHA256)
	}
	zr, err := zip.NewReader(bytes.NewReader([]byte(zb)), int64(len(zb)))
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, f := range zr.File {
		names = append(names, f.Name)
	}
	if strings.Join(names, ",") != "icons/icon-16.png,manifest.json,popup.js" {
		t.Fatalf("contenido del zip: %v", names)
	}

	// Determinista y con caché: mismo contenido, mismo hash; si cambia la versión cambia el paquete.
	_, body2, _ := e.extReq("GET", "/ext/version", tok, "", "")
	if body2 != body {
		t.Fatalf("no determinista: %s vs %s", body2, body)
	}
	os.WriteFile(filepath.Join(dir, "manifest.json"), []byte(`{"version":"0.1.2","pad":"mas largo"}`), 0o644)
	_, body3, _ := e.extReq("GET", "/ext/version", tok, "", "")
	if !strings.Contains(body3, `"version":"0.1.2"`) || body3 == body {
		t.Fatalf("no refrescó: %s", body3)
	}
}

func TestExtPaqueteNoDisponible(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("a")
	e.s.extDir = filepath.Join(e.fake, "no-existe")
	if code, _, _ := e.extReq("GET", "/ext/version", tok, "", ""); code != 404 {
		t.Fatalf("sin carpeta: %d", code)
	}
}
