package app

import (
	"bufio"
	"net/http"
	"strings"
	"testing"
	"time"

	"controlimpresion/internal/rules"
)

// leeEvento devuelve la próxima línea `data:` del flujo (o falla por tiempo).
func leeEvento(t *testing.T, sc *bufio.Scanner) string {
	t.Helper()
	got := make(chan string, 1)
	go func() {
		for sc.Scan() {
			if d, ok := strings.CutPrefix(sc.Text(), "data: "); ok {
				got <- d
				return
			}
		}
		got <- ""
	}()
	select {
	case d := <-got:
		return d
	case <-time.After(5 * time.Second):
		t.Fatal("sin evento en 5 s")
		return ""
	}
}

func TestExtEventsEmpujaReglas(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.extToken("flujo")

	if code, _, _ := e.extReq("GET", "/ext/events", "", "", ""); code != 401 {
		t.Fatalf("sin token: %d", code)
	}

	req, _ := http.NewRequest("GET", e.ts.URL+"/ext/events", nil)
	req.Header.Set("Authorization", "Bearer "+tok)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 || !strings.HasPrefix(resp.Header.Get("Content-Type"), "text/event-stream") {
		t.Fatalf("flujo: %d %q", resp.StatusCode, resp.Header.Get("Content-Type"))
	}
	sc := bufio.NewScanner(resp.Body)

	if d := leeEvento(t, sc); !strings.Contains(d, `"block":[]`) {
		t.Fatalf("estado inicial: %s", d)
	}

	// Un cambio de reglas llega sin sondeo.
	e.s.mu.Lock()
	e.s.cfg.Global = rules.Rules{Enabled: true, Mode: ptr("block"), Sites: []string{"ejemplo.cl"}}
	err = e.s.saveLocked()
	e.s.mu.Unlock()
	if err != nil {
		t.Fatal(err)
	}
	if d := leeEvento(t, sc); !strings.Contains(d, "ejemplo.cl") {
		t.Fatalf("tras el cambio: %s", d)
	}
}

func TestActiveSitesOmiteSuspendidos(t *testing.T) {
	r := rules.Rules{Sites: []string{"a.cl", "b.cl", "c.cl"}, Paused: map[string]int64{"a.cl": 0, "b.cl": 1}}
	got := activeSites(&r)
	if len(got) != 2 || got[0] != "b.cl" || got[1] != "c.cl" {
		t.Fatalf("activeSites = %v; quería [b.cl c.cl] (a.cl suspendido, b.cl vencido)", got)
	}
}
