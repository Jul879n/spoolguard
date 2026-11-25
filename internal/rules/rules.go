// Package rules replica core/src/rules.rs: mismas reglas, mismos motivos.
// Los vectores de core/tests/vectors/rules.json validan que no diverjan.
package rules

import (
	"net/netip"
	"strings"
	"time"
)

const (
	Block = "block"
	Allow = "allow"
)

// Job son los datos de un trabajo; los desconocidos son nil.
type Job struct {
	Document string  `json:"document"`
	App      *string `json:"app"`
	User     *string `json:"user"`
	OriginIP *string `json:"origin_ip"`
	// OriginHost es el nombre del equipo de origen cuando solo se conoce el nombre (Windows no informa la IP).
	// Lo reporta el cliente: sirve para reglas por equipo, no como prueba de identidad.
	OriginHost *string `json:"origin_host"`
	Site     *string `json:"site"`
}

// Rules es PrinterRules de Rust (mismo JSON). Mode nil equivale a "block".
type Rules struct {
	Enabled bool     `json:"enabled"`
	Mode    *string  `json:"mode"`
	Apps    []string `json:"apps"`
	Sites   []string `json:"sites"`
	IPs     []string `json:"ips"`
	// Paused: regla (patrón) → instante Unix hasta el que no aplica; 0 = hasta reanudarla a mano.
	// Pasado el instante la regla vuelve a aplicar sola.
	Paused map[string]int64 `json:"paused,omitempty"`
}

// IsPaused indica si el patrón está suspendido ahora.
func (r *Rules) IsPaused(pat string) bool {
	t, ok := r.Paused[pat]
	return ok && (t == 0 || t > time.Now().Unix())
}

// Normalize garantiza listas no nulas (la interfaz hace r[k].map) y modo válido.
func (r *Rules) Normalize() {
	if r.Apps == nil {
		r.Apps = []string{}
	}
	if r.Sites == nil {
		r.Sites = []string{}
	}
	if r.IPs == nil {
		r.IPs = []string{}
	}
	if r.Mode != nil && *r.Mode != Block && *r.Mode != Allow {
		r.Mode = nil
	}
}

func (r *Rules) mode() string {
	if r.Mode != nil && *r.Mode == Allow {
		return Allow
	}
	return Block
}

func str(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

var seps = " -()[]_,;"

func extMatches(t, dot string) bool {
	if strings.HasSuffix(t, dot) {
		return true
	}
	for from := 0; ; {
		i := strings.Index(t[from:], dot)
		if i < 0 {
			return false
		}
		end := from + i + len(dot)
		if end < len(t) && strings.ContainsRune(seps, rune(t[end])) {
			return true
		}
		from += i + 1
	}
}

func appMatches(pat string, j *Job) bool {
	pat = strings.ToLower(strings.TrimSpace(pat))
	if pat == "" {
		return false
	}
	doc := strings.ToLower(j.Document)
	app := strings.ToLower(str(j.App))
	if ext, ok := strings.CutPrefix(pat, "*."); ok {
		dot := "." + ext
		return extMatches(doc, dot) || extMatches(app, dot)
	}
	return strings.Contains(doc, pat) || strings.Contains(app, pat)
}

func hostOf(site string) string {
	s := strings.ToLower(strings.TrimSpace(site))
	if i := strings.LastIndex(s, "://"); i >= 0 {
		s = s[i+3:]
	}
	if i := strings.IndexAny(s, "/?#"); i >= 0 {
		s = s[:i]
	}
	if i := strings.IndexByte(s, ':'); i >= 0 {
		s = s[:i]
	}
	return s
}

// HostOf es el dominio de un patrón o sitio (sin esquema, ruta ni puerto, en minúsculas).
func HostOf(s string) string { return hostOf(s) }

func siteMatches(pat string, j *Job) bool {
	if j.Site == nil {
		return false
	}
	p, h := hostOf(pat), hostOf(*j.Site)
	return p != "" && (h == p || strings.HasSuffix(h, "."+p))
}

// ParseIP acepta IPv4/IPv6; ::ffff:a.b.c.d se trata como IPv4.
func ParseIP(s string) (netip.Addr, bool) {
	s = strings.Trim(strings.TrimSpace(s), "[]")
	if i := strings.IndexByte(s, '%'); i >= 0 {
		s = s[:i]
	}
	a, err := netip.ParseAddr(s)
	if err != nil {
		return netip.Addr{}, false
	}
	return a.Unmap(), true
}

func prefixEqual(a, b []byte, n int) bool {
	for i := 0; i < len(a) && n > 0; i++ {
		bits := min(n, 8)
		mask := byte(0xff << (8 - bits))
		if a[i]&mask != b[i]&mask {
			return false
		}
		n -= bits
	}
	return true
}

// hostKey normaliza un nombre de equipo para comparar (sin `\\` iniciales ni espacios, en minúsculas).
func hostKey(s string) string {
	return strings.ToLower(strings.TrimSpace(strings.TrimLeft(strings.TrimSpace(s), `\`)))
}

// hostMatches: el patrón es un nombre (no una IP ni un CIDR) y coincide con el equipo de origen.
func hostMatches(pat string, j *Job) bool {
	p := hostKey(pat)
	if p == "" || strings.Contains(p, "/") {
		return false
	}
	if _, ok := ParseIP(p); ok {
		return false
	}
	if j.OriginHost != nil && hostKey(*j.OriginHost) == p {
		return true
	}
	// Un origin_ip que no es una IP es un nombre de equipo (así lo informaban las versiones anteriores).
	if j.OriginIP != nil {
		if _, ok := ParseIP(*j.OriginIP); !ok && hostKey(*j.OriginIP) == p {
			return true
		}
	}
	return false
}

func ipMatches(pat string, j *Job) bool {
	if j.OriginIP != nil {
		if ip, ok := ParseIP(*j.OriginIP); ok && cidrMatches(pat, ip) {
			return true
		}
	}
	return hostMatches(pat, j)
}

func cidrMatches(pat string, ip netip.Addr) bool {
	if base, n, found := strings.Cut(strings.TrimSpace(pat), "/"); found {
		b, ok := ParseIP(base)
		bits, ok2 := parseUint(n)
		if !ok || !ok2 || ip.Is4() != b.Is4() {
			return false
		}
		width := 128
		if ip.Is4() {
			width = 32
		}
		if bits > width {
			return false
		}
		x, y := ip.AsSlice(), b.AsSlice()
		return prefixEqual(x, y, bits)
	}
	p, ok := ParseIP(pat)
	return ok && p == ip
}

func parseUint(s string) (int, bool) {
	if s == "" || len(s) > 4 {
		return 0, false
	}
	n := 0
	for _, c := range s {
		if c < '0' || c > '9' {
			return 0, false
		}
		n = n*10 + int(c-'0')
	}
	return n, true
}

// Matched devuelve la primera regla que coincide ("Aplicación: x", "Sitio: x", "IP: x").
func (r *Rules) Matched(j *Job) (string, bool) {
	for _, p := range r.Apps {
		if !r.IsPaused(p) && appMatches(p, j) {
			return "Aplicación: " + p, true
		}
	}
	for _, p := range r.Sites {
		if !r.IsPaused(p) && siteMatches(p, j) {
			return "Sitio: " + p, true
		}
	}
	for _, p := range r.IPs {
		if !r.IsPaused(p) && ipMatches(p, j) {
			return "IP: " + p, true
		}
	}
	return "", false
}

// Decide devuelve true si se permite.
func (r *Rules) Decide(j *Job) bool {
	if !r.Enabled {
		return true
	}
	_, m := r.Matched(j)
	if r.mode() == Block {
		return !m
	}
	return m
}

// Explain devuelve decisión (true = permitir) y motivo para el registro.
func (r *Rules) Explain(j *Job) (bool, string) {
	d := r.Decide(j)
	m, ok := r.Matched(j)
	switch {
	case !r.Enabled:
		return d, "Control desactivado"
	case ok:
		return d, m
	case r.mode() == Block:
		return d, "Sin coincidencia"
	default:
		return d, "No está en la lista permitida"
	}
}

// ExplainBoth combina la regla global con la de la impresora: el bloqueo gana siempre.
func ExplainBoth(global, specific *Rules, j *Job) (bool, string) {
	if !global.Enabled {
		return specific.Explain(j)
	}
	gd, gr := global.Explain(j)
	if !specific.Enabled {
		return gd, "Global · " + gr
	}
	sd, sr := specific.Explain(j)
	switch {
	case !gd:
		return false, "Global · " + gr
	case !sd:
		return false, sr
	default:
		return true, sr
	}
}
