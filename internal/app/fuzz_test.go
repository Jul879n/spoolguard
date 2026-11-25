package app

import (
	"encoding/json"
	"os"
	"testing"

	"controlimpresion/internal/rules"
)

// Fuzz del importador de copias: ninguna entrada puede provocar un pánico; si se acepta, el plan
// de importación tampoco. Semillas: los vectores compartidos con Rust.
func FuzzParseBackup(f *testing.F) {
	if b, err := os.ReadFile("../../core/tests/vectors/backup.json"); err == nil {
		var v struct {
			Cases []map[string]json.RawMessage `json:"cases"`
		}
		if json.Unmarshal(b, &v) == nil {
			for _, c := range v.Cases {
				if len(c["file"]) > 0 {
					f.Add([]byte(c["file"]))
				}
			}
		}
	}
	f.Add([]byte(`{"format":"spoolguard-backup","version":1,"reglas":{"impresoras":{"A":{"enabled":true,"mode":"block","apps":["*.docx"],"sites":["a.cl"],"ips":["10.0.0.0/8"]}}}}`))
	f.Add([]byte(`[[[[[[[[[[`))
	f.Fuzz(func(t *testing.T, raw []byte) {
		p, err := parseBackup(raw, 1_800_000_000)
		if err != nil || p == nil {
			return
		}
		cur := Snapshot{Printers: map[string]rules.Rules{}, Params: defaultParams(), Quick: Quick{Blocks: []QuickBlock{}, Paused: map[string]QuickMark{}}}
		for _, m := range []op{opCombinar, opReemplazar} {
			_ = planImport(cur.clone(), p, allModes(m), BackupCtx{Local: []string{"A"}, ExistingIDs: map[string]bool{}, Server: true})
		}
	})
}

// Fuzz de la ingesta de eventos de un equipo (JSON no confiable, lote completo).
func FuzzIngestEvents(f *testing.F) {
	f.Add([]byte(`{"events":[{"id":"abcdefgh","ts":1800000000,"decision":"allow","document":"x","printer":"P"}]}`))
	f.Add([]byte(`{"events":[{"id":"\u0000","ts":-1,"pages":-5,"decision":"x"}]}`))
	f.Fuzz(func(t *testing.T, body []byte) {
		var in struct {
			Events []Event `json:"events"`
		}
		if json.Unmarshal(body, &in) != nil {
			return
		}
		for i := range in.Events {
			_ = validateIngest(&in.Events[i], 1_800_000_000)
			clampEvent(&in.Events[i])
		}
	})
}

// Fuzz del cuerpo de los comandos que reciben reglas.
func FuzzSetRulesArgs(f *testing.F) {
	f.Add([]byte(`{"printer":"A","rules":{"enabled":true,"mode":"allow","apps":["*.pdf"],"sites":["x.cl"],"ips":["1.2.3.0/24","::1"]}}`))
	f.Fuzz(func(t *testing.T, body []byte) {
		var a struct {
			Printer string      `json:"printer"`
			Rules   rules.Rules `json:"rules"`
		}
		if decode(body, &a) != nil {
			return
		}
		a.Rules.Normalize()
		sv, d := "docs.example.com", "u"
		j := &rules.Job{Document: a.Printer, App: &d, User: &d, OriginIP: &sv, Site: &sv}
		_, _ = a.Rules.Explain(j)
		_, _ = resolveHost(a.Printer)
	})
}

func FuzzCSVCell(f *testing.F) {
	f.Add("=1+1")
	f.Fuzz(func(t *testing.T, s string) {
		c := csvCell(s)
		if len(c) < 2 || c[0] != '"' || c[len(c)-1] != '"' {
			t.Fatalf("celda mal formada: %q", c)
		}
		if len(s) > 0 && (s[0] == '=' || s[0] == '+' || s[0] == '-' || s[0] == '@') && c[1] != '\'' {
			t.Fatalf("fórmula sin neutralizar: %q", c)
		}
	})
}
