package app

import (
	"bytes"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func (e *env) obj(cmd string, body any) map[string]any {
	e.t.Helper()
	var m map[string]any
	if err := json.Unmarshal(e.ok(cmd, body), &m); err != nil {
		e.t.Fatal(err)
	}
	return m
}

func (e *env) str(cmd string, body any) string {
	e.t.Helper()
	var s string
	json.Unmarshal(e.ok(cmd, body), &s)
	return s
}

func TestBackupIdaYVueltaPorHTTP(t *testing.T) {
	e := newEnv(t)
	e.login()
	// Estado inicial: reglas en Oficina, global y parámetros distintos, un bloqueo rápido y eventos.
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": map[string]any{"enabled": true, "mode": "block", "apps": []string{"Word"}, "sites": []string{"a.cl"}, "ips": []string{"10.0.0.0/24"}}})
	e.ok("set_settings", map[string]any{"settings": map[string]any{"theme": "dark", "interval_secs": 7}})
	e.ok("block_host", map[string]string{"host": "10.9.9.9"})
	e.ok("pause_printer", map[string]any{"printer": "Taller", "paused": true})
	e.job("Oficina", "ana", "pc1", "carta.docx")
	e.tick()
	exp := e.str("export_backup", map[string]any{"categories": backupCats})
	var file map[string]any
	if err := json.Unmarshal([]byte(exp), &file); err != nil {
		t.Fatal(err)
	}
	if file["format"] != "spoolguard-backup" || file["version"].(float64) != 1 {
		t.Fatalf("cabecera: %v", file)
	}
	for _, w := range []string{"password", "token", "salt", "session", "secreto"} {
		if strings.Contains(strings.ToLower(exp), w) {
			t.Errorf("el archivo exportado contiene «%s»", w)
		}
	}
	for _, c := range backupCats {
		if file[c] == nil {
			t.Errorf("falta la categoría %s", c)
		}
	}
	before := e.s.snapshot()

	// Cambiamos todo y reimportamos en modo reemplazar: queda igual que antes.
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": map[string]any{"enabled": false, "apps": []string{"Otra"}}})
	e.ok("set_settings", map[string]any{"settings": map[string]any{"theme": "light", "interval_secs": 3}})
	e.ok("unblock_host", map[string]string{"host": "10.9.9.9"})
	pv := e.obj("preview_import", map[string]any{"json": exp, "modo": "reemplazar"})
	if pv["ok"] != true || lookup(pv, "reglas.modificadas").(float64) < 1 {
		t.Fatalf("preview: %v", pv)
	}
	if e.s.snapshot().Params.Theme != "light" {
		t.Fatal("preview_import aplicó algo")
	}
	res := e.obj("apply_import", map[string]any{"json": exp, "modo": "reemplazar"})
	if len(res["aplicado"].([]any)) != 5-1 { // inventario es informativo
		t.Fatalf("aplicado: %v", res["aplicado"])
	}
	after := e.s.snapshot()
	if !jsonEq(t, before.value(), after.value()) {
		t.Fatalf("ida y vuelta distinta\nantes   %v\ndespués %v", anyOf(t, before.value()), anyOf(t, after.value()))
	}
	// Copias previas y retención reaplicada (Oficina tiene regla activa → retenida).
	for _, f := range []string{"config.json.bak", "quick.json.bak"} {
		if _, err := os.Stat(filepath.Join(e.s.dir, f)); err != nil {
			t.Errorf("falta %s", f)
		}
	}
	if _, held := e.s.managed()["Oficina"]; !held {
		t.Error("la retención no se reaplicó")
	}
	// Idempotente: reimportar el registro no duplica eventos.
	n := e.s.events.count()
	e.ok("apply_import", map[string]any{"json": exp, "modo": "combinar"})
	if m := e.s.events.count(); m != n+1 { // +1 = el aviso de la importación anterior; el resto no se repite
		t.Errorf("eventos %d → %d, esperado +1 (aviso)", n, m)
	}
	// El registro del servidor deja constancia de quién y cuándo.
	logs := e.lastLog(3)
	var found bool
	for _, a := range logs {
		if strings.Contains(a.Document, "Importación de copia de seguridad por admin@") && strings.Contains(a.Document, "reemplazar") {
			found = true
		}
	}
	if !found {
		t.Errorf("sin aviso de importación en el registro: %+v", logs)
	}
	if !strings.Contains(e.str("export_backup", map[string]any{"categories": []string{"reglas"}}), "spoolguard-backup") {
		t.Fatal("export")
	}
	if !jsonContainsNotice(e, "Exportación de copia de seguridad por admin@") {
		t.Error("sin aviso de exportación")
	}
}

func jsonEq(t *testing.T, a, b any) bool {
	t.Helper()
	x, y := anyOf(t, a), anyOf(t, b)
	return bytes.Equal(bkJSON(x), bkJSON(y))
}

func bkJSON(v any) []byte { b, _ := json.Marshal(v); return b }

func jsonContainsNotice(e *env, sub string) bool {
	rows, _, _ := e.s.events.query(EventFilter{}, 100, 0)
	for _, r := range rows {
		if strings.Contains(r.Document, sub) {
			return true
		}
	}
	return false
}

func TestBackupSoloConSesionYLimites(t *testing.T) {
	e := newEnv(t)
	for _, c := range []string{"export_backup", "preview_import", "apply_import"} {
		if code, _ := e.post(c, map[string]any{"json": "{}", "categories": []string{"reglas"}}); code != 401 {
			t.Errorf("%s sin sesión: %d", c, code)
		}
	}
	e.login()
	// Cuerpo mayor que el límite: 413. Un archivo de 11 MiB dentro del límite del cuerpo: rechazado por tamaño.
	big := strings.Repeat("a", backupBodyMax+1)
	if code, _ := e.post("preview_import", map[string]any{"json": big}); code != http.StatusRequestEntityTooLarge {
		t.Errorf("cuerpo enorme: %d", code)
	}
	mid := `{"format":"spoolguard-backup","version":1,"pad":"` + strings.Repeat("a", 10<<20) + `"}`
	code, out := e.post("preview_import", map[string]any{"json": mid})
	if code != 400 || !strings.Contains(string(out), "demasiado grande") {
		t.Errorf("archivo de 10 MiB+: %d %s", code, out)
	}
	for name, body := range map[string]string{
		"corrupto":   `{"format":`,
		"futuro":     `{"format":"spoolguard-backup","version":9}`,
		"anidado":    strings.Repeat("[", 200) + strings.Repeat("]", 200),
		"no objeto":  `"hola"`,
		"sin format": `{"hola":1}`,
	} {
		if code, out := e.post("apply_import", map[string]any{"json": body, "modo": "reemplazar"}); code != 400 {
			t.Errorf("%s: %d %s", name, code, out)
		}
	}
	if code, _ := e.post("apply_import", map[string]any{"json": `{"format":"spoolguard-backup","version":1}`, "modo": "borrar"}); code != 400 {
		t.Error("modo inválido")
	}
	if code, _ := e.post("export_backup", map[string]any{"categories": []string{"contraseñas"}}); code != 400 {
		t.Error("categoría inventada")
	}
}

func TestBackupArchivoHostil(t *testing.T) {
	e := newEnv(t)
	e.login()
	e.ok("set_settings", map[string]any{"settings": map[string]any{"theme": "light"}})
	hashAntes := e.s.config().Settings.PasswordHash
	if hashAntes == "" {
		t.Fatal("sin hash inicial")
	}
	hostil := `{"format":"spoolguard-backup","version":1,"password_hash":"aa","token":"x",
	  "parametros":{"theme":"dark","password_hash":"deadbeef","password_salt":"s","server_token":"sgm_x","__proto__":{"polluted":true}},
	  "reglas":{"global":{"enabled":true,"mode":"block","apps":["x"],"sites":[],"ips":[],"token":"t","__proto__":{"a":1}},
	            "printers":{"Oficina":{"enabled":true,"apps":["Word"],"password_hash":"zz"}}}}`
	res := e.obj("apply_import", map[string]any{"json": hostil, "modo": "reemplazar"})
	cfg := e.s.config()
	if cfg.Settings.PasswordHash != hashAntes || cfg.Settings.Theme != "dark" {
		t.Fatalf("la contraseña no debe cambiar y el tema sí: %+v", cfg.Settings)
	}
	if !e.s.checkPassword("secreto-123") {
		t.Fatal("la contraseña de administrador cambió")
	}
	raw, _ := os.ReadFile(filepath.Join(e.s.dir, "config.json"))
	for _, w := range []string{"deadbeef", "sgm_x", "polluted"} {
		if strings.Contains(string(raw), w) {
			t.Errorf("config.json contiene «%s»", w)
		}
	}
	av := strings.Join(strListAny(res["avisos"]), "\n")
	for _, w := range []string{"campo sensible ignorado por seguridad: password_hash", "clave peligrosa ignorada: __proto__", "server_token"} {
		if !strings.Contains(av, w) {
			t.Errorf("falta aviso «%s»:\n%s", w, av)
		}
	}
	// Tipos erróneos: no se aplica nada y se explica el motivo.
	antes := e.s.snapshot()
	code, out := e.post("apply_import", map[string]any{"json": `{"format":"spoolguard-backup","version":1,"parametros":{"interval_secs":"x"},"reglas":{"global":{"ips":["999.9.9.9"]}}}`, "modo": "reemplazar"})
	if code != 400 || !strings.Contains(string(out), "no se aplicó nada") {
		t.Errorf("tipos erróneos: %d %s", code, out)
	}
	if !jsonEq(t, antes.value(), e.s.snapshot().value()) {
		t.Error("una importación con errores cambió el estado")
	}
}

func strListAny(v any) []string {
	out := []string{}
	a, _ := v.([]any)
	for _, x := range a {
		if s, ok := x.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

func TestBackupCompatibilidadCruzada(t *testing.T) {
	// El archivo que exporta el escritorio (vector compartido) se importa en el servidor.
	b, err := os.ReadFile("../../core/tests/vectors/backup.json")
	if err != nil {
		t.Fatal(err)
	}
	var v struct {
		Cases []map[string]json.RawMessage `json:"cases"`
	}
	json.Unmarshal(b, &v)
	var desktopFile json.RawMessage
	for _, c := range v.Cases {
		if string(c["kind"]) == `"export"` && string(c["server"]) == "false" {
			desktopFile = c["expect_file"]
		}
	}
	if desktopFile == nil {
		t.Fatal("sin archivo de escritorio en los vectores")
	}
	e := newEnv(t)
	e.login()
	// Los eventos del vector son de 2023; el servidor los acepta (hora válida) y la impresora Oficina existe.
	res := e.obj("apply_import", map[string]any{"json": string(desktopFile), "modo": "reemplazar"})
	if int(res["eventos_importados"].(float64)) != 1 {
		t.Fatalf("eventos: %v", res)
	}
	s := e.s.snapshot()
	if r := s.Printers["Oficina"]; !r.Enabled || len(r.Apps) != 2 || s.Params.Theme != "dark" || s.Params.RetentionDays != 30 {
		t.Fatalf("estado importado: %+v", s)
	}
	// Y el que exporta el servidor lo lee el parser compartido (mismo código que el escritorio).
	out := e.str("export_backup", map[string]any{"categories": backupCats})
	p, err := parseBackup([]byte(out), int64(now()))
	if err != nil || len(p.Errors) > 0 {
		t.Fatalf("el export del servidor no valida: %v %v", err, p.Errors)
	}
	// Formato antiguo de reglas (export_rules de versiones previas) sigue importándose.
	old := `{"printers":{"Taller":{"enabled":true,"mode":"block","apps":["x"],"sites":[],"ips":[]}},"global":{"enabled":false,"mode":null,"apps":[],"sites":[],"ips":[]},"settings":{"theme":"light","password_hash":"","server_token":"sgm_viejo"}}`
	e.ok("import_rules", map[string]string{"json": old})
	if r := e.s.snapshot().Printers["Taller"]; !r.Enabled {
		t.Fatal("formato antiguo no importado")
	}
	if strings.Contains(string(mustRead(filepath.Join(e.s.dir, "config.json"))), "sgm_viejo") {
		t.Fatal("el token del archivo antiguo se coló")
	}
}

func TestBackupReemplazarNoToleraEventosAMedias(t *testing.T) {
	e := newEnv(t)
	e.login()
	e.s.appendEvent(Event{Printer: "Oficina", Document: "x", Decision: "allow"})
	n := e.s.events.count()
	f := `{"format":"spoolguard-backup","version":1,"registro":[{"id":"cccccccc-0001","ts":1700000000,"decision":"block","printer":"Oficina"}]}`
	e.ok("apply_import", map[string]any{"json": f, "modo": map[string]string{"registro": "reemplazar"}})
	// Reemplazar borra lo anterior (n eventos) e importa 1, más el aviso de la importación.
	if m := e.s.events.count(); m != 2 {
		t.Errorf("eventos tras reemplazar: %d (antes %d)", m, n)
	}
}
