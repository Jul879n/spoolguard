package app

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func (e *env) devToken(name string) (id, tok string) {
	e.t.Helper()
	var r struct{ ID, Token, Kind string }
	if err := json.Unmarshal(e.ok("create_ext_token", map[string]string{"name": name, "kind": "device"}), &r); err != nil {
		e.t.Fatal(err)
	}
	if r.Kind != "device" || !strings.HasPrefix(r.Token, "sgm_") {
		e.t.Fatalf("token de equipo: %+v", r)
	}
	return r.ID, r.Token
}

func evJSON(id, doc, user, decision string, ts int64) map[string]any {
	return map[string]any{"id": id, "ts": ts, "host": "pc-bodega", "ip": "10.0.0.7", "user": user, "printer": "Oficina", "document": doc,
		"decision": decision, "reason": "r", "status": "liberado", "source": "desktop", "pages": 3}
}

type ingestResp struct {
	Accepted []string `json:"accepted"`
	Rejected []string `json:"rejected"`
}

func (e *env) ingest(tok string, evs ...map[string]any) (int, ingestResp) {
	e.t.Helper()
	b, _ := json.Marshal(map[string]any{"events": evs})
	code, body, _ := e.extReq("POST", "/api/events/ingest", tok, "", string(b))
	var r ingestResp
	json.Unmarshal([]byte(body), &r)
	return code, r
}

func newID(i int) string { return fmt.Sprintf("00000000-0000-7000-8000-%012d", i) }

type page struct {
	Total int64
	Rows  []eventRow
}

func (e *env) q(filter map[string]any, pg, size int) page {
	e.t.Helper()
	var p page
	if err := json.Unmarshal(e.ok("query_events", map[string]any{"filter": filter, "page": pg, "size": size}), &p); err != nil {
		e.t.Fatal(err)
	}
	return p
}

func TestIngestIdempotenteYEtiquetaDeEquipo(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.devToken("PC Bodega")
	now := time.Now().Unix()
	a, b := evJSON(newID(1), "a.pdf", "ana", "allow", now-10), evJSON(newID(2), "b.docx", "luis", "block", now-5)
	b["device"] = "falsificado" // el cliente no decide su etiqueta
	for i := 0; i < 2; i++ {
		code, r := e.ingest(tok, a, b)
		if code != 200 || len(r.Accepted) != 2 || len(r.Rejected) != 0 {
			t.Fatalf("envío %d: %d %+v", i, code, r)
		}
	}
	if n := e.s.events.count(); n != 2 {
		t.Fatalf("reenviar duplicó: %d", n)
	}
	p := e.q(nil, 1, 10)
	if p.Total != 2 || p.Rows[0].Document != "b.docx" || p.Rows[0].Device != "PC Bodega" || p.Rows[0].Source != "desktop" || p.Rows[0].Allowed {
		t.Fatalf("filas: %+v", p)
	}
	if p.Rows[1].Pages == nil || *p.Rows[1].Pages != 3 || p.Rows[1].Origin != "10.0.0.7" {
		t.Fatalf("campos: %+v", p.Rows[1])
	}
	// El primero que llega se conserva.
	a["document"] = "otro.pdf"
	e.ingest(tok, a)
	if got := e.q(map[string]any{"user": "ana"}, 1, 10).Rows[0].Document; got != "a.pdf" {
		t.Fatalf("sobrescribió: %s", got)
	}
}

func TestIngestLimites(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.devToken("PC")
	now := time.Now().Unix()
	many := make([]map[string]any, 501)
	for i := range many {
		many[i] = evJSON(newID(i+1), "d", "u", "allow", now)
	}
	if code, _ := e.ingest(tok, many...); code != 413 {
		t.Fatalf("501 eventos: %d", code)
	}
	if code, r := e.ingest(tok, many[:500]...); code != 200 || len(r.Accepted) != 500 {
		t.Fatalf("500 eventos: %d %d", code, len(r.Accepted))
	}
	// Más de 1 MiB.
	big := evJSON(newID(9000), strings.Repeat("x", 1000), "u", "allow", now)
	var huge []map[string]any
	for i := 0; i < 1200; i++ {
		x := map[string]any{}
		for k, v := range big {
			x[k] = v
		}
		x["id"] = newID(9000 + i)
		x["document"] = strings.Repeat("x", 1000)
		huge = append(huge, x)
	}
	if code, _, _ := e.extReq("POST", "/api/events/ingest", tok, "", mustJSON(map[string]any{"events": huge})); code != 413 {
		t.Fatalf("cuerpo de más de 1 MiB: %d", code)
	}
	bad := map[string]map[string]any{
		"documento largo": evJSON(newID(6020), strings.Repeat("x", 1001), "u", "allow", now),
		"usuario largo":   evJSON(newID(6021), "d", strings.Repeat("u", 256), "allow", now),
		"decisión":        evJSON(newID(6022), "d", "u", "quizas", now),
		"hora futura":     evJSON(newID(6023), "d", "u", "allow", now+3*86400),
		"hora cero":       evJSON(newID(6024), "d", "u", "allow", 0),
		"id con espacios": evJSON("id con espacios", "d", "u", "allow", now),
		"páginas <0":      func() map[string]any { x := evJSON(newID(6027), "d", "u", "allow", now); x["pages"] = -1; return x }(),
	}
	var batch []map[string]any
	for _, v := range bad {
		batch = append(batch, v)
	}
	good := evJSON(newID(6030), "ok", "u", "allow", now)
	batch = append(batch, good)
	_, r := e.ingest(tok, batch...)
	if len(r.Accepted) != 1 || r.Accepted[0] != newID(6030) {
		t.Fatalf("aceptados: %+v", r)
	}
	// Los inválidos con id válido se informan como rechazados (el cliente deja de reintentarlos); el de id inválido, no.
	if len(r.Rejected) != len(bad)-1 {
		t.Fatalf("rechazados: %d %+v", len(r.Rejected), r.Rejected)
	}
	for _, id := range []string{newID(6020), newID(6021), newID(6022)} {
		if e.s.events.has(id) {
			t.Fatalf("guardó un evento inválido %s", id)
		}
	}
	if code, _, _ := e.extReq("POST", "/api/events/ingest", tok, "", "{no es json"); code != 400 {
		t.Fatalf("json inválido: %d", code)
	}
	// Un lote vacío es válido.
	if code, r := e.ingest(tok); code != 200 || len(r.Accepted) != 0 {
		t.Fatalf("lote vacío: %d", code)
	}
}

func mustJSON(v any) string { b, _ := json.Marshal(v); return string(b) }

func (st *eventStore) has(id string) bool {
	var n int
	_ = st.db.QueryRow("SELECT COUNT(*) FROM events WHERE id = ?", id).Scan(&n)
	return n > 0
}

func TestIngestTokens(t *testing.T) {
	e := newEnv(t)
	e.login()
	ok := []map[string]any{evJSON(newID(1), "d", "u", "allow", time.Now().Unix())}
	devID, dev := e.devToken("PC")
	_, ext := e.extToken("Extensión")
	for name, tok := range map[string]string{"sin token": "", "inventado": "sgm_inventado", "de extensión": ext} {
		if code, _ := e.ingest(tok, ok...); code != 401 {
			t.Errorf("%s: %d", name, code)
		}
	}
	// Por GET no, y la cookie de sesión del admin tampoco vale como token de equipo.
	if code, _, _ := e.extReq("GET", "/api/events/ingest", dev, "", ""); code != 405 {
		t.Errorf("GET: %d", code)
	}
	// El token de equipo no sirve para la extensión ni para el panel (no puede leer eventos).
	if code, _, _ := e.extReq("GET", "/ext/rules", dev, "", ""); code != 401 {
		t.Errorf("token de equipo en /ext/rules: %d", code)
	}
	code, _, _ := e.extReq("POST", "/api/query_events", dev, "", "{}")
	if code != 401 {
		t.Errorf("token de equipo leyendo eventos: %d", code)
	}
	if code, r := e.ingest(dev, ok...); code != 200 || len(r.Accepted) != 1 {
		t.Fatalf("válido: %d", code)
	}
	// Revocado.
	e.ok("revoke_ext_token", map[string]string{"id": devID})
	if code, _ := e.ingest(dev, ok...); code != 401 {
		t.Errorf("revocado: %d", code)
	}
	// Solo se guarda el hash.
	_, dev2 := e.devToken("PC2")
	raw, _ := os.ReadFile(filepath.Join(e.s.dir, extTokensFile))
	if strings.Contains(string(raw), dev2) || !strings.Contains(string(raw), hashExtToken(dev2)) {
		t.Errorf("tokens.json debe guardar solo el hash")
	}
	// El listado distingue el tipo.
	var l []struct{ Name, Kind string }
	json.Unmarshal(e.ok("list_ext_tokens", nil), &l)
	kinds := map[string]string{}
	for _, x := range l {
		kinds[x.Name] = x.Kind
	}
	if kinds["PC2"] != "device" || kinds["Extensión"] != "ext" {
		t.Errorf("tipos: %v", kinds)
	}
	if code, _ := e.post("create_ext_token", map[string]string{"name": "x", "kind": "otro"}); code != 400 {
		t.Errorf("tipo desconocido: %d", code)
	}
}

func TestIngestLimiteDeFrecuencia(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.devToken("PC")
	clock := time.Now()
	e.s.ext.now = func() time.Time { return clock }
	for i := 0; i < ingestPerMin; i++ {
		if code, _ := e.ingest(tok); code != 200 {
			t.Fatalf("petición %d: %d", i, code)
		}
	}
	code, _, h := e.extReq("POST", "/api/events/ingest", tok, "", `{"events":[]}`)
	if code != 429 || h.Get("Retry-After") == "" {
		t.Fatalf("exceso: %d", code)
	}
	clock = clock.Add(61 * time.Second)
	if code, _ := e.ingest(tok); code != 200 {
		t.Fatalf("tras la ventana: %d", code)
	}
}

func TestConsultaConFiltrosPaginacionYCSV(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.devToken("PC Bodega")
	now := time.Now().Unix()
	var evs []map[string]any
	for i := 0; i < 30; i++ {
		u, d := "ana", "allow"
		if i%2 == 1 {
			u = "luis"
		}
		if i%5 == 0 {
			d = "block"
		}
		x := evJSON(newID(i+1), fmt.Sprintf("doc%02d.pdf", i), u, d, now-int64(100-i))
		if i == 7 {
			x["document"] = "100%_real.docx"
		}
		if i == 8 {
			x["document"] = "=CMD()"
		}
		evs = append(evs, x)
	}
	e.ingest(tok, evs...)
	// Un evento propio del servidor.
	e.s.appendLog(Activity{Time: uint64(now), Printer: "Taller", Document: "propio.txt", Allowed: true, Reason: "Sin coincidencia"})

	if p := e.q(nil, 1, 10); p.Total != 31 || len(p.Rows) != 10 || p.Rows[0].Document != "propio.txt" || p.Rows[0].Source != "server" || p.Rows[0].Device != "" {
		t.Fatalf("primera página: %d %d %+v", p.Total, len(p.Rows), p.Rows[0])
	}
	if p := e.q(nil, 4, 10); len(p.Rows) != 1 {
		t.Fatalf("última página: %d", len(p.Rows))
	}
	for name, c := range map[string]struct {
		f    map[string]any
		want int64
	}{
		"usuario":    {map[string]any{"user": "ana"}, 15},
		"impresora":  {map[string]any{"printer": "Taller"}, 1},
		"bloqueados": {map[string]any{"decision": "bad"}, 6},
		"permitidos": {map[string]any{"decision": "ok"}, 25},
		"equipo":     {map[string]any{"host": "PC Bodega"}, 30},
		"fechas":     {map[string]any{"from": now - 100, "to": now - 90}, 11},
		"texto":      {map[string]any{"text": "DOC1"}, 10},
		"comodín %":  {map[string]any{"text": "%"}, 1},
		"inyección":  {map[string]any{"text": "'; DROP TABLE events; --"}, 0},
		"combinado":  {map[string]any{"user": "luis", "decision": "ok", "text": "doc"}, 12},
	} {
		if got := e.q(c.f, 1, 5).Total; got != c.want {
			t.Errorf("%s: %d, se esperaban %d", name, got, c.want)
		}
	}
	if e.s.events.count() != 31 {
		t.Fatal("la inyección alteró la tabla")
	}
	// CSV con filtro, columna de equipo y fórmulas neutralizadas.
	var csv string
	json.Unmarshal(e.ok("export_log_csv", map[string]any{"filter": map[string]any{"user": "ana"}}), &csv)
	if !strings.Contains(csv, ",equipo,") || strings.Count(csv, "\n") != 16 || !strings.Contains(csv, `"PC Bodega"`) {
		t.Fatalf("csv: %q", csv)
	}
	json.Unmarshal(e.ok("export_log_csv", nil), &csv)
	if !strings.Contains(csv, `"'=CMD()"`) || strings.Count(csv, "\n") != 32 {
		t.Fatalf("csv completo: %d líneas", strings.Count(csv, "\n"))
	}
	// get_activity sigue disponible (más antiguo primero).
	if l := e.lastLog(1); l[0].Document != "propio.txt" {
		t.Fatalf("get_activity: %+v", l)
	}
	e.ok("clear_log", nil)
	if e.s.events.count() != 0 {
		t.Fatal("clear_log no vació")
	}
}

func TestRetencion(t *testing.T) {
	e := newEnv(t)
	n := time.Now().Unix()
	for i, age := range []int64{400, 200, 10} {
		if err := e.s.events.add(Event{ID: newID(i + 1), TS: n - age*86400, Document: "d", Decision: "allow"}); err != nil {
			t.Fatal(err)
		}
	}
	if k, _ := e.s.events.purge(0); k != 0 || e.s.events.count() != 3 {
		t.Fatalf("0 = sin límite")
	}
	e.s.retention = 365
	e.s.purgeEvents()
	if e.s.events.count() != 2 {
		t.Fatalf("365 días: %d", e.s.events.count())
	}
	e.s.retention = 30
	e.s.purgeEvents()
	if e.s.events.count() != 1 {
		t.Fatalf("30 días: %d", e.s.events.count())
	}
	for v, want := range map[string]int64{"": 365, "90": 90, "0": 0, "-5": 365, "abc": 365, "999999": 365} {
		t.Setenv("CONTROL_LOG_RETENTION_DAYS", v)
		if got := retentionDays(); got != want {
			t.Errorf("CONTROL_LOG_RETENTION_DAYS=%q: %d, se esperaba %d", v, got, want)
		}
	}
}

func tmpDir(t *testing.T) string {
	t.Helper()
	root, _ := filepath.Abs("../../work")
	d, err := os.MkdirTemp(root, "gotest-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(d) })
	return d
}

func TestMigracionDeLogJSONL(t *testing.T) {
	t.Setenv("CONTROL_ADMIN_PASSWORD", "secreto-123")
	dir := tmpDir(t)
	t0 := time.Now().Unix() - 3600
	lines := []string{
		fmt.Sprintf(`{"time":%d,"printer":"Oficina","document":"a.pdf","user":"ana","origin":"10.0.0.5","allowed":true,"reason":"Sin coincidencia"}`, t0),
		fmt.Sprintf(`{"time":%d,"printer":"Oficina","document":"b.docx","user":null,"origin":null,"allowed":false,"reason":"Aplicación: *.docx"}`, t0+1),
		`basura`,
		fmt.Sprintf(`{"time":%d,"printer":"—","document":"aviso","user":null,"origin":null,"allowed":true,"reason":"Aviso del sistema"}`, t0+2),
	}
	orig := strings.Join(lines, "\n") + "\n"
	os.WriteFile(filepath.Join(dir, "log.jsonl"), []byte(orig), 0o600)
	s, err := New(dir)
	if err != nil {
		t.Fatal(err)
	}
	if s.events.count() != 3 {
		t.Fatalf("migrados: %d", s.events.count())
	}
	if _, err := os.Stat(filepath.Join(dir, "log.jsonl")); err == nil {
		t.Fatal("log.jsonl debe renombrarse")
	}
	if b, _ := os.ReadFile(filepath.Join(dir, "log.jsonl.migrado")); string(b) != orig {
		t.Fatal("el respaldo debe quedar intacto")
	}
	rows, _, _ := s.events.query(EventFilter{}, 10, 0)
	if rows[0].Decision != "notice" || rows[1].Decision != "block" || rows[1].User != "" || rows[2].IP != "10.0.0.5" || rows[2].Source != "server" {
		t.Fatalf("filas: %+v", rows)
	}
	// Reabrir no duplica; un log.jsonl nuevo se importa aparte conservando el primer respaldo.
	s.events.Close()
	s2, err := New(dir)
	if err != nil || s2.events.count() != 3 {
		t.Fatalf("reabrir: %v %d", err, s2.events.count())
	}
	s2.events.Close()
	os.WriteFile(filepath.Join(dir, "log.jsonl"), []byte(lines[0]+"\n"), 0o600)
	s3, _ := New(dir)
	if s3.events.count() != 4 {
		t.Fatalf("segundo import: %d", s3.events.count())
	}
	if ms, _ := filepath.Glob(filepath.Join(dir, "log.jsonl.migrado*")); len(ms) != 2 {
		t.Fatalf("respaldos: %v", ms)
	}
	s3.events.Close()
	// Migración interrumpida: archivo ya renombrado y sin importar.
	d2 := tmpDir(t)
	os.WriteFile(filepath.Join(d2, "log.jsonl.migrado"), []byte(lines[0]+"\n"), 0o600)
	s4, _ := New(d2)
	if s4.events.count() != 1 {
		t.Fatalf("interrumpida: %d", s4.events.count())
	}
}

func TestPermisosYEsquemaDeEventsDB(t *testing.T) {
	e := newEnv(t)
	e.s.appendLog(Activity{Time: now(), Printer: "P", Document: "d", Allowed: true})
	for _, f := range []string{"events.db", "events.db-wal", "events.db-shm"} {
		fi, err := os.Stat(filepath.Join(e.s.dir, f))
		if err != nil {
			if f == "events.db" {
				t.Fatal(err)
			}
			continue
		}
		if fi.Mode().Perm() != 0o600 {
			t.Errorf("%s: %v", f, fi.Mode().Perm())
		}
	}
	var mode string
	e.s.events.db.QueryRow("PRAGMA journal_mode").Scan(&mode)
	var v int
	e.s.events.db.QueryRow("PRAGMA user_version").Scan(&v)
	if mode != "wal" || v != 1 {
		t.Errorf("journal_mode=%s user_version=%d", mode, v)
	}
	// Una base de una versión futura no se abre.
	e.s.events.db.Exec("PRAGMA user_version = 99")
	e.s.events.Close()
	if _, err := openEvents(e.s.dir); err == nil || !strings.Contains(err.Error(), "más nueva") {
		t.Errorf("versión futura: %v", err)
	}
}
