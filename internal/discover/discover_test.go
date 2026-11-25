package discover

import (
	"bytes"
	"context"
	"encoding/binary"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
)

func TestParseCIDRsValidacion(t *testing.T) {
	ok := func(c []string, pub bool, want int) {
		t.Helper()
		h, err := ParseCIDRs(c, pub)
		if err != nil || len(h) != want {
			t.Fatalf("%v: %d hosts, err=%v (quería %d)", c, len(h), err, want)
		}
	}
	ko := func(c []string, pub bool, frag string) {
		t.Helper()
		_, err := ParseCIDRs(c, pub)
		if err == nil || !strings.Contains(err.Error(), frag) {
			t.Fatalf("%v: err=%v (quería %q)", c, err, frag)
		}
	}
	ok([]string{"192.168.1.0/24"}, false, 254)
	ok([]string{"192.168.1.77/24"}, false, 254) // se normaliza a la red
	ok([]string{"10.0.0.5"}, false, 1)
	ok([]string{"127.0.0.1/32"}, false, 1)
	ok([]string{"127.0.0.1"}, false, 1)
	ok([]string{"192.168.1.0/31"}, false, 2)
	ok([]string{"192.168.1.0/24", "192.168.2.0/24"}, false, 508)
	ok([]string{"192.168.1.0/24", "192.168.1.0/25"}, false, 254) // sin duplicados
	ko(nil, false, "no hay subredes")
	ko([]string{" "}, false, "no hay subredes")
	ko([]string{"10.0.0.0/8"}, false, "prefijo")
	ko([]string{"192.168.0.0/16"}, false, "prefijo")
	ko([]string{"192.168.0.0/23"}, false, "prefijo")
	ko([]string{"0.0.0.0/0"}, false, "prefijo")
	ko([]string{"::1/128"}, false, "IPv4")
	ko([]string{"fe80::/64"}, false, "IPv4")
	ko([]string{"abc"}, false, "inválida")
	ko([]string{"10.0.0.0/33"}, false, "inválido")
	ko([]string{"224.0.0.0/24"}, false, "reservado")
	ko([]string{"0.1.2.0/24"}, false, "reservado")
	ko([]string{"8.8.8.0/24"}, false, "público")
	ko([]string{"8.8.8.8"}, false, "público")
	ko([]string{"100.64.0.0/24"}, false, "público") // CGNAT no es privado
	ok([]string{"8.8.8.0/24"}, true, 254)           // con confirmación explícita
	ko([]string{"192.168.1.0/24", "192.168.2.0/24", "192.168.3.0/24"}, false, "demasiados hosts")
	var many []string
	for i := 0; i < 17; i++ {
		many = append(many, "10.0.0."+strconv.Itoa(i))
	}
	ko(many, false, "demasiados rangos")
	ko([]string{strings.Repeat("1", 80)}, false, "inválido")
}

func TestParsePorts(t *testing.T) {
	if p, err := ParsePorts(""); err != nil || p != DefaultPorts {
		t.Fatal(p, err)
	}
	if p, err := ParsePorts("1,2,3"); err != nil || p != (Ports{1, 2, 3}) {
		t.Fatal(p, err)
	}
	for _, bad := range []string{"1,2", "a,b,c", "0,1,2", "1,2,70000"} {
		if _, err := ParsePorts(bad); err == nil {
			t.Fatal(bad)
		}
	}
}

// fakeIPP arma respuestas IPP para el transporte simulado.
func ippResp(status uint16, groups ...[]ippAttr) []byte {
	var b bytes.Buffer
	b.Write([]byte{1, 1, byte(status >> 8), byte(status), 0, 0, 0, 1})
	put := func(a ippAttr) {
		b.WriteByte(a.tag)
		binary.Write(&b, binary.BigEndian, uint16(len(a.name)))
		b.WriteString(a.name)
		binary.Write(&b, binary.BigEndian, uint16(len(a.value)))
		b.WriteString(a.value)
	}
	for i, g := range groups {
		tag := byte(0x04)
		if len(g) > 0 && g[0].name == "job-id" {
			tag = 0x02
		}
		_ = i
		b.WriteByte(tag)
		for _, a := range g {
			put(a)
		}
	}
	b.WriteByte(0x03)
	return b.Bytes()
}

func i32(n int32) string {
	var x [4]byte
	binary.BigEndian.PutUint32(x[:], uint32(n))
	return string(x[:])
}

func TestParseResponseColeccionesYValoresMultiples(t *testing.T) {
	g := []ippAttr{
		{0x44, "printer-uri-supported", "ipp://x/ipp/print"},
		{0x45, "", "ipps://x/ipp/print"},
		{0x34, "media-col", ""}, {0x4A, "", "media-size"}, {0x34, "", ""}, {0x21, "", i32(5)}, {0x37, "", ""}, {0x37, "", ""},
		{0x23, "printer-state", i32(3)},
		{0x22, "printer-is-accepting-jobs", "\x01"},
		{0x41, "printer-info", "Oficina"},
		{0x21, "printer-impressions-completed", i32(1234)},
	}
	r, err := ParseResponse(ippResp(0, g))
	if err != nil {
		t.Fatal(err)
	}
	a := r.Groups[0].Attrs
	if len(a["printer-uri-supported"]) != 2 || a["printer-state"][0] != "3" || a["printer-is-accepting-jobs"][0] != "true" || a["printer-impressions-completed"][0] != "1234" {
		t.Fatalf("%v", a)
	}
	if _, ok := a["media-col"]; ok || len(a["printer-info"]) != 1 {
		t.Fatalf("colección mal tratada: %v", a)
	}
}

func TestParseResponseTruncadaYBasura(t *testing.T) {
	good := ippResp(0, []ippAttr{{0x41, "printer-info", "abcdef"}})
	for n := 0; n < len(good)-1; n++ {
		ParseResponse(good[:n]) // no debe entrar en pánico
	}
	if _, err := ParseResponse(good[:12]); err == nil {
		t.Fatal("truncada debería fallar")
	}
	if _, err := ParseResponse([]byte("HTTP/1.1 200")); err == nil {
		// 12 bytes de texto: no es IPP; no debe pánico y suele fallar por longitud
		t.Log("texto aceptado como respuesta vacía (sin atributos)")
	}
}

func TestEncodeRequestRoundTrip(t *testing.T) {
	b := encodeRequest(0x000B, "ipp://h:631/ipp/print", requested([]string{"a", "b"}))
	r, err := ParseResponse(b) // la estructura de atributos es la misma
	if err != nil || len(r.Groups) != 1 {
		t.Fatal(err)
	}
	g := r.Groups[0].Attrs
	if g["printer-uri"][0] != "ipp://h:631/ipp/print" || strings.Join(g["requested-attributes"], ",") != "a,b" || g["attributes-charset"][0] != "utf-8" {
		t.Fatalf("%v", g)
	}
	if b[2] != 0 || b[3] != 0x0B {
		t.Fatal("operación")
	}
}

func listener(t *testing.T) (port int) {
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

func TestScanConListenersFalsosEnLoopback(t *testing.T) {
	raw := listener(t)
	// IPP falso: servidor HTTP real en 127.0.0.1 que habla IPP mínimo.
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "POST" || r.Header.Get("Content-Type") != "application/ipp" {
			http.Error(w, "no", 400)
			return
		}
		w.Header().Set("Content-Type", "application/ipp")
		w.Write(ippResp(0, []ippAttr{{0x41, "printer-make-and-model", "Fake Laser 9000"}, {0x42, "printer-name", "fake"}, {0x23, "printer-state", i32(3)}, {0x45, "printer-uri-supported", "ipp://127.0.0.1/ipp/print"}}))
	}))
	defer srv.Close()
	ippPort, _ := strconv.Atoi(srv.URL[strings.LastIndex(srv.URL, ":")+1:])
	// Un puerto libre garantizado: se abre y se cierra.
	lpd := func() int {
		l, _ := net.Listen("tcp", "127.0.0.1:0")
		p := l.Addr().(*net.TCPAddr).Port
		l.Close()
		return p
	}()

	hosts, err := ParseCIDRs([]string{"127.0.0.1"}, false)
	if err != nil {
		t.Fatal(err)
	}
	var pm sync.Mutex
	var last, tot int
	sc := &Scanner{Ports: Ports{raw, lpd, ippPort}, Progress: func(d, t int) { pm.Lock(); last, tot = max(last, d), t; pm.Unlock() }}
	devs := sc.Scan(context.Background(), hosts)
	if len(devs) != 1 {
		t.Fatalf("%+v", devs)
	}
	d := devs[0]
	if d.IP != "127.0.0.1" || len(d.Ports) != 2 || contains(d.Ports, lpd) || !d.IPP || d.Model != "Fake Laser 9000" || d.Name != "fake" || d.State != "lista" {
		t.Fatalf("%+v", d)
	}
	if last != 3 || tot != 3 {
		t.Fatalf("progreso %d/%d", last, tot)
	}
}

func TestScanNoAbreMasDeOchoConexiones(t *testing.T) {
	hosts, _ := ParseCIDRs([]string{"10.1.2.0/24"}, false)
	var cur, peak int32
	var mu = make(chan struct{}, 1)
	dial := func(ctx context.Context, addr string) (net.Conn, error) {
		mu <- struct{}{}
		cur++
		if cur > peak {
			peak = cur
		}
		<-mu
		select { // simula un host que no responde hasta el timeout
		case <-ctx.Done():
		}
		mu <- struct{}{}
		cur--
		<-mu
		return nil, ctx.Err()
	}
	sc := &Scanner{Dial: dial}
	ctx := context.Background()
	done := make(chan []Device)
	go func() { done <- sc.Scan(ctx, hosts[:5]) }()
	// 15 sondeos de 500 ms con 8 en paralelo: termina solo en ~1 s.
	devs := <-done
	if len(devs) != 0 {
		t.Fatal(devs)
	}
	if peak > Parallel {
		t.Fatalf("concurrencia %d > %d", peak, Parallel)
	}
}

func TestGetCompletedJobs(t *testing.T) {
	tr := func(ctx context.Context, host string, port int, req []byte) ([]byte, error) {
		if req[3] != 0x0A {
			t.Fatalf("operación %x", req[3])
		}
		return ippResp(0,
			[]ippAttr{{0x21, "job-id", i32(7)}, {0x21, "job-impressions-completed", i32(3)}},
			[]ippAttr{{0x21, "job-id", i32(8)}}), nil
	}
	jobs, err := GetCompletedJobs(context.Background(), tr, "h", 631)
	if err != nil || len(jobs) != 2 || jobs[0].ID != 7 || jobs[0].Impressions != 3 || jobs[1].Impressions != -1 {
		t.Fatal(jobs, err)
	}
	bad := func(context.Context, string, int, []byte) ([]byte, error) { return ippResp(0x0400), nil }
	if _, err := GetCompletedJobs(context.Background(), bad, "h", 631); err == nil {
		t.Fatal("estado de error ignorado")
	}
}
