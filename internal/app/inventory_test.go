package app

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"controlimpresion/internal/discover"
)

func (e *env) setURI(printer, uri string) {
	e.t.Helper()
	if err := os.WriteFile(filepath.Join(e.fake, "uri."+printer), []byte(uri), 0o644); err != nil {
		e.t.Fatal(err)
	}
}

func (e *env) events(filter map[string]any) []eventRow { return e.q(filter, 1, 100).Rows }

func TestEventoLlevaPuertoTipoPaginasYBytes(t *testing.T) {
	e := newEnv(t)
	e.login()
	e.setURI("Oficina", "socket://admin:clave@10.0.0.5:9100?x=1")
	e.setURI("Taller", "usb://Printronix/T6304?location=14410000")
	for _, p := range []string{"Oficina", "Taller"} {
		e.ok("set_rules", map[string]any{"printer": p, "rules": map[string]any{"enabled": true, "mode": "block", "apps": []string{}, "sites": []string{}, "ips": []string{}}})
	}
	e.jobSized("Oficina", "ana", "pc1", "con-datos.docx", "12", "3")
	e.jobSized("Oficina", "ana", "pc1", "sin-datos.docx", "", "")
	e.jobSized("Taller", "luis", "pc2", "etiqueta.txt", "1", "")
	e.tick()
	by := map[string]eventRow{}
	for _, r := range e.events(nil) {
		by[r.Document] = r
	}
	a := by["con-datos.docx"]
	if a.Port != "socket://10.0.0.5:9100" || a.ConnType != "socket" || a.Pages == nil || *a.Pages != 3 || a.Bytes == nil || *a.Bytes != 12*1024 {
		t.Fatalf("%+v", a)
	}
	if strings.Contains(a.Port, "clave") || strings.Contains(a.Port, "x=1") {
		t.Fatal("el puerto filtra credenciales o consulta")
	}
	b := by["sin-datos.docx"]
	if b.Pages != nil || b.Bytes != nil || b.Port == "" {
		t.Fatalf("no debe inventar páginas ni bytes: %+v", b)
	}
	c := by["etiqueta.txt"]
	if c.ConnType != "usb" || c.Port != "usb://Printronix/T6304" || c.Pages != nil || c.Bytes == nil || *c.Bytes != 1024 {
		t.Fatalf("%+v", c)
	}
	// El CSV lleva las columnas nuevas.
	csv := e.str("export_log_csv", map[string]any{})
	if !strings.Contains(csv, `"socket://10.0.0.5:9100","socket"`) || !strings.Contains(csv, ",3,12288,") {
		t.Fatalf("csv sin las columnas nuevas:\n%s", csv)
	}
}

func TestConnType(t *testing.T) {
	for uri, want := range map[string]string{
		"usb://HP/x?serial=1": "usb", "ippusb://HP/ipp": "usb", "socket://1.2.3.4:9100": "socket", "ipp://h/ipp/print": "ipp", "ipps://h/x": "ipp",
		"http://h:631/printers/x": "ipp", "lpd://1.2.3.4/q": "lpd", "dnssd://Foo._ipp._tcp.local/": "red", "smb://srv/p": "compartida",
		"file:///dev/null": "archivo", "cups-pdf:/": "archivo", "serial:/dev/ttyS0": "local", "": "desconocido", "algo-raro://x": "desconocido",
	} {
		if got := connTypeOf(uri); got != want {
			t.Errorf("%q: %q, quería %q", uri, got, want)
		}
	}
}

func TestInventarioYCopiaDeSeguridad(t *testing.T) {
	e := newEnv(t)
	e.login()
	e.setURI("Oficina", "ipp://user:pw@10.0.0.5:631/ipp/print")
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": map[string]any{"enabled": true, "mode": "block", "apps": []string{}, "sites": []string{}, "ips": []string{}}})
	var inv []InventoryRow
	if err := json.Unmarshal(e.ok("get_inventory", nil), &inv); err != nil || len(inv) != 2 {
		t.Fatal(err, inv)
	}
	o := inv[0]
	if o.Name != "Oficina" || o.URI != "ipp://10.0.0.5:631/ipp/print" || o.ConnType != "ipp" || o.Host != "10.0.0.5" || o.Driver != "Fake Laser 9000" ||
		o.Shared == nil || *o.Shared || !o.Controlled || !o.HoldSupported || o.Kind != "red" || o.Status == "" {
		t.Fatalf("%+v", o)
	}
	if inv[1].Controlled || inv[1].Host != "127.0.0.1" {
		t.Fatalf("%+v", inv[1])
	}
	// La copia de seguridad ya exporta datos reales y el inventario sigue siendo informativo al importar.
	exp := e.str("export_backup", map[string]any{"categories": []string{"inventario"}})
	var f struct {
		Inventario []map[string]any `json:"inventario"`
	}
	if err := json.Unmarshal([]byte(exp), &f); err != nil || len(f.Inventario) != 2 {
		t.Fatal(err, exp)
	}
	it := f.Inventario[0]
	if it["name"] != "Oficina" || it["kind"] != "red" || it["conn_type"] != "ipp" || it["host"] != "10.0.0.5" || it["controlled"] != true || it["driver"] != "Fake Laser 9000" {
		t.Fatalf("%v", it)
	}
	if strings.Contains(exp, "pw@") {
		t.Fatal("la copia filtra credenciales")
	}
	pv := e.obj("preview_import", map[string]any{"json": exp})
	if pv["ok"] != true {
		t.Fatalf("%v", pv)
	}
}

func TestResumenQuienImprimioQue(t *testing.T) {
	e := newEnv(t)
	e.login()
	_, tok := e.devToken("caja")
	pg := func(n int) map[string]any { return map[string]any{"pages": n} }
	mk := func(i int, user, printer, dec string, extra map[string]any) map[string]any {
		m := evJSON(newID(i), "d", user, dec, 1700000000+int64(i))
		m["printer"] = printer
		delete(m, "pages")
		for k, v := range extra {
			m[k] = v
		}
		return m
	}
	e.ingest(tok, mk(1, "ana", "Oficina", "allow", pg(3)), mk(2, "ana", "Oficina", "block", nil), mk(3, "luis", "Taller", "allow", nil),
		mk(4, "ana", "Taller", "allow", pg(2)), mk(5, "", "Taller", "notice", nil))
	var rows []SummaryRow
	sum := func(by string, f map[string]any) []SummaryRow {
		rows = nil
		if err := json.Unmarshal(e.ok("summarize_events", map[string]any{"by": by, "filter": f}), &rows); err != nil {
			t.Fatal(err)
		}
		return rows
	}
	r := sum("user", nil)
	if len(r) != 2 || r[0].Key != "ana" || r[0].Jobs != 3 || r[0].Allowed != 2 || r[0].Blocked != 1 || r[0].Pages == nil || *r[0].Pages != 5 || r[0].PagesJobs != 2 {
		t.Fatalf("%+v", r)
	}
	if r[1].Key != "luis" || r[1].Pages != nil || r[1].PagesJobs != 0 { // ningún trabajo informó páginas: no se inventan
		t.Fatalf("%+v", r[1])
	}
	if r := sum("printer", nil); len(r) != 2 || r[0].Key != "Taller" && r[0].Key != "Oficina" {
		t.Fatalf("%+v", r)
	}
	if r := sum("host", nil); len(r) != 1 || r[0].Key != "caja" || r[0].Jobs != 4 { // etiqueta del token
		t.Fatalf("%+v", r)
	}
	if r := sum("decision", nil); len(r) != 2 || r[0].Key != "allow" || r[0].Jobs != 3 {
		t.Fatalf("%+v", r)
	}
	if r := sum("user", map[string]any{"printer": "Taller"}); len(r) != 2 || r[0].Jobs != 1 {
		t.Fatalf("filtro: %+v", r)
	}
	if code, _ := e.post("summarize_events", map[string]any{"by": "id; DROP TABLE events"}); code != 400 {
		t.Fatalf("agrupación inválida: %d", code)
	}
	if e.q(nil, 1, 10).Total != 5 {
		t.Fatal("la tabla debe seguir intacta")
	}
}

// --- IPP simulado ---

func ipp(status uint16, groups ...[][3]string) []byte {
	var b bytes.Buffer
	b.Write([]byte{1, 1, byte(status >> 8), byte(status), 0, 0, 0, 1})
	for _, g := range groups {
		tag := byte(4)
		if len(g) > 0 && g[0][1] == "job-id" {
			tag = 2
		}
		b.WriteByte(tag)
		for _, a := range g {
			b.WriteByte(a[0][0])
			binary.Write(&b, binary.BigEndian, uint16(len(a[1])))
			b.WriteString(a[1])
			binary.Write(&b, binary.BigEndian, uint16(len(a[2])))
			b.WriteString(a[2])
		}
	}
	b.WriteByte(3)
	return b.Bytes()
}

func i32s(n int) string {
	var x [4]byte
	binary.BigEndian.PutUint32(x[:], uint32(n))
	return string(x[:])
}

func fakeListener(t *testing.T) int {
	t.Helper()
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { l.Close() })
	go func() {
		for {
			c, err := l.Accept()
			if err != nil {
				return
			}
			c.Close()
		}
	}()
	return l.Addr().(*net.TCPAddr).Port
}

func waitDiscovery(t *testing.T, e *env) discJob {
	t.Helper()
	for i := 0; i < 100; i++ {
		var j discJob
		if err := json.Unmarshal(e.ok("discover_status", nil), &j); err != nil {
			t.Fatal(err)
		}
		if !j.Running && j.Started != 0 {
			return j
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatal("el descubrimiento no terminó")
	return discJob{}
}

func TestDescubrimientoDeRedConListenersFalsos(t *testing.T) {
	e := newEnv(t)
	e.login()
	raw := fakeListener(t)
	free := func() int {
		l, _ := net.Listen("tcp", "127.0.0.1:0")
		p := l.Addr().(*net.TCPAddr).Port
		l.Close()
		return p
	}
	ippPort := fakeListener(t) // el puerto IPP "abierto"; el transporte simulado responde
	e.s.net.ports = discover.Ports{Raw: raw, LPD: free(), IPP: ippPort}
	e.s.net.tr = func(_ context.Context, host string, port int, req []byte) ([]byte, error) {
		return ipp(0, [][3]string{{"A", "printer-make-and-model", "Fake Laser 9000"}, {"B", "printer-name", "falsa"}}), nil
	}
	// Por defecto no hay subredes configuradas y no se escanea nada.
	if code, body := e.post("discover_network", map[string]any{}); code != 400 || !strings.Contains(string(body), "no hay subredes") {
		t.Fatalf("%d %s", code, body)
	}
	for _, c := range []string{"10.0.0.0/8", "8.8.8.0/24", "::1/128", "x"} {
		if code, _ := e.post("discover_network", map[string]any{"cidrs": []string{c}}); code != 400 {
			t.Fatalf("%s debía rechazarse: %d", c, code)
		}
	}
	if code, _ := e.post("set_network_settings", map[string]any{"cidrs": []string{"10.0.0.0/8"}}); code != 400 {
		t.Fatalf("set_network_settings acepta /8: %d", code)
	}
	if e.s.net.job != nil {
		t.Fatal("no debió iniciarse ningún descubrimiento")
	}
	// Con una cola que apunta a esta IP y control activo: controlada.
	e.setURI("Taller", "usb://Printronix/T6304")
	e.setURI("Oficina", "socket://127.0.0.1:"+strconv.Itoa(raw))
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": map[string]any{"enabled": true, "mode": "block", "apps": []string{}, "sites": []string{}, "ips": []string{}}})
	e.ok("set_network_settings", map[string]any{"cidrs": []string{"127.0.0.1"}, "direct": map[string]any{"enabled": false, "interval_secs": 60}})
	e.ok("discover_network", map[string]any{})
	if code, _ := e.post("discover_network", map[string]any{}); code != 400 && code != 200 {
		t.Fatalf("segunda consulta: %d", code)
	}
	j := waitDiscovery(t, e)
	if len(j.Devices) != 1 {
		t.Fatalf("%+v", j)
	}
	d := j.Devices[0]
	if d.IP != "127.0.0.1" || !d.IPP || d.Model != "Fake Laser 9000" || d.Managed != "controlada" || d.Queue != "Oficina" || len(d.Ports) != 2 || d.Explanation == "" {
		t.Fatalf("%+v", d)
	}
	if !strings.HasPrefix(j.By, "admin@") || j.Hosts != 1 {
		t.Fatalf("%+v", j)
	}
	// Quedó registrado quién y qué rango.
	found := false
	for _, r := range e.events(map[string]any{"text": "Descubrimiento de red"}) {
		if strings.Contains(r.Document, "iniciado por admin@") && strings.Contains(r.Document, "127.0.0.1") {
			found = true
		}
	}
	if !found {
		t.Fatal("no se registró el descubrimiento en el log")
	}
	// Sin control activo en la cola: cola_sin_control. Sin cola apuntando: sin_cola (riesgo).
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": map[string]any{"enabled": false, "mode": "block", "apps": []string{}, "sites": []string{}, "ips": []string{}}})
	e.ok("discover_network", map[string]any{})
	if d := waitDiscovery(t, e).Devices[0]; d.Managed != "cola_sin_control" {
		t.Fatalf("%+v", d)
	}
	e.setURI("Oficina", "socket://10.9.9.9:9100")
	e.s.routes = routeCache{} // caduca el device-uri recordado
	e.ok("discover_network", map[string]any{})
	d = waitDiscovery(t, e).Devices[0]
	if d.Managed != "sin_cola" || d.Queue != "" || !strings.Contains(d.Explanation, "impresión directa") {
		t.Fatalf("%+v", d)
	}
}

// --- detección de impresión directa con IPP simulado ---

type simPrinter struct {
	counter    int
	hasCounter bool
	jobs       [][3]int // id, impresiones (-1 = no informa)
	fail       bool
}

func (p *simPrinter) tr(_ context.Context, _ string, _ int, req []byte) ([]byte, error) {
	if p.fail {
		return nil, context.DeadlineExceeded
	}
	if req[3] == 0x0B {
		if p.hasCounter {
			return ipp(0, [][3]string{{"!", "printer-impressions-completed", i32s(p.counter)}}), nil
		}
		return ipp(0, [][3]string{{"A", "printer-name", "x"}}), nil
	}
	var gs [][][3]string
	for _, j := range p.jobs {
		g := [][3]string{{"!", "job-id", i32s(j[0])}}
		if j[1] >= 0 {
			g = append(g, [3]string{"!", "job-impressions-completed", i32s(j[1])})
		}
		gs = append(gs, g)
	}
	return ipp(0, gs...), nil
}

func directEvents(e *env) []eventRow {
	var out []eventRow
	for _, r := range e.events(map[string]any{"text": "Impresión directa sospechosa"}) {
		out = append(out, r)
	}
	return out
}

func TestImpresionDirectaSospechosa(t *testing.T) {
	e := newEnv(t)
	e.login()
	e.setURI("Oficina", "socket://192.168.1.50:9100")
	e.setURI("Taller", "usb://Printronix/T6304") // sin host de red: no se consulta
	p := &simPrinter{counter: 100, hasCounter: true}
	e.s.net.tr = p.tr
	t0 := time.Now().Add(-10 * time.Minute)
	tick := func(i int) { e.s.DirectTick(context.Background(), t0.Add(time.Duration(i)*time.Minute)) }

	tick(0) // línea base: nunca alerta
	p.counter = 100
	tick(1) // sin cambios
	if len(directEvents(e)) != 0 {
		t.Fatal("alerta sin cambios")
	}
	p.counter = 107 // sube sin trabajos del servidor: alerta
	tick(2)
	ev := directEvents(e)
	if len(ev) != 1 || ev[0].Printer != "Oficina" || ev[0].Decision != "notice" || ev[0].Pages == nil || *ev[0].Pages != 7 || ev[0].ConnType != "socket" || !strings.Contains(ev[0].Document, "+7 impresiones") {
		t.Fatalf("%+v", ev)
	}
	// El servidor liberó un trabajo de 5 páginas: el aumento de 5 queda explicado.
	e.ok("set_rules", map[string]any{"printer": "Oficina", "rules": map[string]any{"enabled": true, "mode": "block", "apps": []string{}, "sites": []string{}, "ips": []string{}}})
	e.jobSized("Oficina", "ana", "pc1", "liberado.docx", "5", "5")
	e.tick()
	p.counter = 112
	tick(3)
	if len(directEvents(e)) != 1 {
		t.Fatalf("un aumento explicado por el servidor no debe alertar: %+v", directEvents(e))
	}
	// Pero 4 páginas más de lo liberado sí.
	p.counter = 125 // +13 desde 112; el servidor liberó 5 en la ventana
	tick(4)
	if n := len(directEvents(e)); n != 2 {
		t.Fatalf("esperaba 2 alertas, hay %d", n)
	}
	// El contador baja (reinicio de la impresora): se rebasa sin alertar.
	p.counter = 3
	tick(5)
	p.counter = 3
	tick(6)
	if n := len(directEvents(e)); n != 2 {
		t.Fatalf("reinicio del contador: %d", n)
	}
	st := e.s.directStatus()
	if len(st) != 1 || st[0].Printer != "Oficina" || st[0].Source != directSrcPrint || st[0].Alerts != 2 {
		t.Fatalf("%+v", st)
	}
}

func TestImpresionDirectaPorHistorialDeTrabajosYNoVerificable(t *testing.T) {
	e := newEnv(t)
	e.login()
	e.setURI("Taller", "usb://Printronix/T6304")
	e.setURI("Oficina", "ipp://192.168.1.50/ipp/print")
	p := &simPrinter{jobs: [][3]int{{1, 4}}} // sin contador: usa el historial de trabajos terminados
	e.s.net.tr = p.tr
	t0 := time.Now().Add(-10 * time.Minute)
	tick := func(i int) { e.s.DirectTick(context.Background(), t0.Add(time.Duration(i)*time.Minute)) }
	tick(0)
	tick(1)
	p.jobs = append(p.jobs, [3]int{2, 6}, [3]int{3, 1})
	tick(2)
	ev := directEvents(e)
	if len(ev) != 1 || ev[0].Pages == nil || *ev[0].Pages != 7 {
		t.Fatalf("%+v", ev)
	}
	// Trabajo nuevo sin impresiones informadas: se avisa en trabajos y sin páginas inventadas.
	p.jobs = append(p.jobs, [3]int{4, -1})
	tick(3)
	ev = directEvents(e)
	if len(ev) != 2 || ev[0].Pages != nil || !strings.Contains(ev[0].Document, "+1 trabajos") {
		t.Fatalf("%+v", ev)
	}
	// La impresora no responde: figura como no verificable y no se acusa a nadie.
	p.fail = true
	tick(4)
	st := e.s.directStatus()
	if len(st) != 1 || !strings.HasPrefix(st[0].Error, "no verificable") || len(directEvents(e)) != 2 {
		t.Fatalf("%+v", st)
	}
}

func TestDirectaApagadaPorDefectoYAjustes(t *testing.T) {
	e := newEnv(t)
	e.login()
	var n struct {
		CIDRs  []string       `json:"cidrs"`
		Direct directSettings `json:"direct"`
	}
	json.Unmarshal(e.ok("get_network_settings", nil), &n)
	if len(n.CIDRs) != 0 || n.Direct.Enabled || n.Direct.IntervalSecs != 300 {
		t.Fatalf("%+v", n)
	}
	if code, _ := e.post("set_network_settings", map[string]any{"direct": map[string]any{"enabled": true, "interval_secs": 5}}); code != 400 {
		t.Fatalf("intervalo demasiado corto: %d", code)
	}
	e.ok("set_network_settings", map[string]any{"direct": map[string]any{"enabled": true, "interval_secs": 45}})
	json.Unmarshal(e.ok("get_network_settings", nil), &n)
	if !n.Direct.Enabled || n.Direct.IntervalSecs != 45 {
		t.Fatalf("%+v", n)
	}
	if len(e.events(map[string]any{"text": "Detección de impresión directa activada"})) != 1 {
		t.Fatal("no se registró la activación")
	}
}
