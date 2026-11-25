package app

import (
	"controlimpresion/internal/rules"
	"encoding/json"
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"
)

// Vectores compartidos con Rust (core/src/backup.rs): core/tests/vectors/backup.json.

func anyOf(t *testing.T, v any) any {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out any
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func loadSnap(t *testing.T, raw json.RawMessage) Snapshot {
	t.Helper()
	s := Snapshot{Printers: map[string]rules.Rules{}, Params: defaultParams(), Quick: Quick{Blocks: []QuickBlock{}, Paused: map[string]QuickMark{}}}
	if len(raw) > 0 {
		if err := json.Unmarshal(raw, &s); err != nil {
			t.Fatal(err)
		}
	}
	return s.clone()
}

func strList(v any) []string {
	out := []string{}
	a, _ := v.([]any)
	for _, x := range a {
		if s, ok := x.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

func rawOf(t *testing.T, c map[string]json.RawMessage) []byte {
	var r string
	if json.Unmarshal(c["raw"], &r) == nil && c["raw"] != nil {
		return []byte(r)
	}
	var gen struct{ Pad, Deep int }
	if c["gen"] != nil {
		json.Unmarshal(c["gen"], &gen)
		if gen.Pad > 0 {
			return []byte(`{"format":"spoolguard-backup","version":1,"pad":"` + strings.Repeat("a", gen.Pad) + `"}`)
		}
		return []byte(strings.Repeat("[", gen.Deep) + strings.Repeat("]", gen.Deep))
	}
	return c["file"]
}

func lookup(v any, path string) any {
	for _, k := range strings.Split(path, ".") {
		m, ok := v.(map[string]any)
		if !ok {
			return nil
		}
		v = m[k]
	}
	return v
}

func TestVectoresCopia(t *testing.T) {
	b, err := os.ReadFile("../../core/tests/vectors/backup.json")
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Now   int64                        `json:"now"`
		Cases []map[string]json.RawMessage `json:"cases"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	if len(f.Cases) < 25 {
		t.Fatalf("pocos casos: %d", len(f.Cases))
	}
	str := func(c map[string]json.RawMessage, k string) string { var s string; json.Unmarshal(c[k], &s); return s }
	for _, c := range f.Cases {
		name := str(c, "name")
		var modo any
		json.Unmarshal(c["modo"], &modo)
		switch str(c, "kind") {
		case "parse_error":
			_, err := parseBackup(rawOf(t, c), f.Now)
			if err == nil || !strings.Contains(err.Error(), str(c, "error")) {
				t.Errorf("%s: error %v, esperado «%s»", name, err, str(c, "error"))
			}
		case "modes_error":
			_, err := parseModes(modo)
			if err == nil || !strings.Contains(err.Error(), str(c, "error")) {
				t.Errorf("%s: error %v, esperado «%s»", name, err, str(c, "error"))
			}
		case "import":
			p, err := parseBackup(rawOf(t, c), f.Now)
			if err != nil {
				t.Errorf("%s: %v", name, err)
				continue
			}
			var in struct {
				Local       []string `json:"local"`
				ExistingIDs []string `json:"existing_ids"`
				EventCount  int64    `json:"event_count"`
				Server      bool     `json:"server"`
			}
			b, _ := json.Marshal(c)
			json.Unmarshal(b, &in)
			ctx := BackupCtx{Local: in.Local, ExistingIDs: map[string]bool{}, EventCount: in.EventCount, Server: in.Server}
			for _, id := range in.ExistingIDs {
				ctx.ExistingIDs[id] = true
			}
			m, err := parseModes(modo)
			if err != nil {
				t.Fatalf("%s: %v", name, err)
			}
			pl := planImport(loadSnap(t, c["current"]), p, m, ctx)
			var ex map[string]any
			json.Unmarshal(c["expect"], &ex)
			pv := anyOf(t, pl.Preview)
			if want := ex["ok"].(bool); pl.OK != want {
				t.Errorf("%s: ok=%v, esperado %v (%v)", name, pl.OK, want, lookup(pv, "errores"))
			}
			if lookup(pv, "ok") != pl.OK {
				t.Errorf("%s: preview.ok", name)
			}
			for _, k := range []struct{ key, field string }{{"errors_contain", "errores"}, {"warnings_contain", "avisos"}} {
				all := strings.Join(strList(lookup(pv, k.field)), "\n")
				for _, w := range strList(ex[k.key]) {
					if !strings.Contains(all, w) {
						t.Errorf("%s: falta «%s» en %s:\n%s", name, w, k.field, all)
					}
				}
			}
			if want, ok := ex["preview"].(map[string]any); ok {
				for k, v := range want {
					if got := lookup(pv, k); !reflect.DeepEqual(got, v) {
						t.Errorf("%s: preview.%s = %v, esperado %v", name, k, got, v)
					}
				}
			}
			if n, ok := ex["next"]; ok && pl.OK {
				nb, _ := json.Marshal(n)
				if got, want := anyOf(t, pl.Next.value()), anyOf(t, loadSnap(t, nb).value()); !reflect.DeepEqual(got, want) {
					t.Errorf("%s: next\n got %v\nwant %v", name, got, want)
				}
			}
			if a, ok := ex["applied"]; ok && !reflect.DeepEqual(pl.Applied, strList(a)) {
				t.Errorf("%s: applied %v, esperado %v", name, pl.Applied, strList(a))
			}
			if ids, ok := ex["event_ids"]; ok {
				got := []string{}
				for _, e := range pl.Events {
					got = append(got, e.ID)
				}
				if !reflect.DeepEqual(got, strList(ids)) {
					t.Errorf("%s: event_ids %v, esperado %v", name, got, strList(ids))
				}
			}
			if r, ok := ex["replace_events"]; ok && pl.ReplaceEvents != r.(bool) {
				t.Errorf("%s: replace_events", name)
			}
		case "export":
			state := loadSnap(t, c["state"])
			var evs []Event
			var inv []InvItem
			var meta struct {
				Created    int64  `json:"created"`
				Host       string `json:"host"`
				AppVersion string `json:"app_version"`
			}
			var cats []string
			var server bool
			json.Unmarshal(c["events"], &evs)
			json.Unmarshal(c["inventory"], &inv)
			json.Unmarshal(c["meta"], &meta)
			json.Unmarshal(c["categories"], &cats)
			json.Unmarshal(c["server"], &server)
			file := exportBackup(state, cats, evs, inv, BackupMeta{meta.Created, meta.Host, meta.AppVersion})
			var want any
			json.Unmarshal(c["expect_file"], &want)
			if got := anyOf(t, file); !reflect.DeepEqual(got, want) {
				gb, _ := json.Marshal(got)
				wb, _ := json.Marshal(want)
				t.Errorf("%s: archivo exportado\n got %s\nwant %s", name, gb, wb)
				continue
			}
			raw, _ := json.Marshal(file)
			p, err := parseBackup(raw, f.Now)
			if err != nil || len(p.Errors) > 0 {
				t.Errorf("%s: %v %v", name, err, p.Errors)
				continue
			}
			pl := planImport(loadSnap(t, nil), p, allModes(opReemplazar), BackupCtx{Server: server})
			if !pl.OK || !reflect.DeepEqual(anyOf(t, pl.Next.value()), anyOf(t, state.value())) {
				t.Errorf("%s: ida y vuelta\n got %v\nwant %v", name, anyOf(t, pl.Next.value()), anyOf(t, state.value()))
			}
			if evs == nil {
				evs = []Event{}
			}
			if !reflect.DeepEqual(anyOf(t, pl.Events), anyOf(t, evs)) {
				t.Errorf("%s: eventos de ida y vuelta", name)
			}
			ids := map[string]bool{}
			for _, e := range evs {
				ids[e.ID] = true
			}
			pl2 := planImport(pl.Next, p, allModes(opCombinar), BackupCtx{Server: server, ExistingIDs: ids})
			if len(pl2.Events) != 0 || !reflect.DeepEqual(anyOf(t, pl2.Next.value()), anyOf(t, state.value())) {
				t.Errorf("%s: combinar sobre lo mismo debe ser idempotente", name)
			}
		default:
			t.Errorf("%s: tipo de caso desconocido", name)
		}
	}
}

func TestArchivoExportadoSinSecretos(t *testing.T) {
	s := loadSnap(t, nil)
	f := exportBackup(s, backupCats, nil, nil, BackupMeta{1, "h", "v"})
	b, _ := json.Marshal(f)
	for _, w := range []string{"password", "hash", "salt", "token", "session", "secret"} {
		if strings.Contains(strings.ToLower(string(b)), w) {
			t.Errorf("el archivo contiene «%s»: %s", w, b)
		}
	}
}

func TestListasGigantesSeRechazan(t *testing.T) {
	apps := make([]string, 3000)
	for i := range apps {
		apps[i] = "a"
	}
	b, _ := json.Marshal(map[string]any{"format": "spoolguard-backup", "version": 1, "reglas": map[string]any{"printers": map[string]any{"P": map[string]any{"apps": apps}}}})
	p, err := parseBackup(b, 1_800_000_000)
	if err != nil || len(p.Errors) == 0 {
		t.Fatalf("debía acumular errores: %v %v", err, p)
	}
	if pl := planImport(loadSnap(t, nil), p, allModes(opReemplazar), BackupCtx{}); pl.OK {
		t.Fatal("no debe poder aplicarse")
	}
}

func TestSanitizeURI(t *testing.T) {
	for in, want := range map[string]string{
		"ipp://admin:clave@10.0.0.5/ipp/print?x=1": "ipp://10.0.0.5/ipp/print",
		"socket://10.0.0.5:9100":                   "socket://10.0.0.5:9100",
		"USB001":                                   "USB001",
	} {
		if got := sanitizeURI(in); got != want {
			t.Errorf("%s -> %s, esperado %s", in, got, want)
		}
	}
	_ = fmt.Sprint
}
