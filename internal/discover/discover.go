// Package discover: descubrimiento opcional de impresoras en subredes que el administrador configura a mano.
// Solo hace conexiones TCP a 9100/515/631 (sin SNMP, sin exploits) y, si el 631 responde, una consulta IPP
// Get-Printer-Attributes. Todo está acotado: prefijo >= /24, tope de hosts, concurrencia y tiempo por conexión.
package discover

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	MaxHosts     = 512 // tope de hosts por consulta
	MaxCIDRs     = 16
	MinPrefix    = 24 // /24 como máximo por rango
	Parallel     = 8
	DialTimeout  = 500 * time.Millisecond
	ippTimeout   = 2 * time.Second
	maxIPPReply  = 1 << 20
	portRawPrint = 9100
	portLPD      = 515
	portIPP      = 631
)

// Ports son los tres puertos que se prueban. Solo las pruebas (y CONTROL_DISCOVER_PORTS) los cambian.
type Ports struct{ Raw, LPD, IPP int }

var DefaultPorts = Ports{portRawPrint, portLPD, portIPP}

func (p Ports) list() []int { return []int{p.Raw, p.LPD, p.IPP} }

// ParsePorts lee "raw,lpd,ipp" (por ejemplo "19100,1515,1631"); vacío = los puertos normales.
func ParsePorts(s string) (Ports, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return DefaultPorts, nil
	}
	f := strings.Split(s, ",")
	if len(f) != 3 {
		return Ports{}, errors.New("se esperan 3 puertos: raw,lpd,ipp")
	}
	var v [3]int
	for i, x := range f {
		n, err := strconv.Atoi(strings.TrimSpace(x))
		if err != nil || n < 1 || n > 65535 {
			return Ports{}, errors.New("puerto inválido")
		}
		v[i] = n
	}
	return Ports{v[0], v[1], v[2]}, nil
}

// ParseCIDRs valida los rangos y devuelve los hosts a probar. Rechaza: no IPv4, prefijo menor que /24
// (por ejemplo /8), direcciones no unicast (0.0.0.0, multicast, reservadas), más de MaxHosts hosts en total y
// rangos públicos salvo que allowPublic sea true (confirmación explícita del administrador).
func ParseCIDRs(cidrs []string, allowPublic bool) ([]netip.Addr, error) {
	var in []string
	for _, c := range cidrs {
		if c = strings.TrimSpace(c); c != "" {
			in = append(in, c)
		}
	}
	if len(in) == 0 {
		return nil, errors.New("no hay subredes configuradas: indica al menos un rango (por ejemplo 192.168.1.0/24)")
	}
	if len(in) > MaxCIDRs {
		return nil, fmt.Errorf("demasiados rangos (máx. %d)", MaxCIDRs)
	}
	seen := map[netip.Addr]bool{}
	var hosts []netip.Addr
	for _, c := range in {
		if len(c) > 64 {
			return nil, errors.New("rango inválido")
		}
		var p netip.Prefix
		if strings.Contains(c, "/") {
			var err error
			if p, err = netip.ParsePrefix(c); err != nil {
				return nil, fmt.Errorf("CIDR inválido: %q", c)
			}
		} else {
			a, err := netip.ParseAddr(c)
			if err != nil {
				return nil, fmt.Errorf("dirección inválida: %q", c)
			}
			p = netip.PrefixFrom(a, a.BitLen())
		}
		a := p.Addr()
		if !a.Is4() {
			return nil, fmt.Errorf("%q: solo IPv4", c)
		}
		if p.Bits() < MinPrefix {
			return nil, fmt.Errorf("%q: el prefijo debe ser /%d a /32 (máx. 256 direcciones por rango)", c, MinPrefix)
		}
		p = p.Masked()
		a = p.Addr()
		if a.IsUnspecified() || a.IsMulticast() || a.As4()[0] == 0 || a.As4()[0] >= 240 {
			return nil, fmt.Errorf("%q: rango reservado o no válido", c)
		}
		private := a.IsPrivate() || a.IsLoopback() || a.IsLinkLocalUnicast()
		if !private && !allowPublic {
			return nil, fmt.Errorf("%q es un rango público: requiere confirmación explícita", c)
		}
		first, last := p.Addr(), lastAddr(p)
		for h := first; ; h = h.Next() {
			skip := p.Bits() <= 30 && (h == first || h == last) // red y difusión
			if !skip && !seen[h] {
				seen[h] = true
				hosts = append(hosts, h)
				if len(hosts) > MaxHosts {
					return nil, fmt.Errorf("demasiados hosts (máx. %d por consulta)", MaxHosts)
				}
			}
			if h == last {
				break
			}
		}
	}
	return hosts, nil
}

func lastAddr(p netip.Prefix) netip.Addr {
	b := p.Addr().As4()
	n := binary.BigEndian.Uint32(b[:])
	n |= ^uint32(0) >> uint(p.Bits())
	binary.BigEndian.PutUint32(b[:], n)
	return netip.AddrFrom4(b)
}

// Device es lo que se supo de un host con algún puerto de impresión abierto.
type Device struct {
	IP       string   `json:"ip"`
	Ports    []int    `json:"ports"`
	IPP      bool     `json:"ipp"` // respondió Get-Printer-Attributes
	Model    string   `json:"model"`
	Name     string   `json:"name"`
	Info     string   `json:"info"`
	Location string   `json:"location"`
	State    string   `json:"state"`
	URIs     []string `json:"uris,omitempty"`
}

// Dialer abre una conexión TCP (se inyecta en las pruebas).
type Dialer func(ctx context.Context, addr string) (net.Conn, error)

func defaultDial(ctx context.Context, addr string) (net.Conn, error) {
	d := net.Dialer{Timeout: DialTimeout}
	return d.DialContext(ctx, "tcp", addr)
}

// Scanner agrupa las dependencias inyectables.
type Scanner struct {
	Ports     Ports
	Dial      Dialer    // por defecto, TCP con timeout de 500 ms
	Transport Transport // por defecto, HTTP simple al puerto IPP
	Progress  func(done, total int)
}

func (sc *Scanner) dial() Dialer {
	if sc.Dial != nil {
		return sc.Dial
	}
	return defaultDial
}

func (sc *Scanner) ports() Ports {
	if sc.Ports == (Ports{}) {
		return DefaultPorts
	}
	return sc.Ports
}

// Scan prueba host por host (Parallel conexiones a la vez como máximo) y devuelve los dispositivos con algún
// puerto abierto, ordenados por IP. Respeta la cancelación del contexto.
func (sc *Scanner) Scan(ctx context.Context, hosts []netip.Addr) []Device {
	ports := sc.ports()
	type job struct {
		host netip.Addr
		port int
	}
	jobs := make(chan job)
	var mu sync.Mutex
	open := map[netip.Addr][]int{}
	done, total := 0, len(hosts)*3
	var wg sync.WaitGroup
	for i := 0; i < Parallel; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer func() { _ = recover() }() // un fallo en un sondeo no debe tumbar el servidor
			for j := range jobs {
				ok := false
				if ctx.Err() == nil {
					cctx, cancel := context.WithTimeout(ctx, DialTimeout)
					c, err := sc.dial()(cctx, net.JoinHostPort(j.host.String(), strconv.Itoa(j.port)))
					cancel()
					if err == nil {
						c.Close()
						ok = true
					}
				}
				mu.Lock()
				if ok {
					open[j.host] = append(open[j.host], j.port)
				}
				done++
				d := done
				mu.Unlock()
				if sc.Progress != nil {
					sc.Progress(d, total)
				}
			}
		}()
	}
feed:
	for _, h := range hosts {
		for _, p := range ports.list() {
			select {
			case jobs <- job{h, p}:
			case <-ctx.Done():
				break feed
			}
		}
	}
	close(jobs)
	wg.Wait()

	var out []Device
	for h, ps := range open {
		sort.Ints(ps)
		d := Device{IP: h.String(), Ports: ps}
		if contains(ps, ports.IPP) && ctx.Err() == nil {
			sc.identify(ctx, &d, ports.IPP)
		}
		out = append(out, d)
	}
	sort.Slice(out, func(i, j int) bool {
		a, b := netip.MustParseAddr(out[i].IP), netip.MustParseAddr(out[j].IP)
		return a.Less(b)
	})
	return out
}

func contains(v []int, n int) bool {
	for _, x := range v {
		if x == n {
			return true
		}
	}
	return false
}

func (sc *Scanner) identify(ctx context.Context, d *Device, port int) {
	tr := sc.Transport
	if tr == nil {
		tr = HTTPTransport
	}
	attrs, err := GetPrinterAttributes(ctx, tr, d.IP, port, []string{"printer-make-and-model", "printer-name", "printer-info", "printer-location", "printer-state", "printer-uri-supported"})
	if err != nil || len(attrs) == 0 {
		return
	}
	d.IPP = true
	first := func(k string) string {
		if v := attrs[k]; len(v) > 0 {
			return v[0]
		}
		return ""
	}
	d.Model, d.Name, d.Info, d.Location = first("printer-make-and-model"), first("printer-name"), first("printer-info"), first("printer-location")
	d.State = map[string]string{"3": "lista", "4": "imprimiendo", "5": "detenida"}[first("printer-state")]
	d.URIs = attrs["printer-uri-supported"]
}

// --- IPP mínimo (RFC 8010): solo lo necesario para Get-Printer-Attributes y Get-Jobs ---

// Transport envía un mensaje IPP a host:port y devuelve la respuesta. Se inyecta en las pruebas.
type Transport func(ctx context.Context, host string, port int, request []byte) ([]byte, error)

// HTTPTransport hace POST application/ipp a /ipp/print y, si no existe, a /.
func HTTPTransport(ctx context.Context, host string, port int, req []byte) ([]byte, error) {
	var last error
	for _, path := range []string{"/ipp/print", "/"} {
		ctx2, cancel := context.WithTimeout(ctx, ippTimeout)
		body, err := postIPP(ctx2, host, port, path, req)
		cancel()
		if err == nil {
			return body, nil
		}
		last = err
	}
	return nil, last
}

func postIPP(ctx context.Context, host string, port int, path string, req []byte) ([]byte, error) {
	u := "http://" + net.JoinHostPort(host, strconv.Itoa(port)) + path
	r, err := http.NewRequestWithContext(ctx, http.MethodPost, u, bytes.NewReader(req))
	if err != nil {
		return nil, err
	}
	r.Header.Set("Content-Type", "application/ipp")
	cl := &http.Client{Transport: &http.Transport{DisableKeepAlives: true, DialContext: func(c context.Context, n, a string) (net.Conn, error) {
		d := net.Dialer{Timeout: DialTimeout}
		return d.DialContext(c, n, a)
	}}, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	resp, err := cl.Do(r)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	return io.ReadAll(io.LimitReader(resp.Body, maxIPPReply))
}

type ippAttr struct {
	tag   byte
	name  string
	value string
}

// encodeRequest arma una petición IPP 1.1. attrs son atributos de operación adicionales.
func encodeRequest(op uint16, printerURI string, attrs []ippAttr) []byte {
	var b bytes.Buffer
	b.Write([]byte{1, 1, byte(op >> 8), byte(op), 0, 0, 0, 1, 0x01})
	put := func(tag byte, name, val string) {
		b.WriteByte(tag)
		_ = binary.Write(&b, binary.BigEndian, uint16(len(name)))
		b.WriteString(name)
		_ = binary.Write(&b, binary.BigEndian, uint16(len(val)))
		b.WriteString(val)
	}
	put(0x47, "attributes-charset", "utf-8")
	put(0x48, "attributes-natural-language", "en")
	put(0x45, "printer-uri", printerURI)
	for _, a := range attrs {
		put(a.tag, a.name, a.value)
	}
	b.WriteByte(0x03)
	return b.Bytes()
}

func requested(names []string) []ippAttr {
	out := make([]ippAttr, len(names))
	for i, n := range names {
		out[i] = ippAttr{0x44, "requested-attributes", n}
		if i > 0 {
			out[i].name = "" // valor adicional del mismo atributo
		}
	}
	return out
}

// Group es un grupo de atributos de la respuesta (tag 0x01 operación, 0x02 trabajo, 0x04 impresora...).
type Group struct {
	Tag   byte
	Attrs map[string][]string
}

// Response es una respuesta IPP decodificada.
type Response struct {
	Status uint16
	Groups []Group
}

// ParseResponse decodifica una respuesta IPP con límites estrictos; ignora colecciones y tipos que no se usan.
func ParseResponse(b []byte) (*Response, error) {
	if len(b) < 9 {
		return nil, errors.New("respuesta IPP demasiado corta")
	}
	r := &Response{Status: binary.BigEndian.Uint16(b[2:4])}
	i := 8
	var cur *Group
	last, depth := "", 0
	for i < len(b) {
		tag := b[i]
		i++
		if tag == 0x03 {
			return r, nil
		}
		if tag <= 0x0F {
			r.Groups = append(r.Groups, Group{Tag: tag, Attrs: map[string][]string{}})
			cur = &r.Groups[len(r.Groups)-1]
			last = ""
			continue
		}
		if i+2 > len(b) {
			return nil, errors.New("IPP truncado")
		}
		nl := int(binary.BigEndian.Uint16(b[i:]))
		i += 2
		if i+nl+2 > len(b) {
			return nil, errors.New("IPP truncado")
		}
		name := string(b[i : i+nl])
		i += nl
		vl := int(binary.BigEndian.Uint16(b[i:]))
		i += 2
		if i+vl > len(b) {
			return nil, errors.New("IPP truncado")
		}
		val := b[i : i+vl]
		i += vl
		switch tag {
		case 0x34:
			depth++
			continue
		case 0x37:
			if depth > 0 {
				depth--
			}
			continue
		}
		if depth > 0 || cur == nil {
			continue
		}
		var s string
		switch tag {
		case 0x21, 0x23:
			if vl != 4 {
				continue
			}
			s = strconv.FormatInt(int64(int32(binary.BigEndian.Uint32(val))), 10)
		case 0x22:
			if vl != 1 {
				continue
			}
			s = strconv.FormatBool(val[0] != 0)
		case 0x41, 0x42, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49:
			s = string(val)
		default:
			continue
		}
		if name == "" {
			name = last
		}
		if name == "" {
			continue
		}
		last = name
		cur.Attrs[name] = append(cur.Attrs[name], s)
	}
	return r, nil
}

func printerURI(host string, port int) string {
	return "ipp://" + net.JoinHostPort(host, strconv.Itoa(port)) + "/ipp/print"
}

func roundTrip(ctx context.Context, tr Transport, host string, port int, req []byte) (*Response, error) {
	b, err := tr(ctx, host, port, req)
	if err != nil {
		return nil, err
	}
	r, err := ParseResponse(b)
	if err != nil {
		return nil, err
	}
	if r.Status >= 0x0100 {
		return nil, fmt.Errorf("IPP status 0x%04x", r.Status)
	}
	return r, nil
}

// GetPrinterAttributes (operación 0x000B) devuelve los atributos de impresora pedidos.
func GetPrinterAttributes(ctx context.Context, tr Transport, host string, port int, names []string) (map[string][]string, error) {
	r, err := roundTrip(ctx, tr, host, port, encodeRequest(0x000B, printerURI(host, port), requested(names)))
	if err != nil {
		return nil, err
	}
	out := map[string][]string{}
	for _, g := range r.Groups {
		if g.Tag == 0x04 {
			for k, v := range g.Attrs {
				out[k] = v
			}
		}
	}
	return out, nil
}

// CompletedJob es un trabajo terminado según la propia impresora.
type CompletedJob struct {
	ID          int64
	Impressions int64 // job-impressions-completed; -1 si no lo informa
}

// GetCompletedJobs (Get-Jobs, which-jobs=completed) devuelve los trabajos terminados que la impresora recuerda.
func GetCompletedJobs(ctx context.Context, tr Transport, host string, port int) ([]CompletedJob, error) {
	attrs := append(requested([]string{"job-id", "job-impressions-completed"}),
		ippAttr{0x44, "which-jobs", "completed"}, ippAttr{0x21, "limit", "\x00\x00\x00\x64"})
	r, err := roundTrip(ctx, tr, host, port, encodeRequest(0x000A, printerURI(host, port), attrs))
	if err != nil {
		return nil, err
	}
	var out []CompletedJob
	for _, g := range r.Groups {
		if g.Tag != 0x02 {
			continue
		}
		id, err := strconv.ParseInt(first(g.Attrs["job-id"]), 10, 64)
		if err != nil {
			continue
		}
		imp := int64(-1)
		if v, err := strconv.ParseInt(first(g.Attrs["job-impressions-completed"]), 10, 64); err == nil && v >= 0 {
			imp = v
		}
		out = append(out, CompletedJob{ID: id, Impressions: imp})
	}
	return out, nil
}

func first(v []string) string {
	if len(v) > 0 {
		return v[0]
	}
	return ""
}
