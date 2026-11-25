package app

// Copia de seguridad .spoolguard.json: formato, validación y plan de importación.
// Lógica pura (sin disco ni red), idéntica a core/src/backup.rs; los vectores
// core/tests/vectors/backup.json validan que ambas implementaciones no diverjan.
//
// Lo que se exporta sale SIEMPRE de un Snapshot (lista blanca de campos): la contraseña, su hash y
// sal, los tokens y la sesión no existen en ese tipo, así que no pueden filtrarse.

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"reflect"
	"slices"
	"sort"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"controlimpresion/internal/rules"
)

const (
	backupFormat    = "spoolguard-backup"
	backupVersion   = 1
	backupMaxBytes  = 10 << 20
	backupMaxDepth  = 32
	backupMaxEvents = 50_000
	backupDefEvents = 10_000
	bkMaxPrinters   = 500
	bkMaxList       = 2000
	bkMaxBlocks     = 1000
	bkMaxNotes      = 50
)

var backupCats = []string{"reglas", "parametros", "bloqueo_rapido", "registro", "inventario"}

// Params son los ajustes que viajan en la copia (el resto son del equipo).
type Params struct {
	UnknownPolicy string `json:"unknown_policy"`
	IntervalSecs  uint64 `json:"interval_secs"`
	NotifyBlock   bool   `json:"notify_block"`
	NotifyAllow   bool   `json:"notify_allow"`
	Theme         string `json:"theme"`
	RetentionDays uint64 `json:"retention_days"`
}

func defaultParams() Params {
	return Params{UnknownPolicy: "allow", IntervalSecs: 1, NotifyBlock: true, Theme: "system", RetentionDays: 90}
}

type partialParams struct {
	UnknownPolicy *string
	IntervalSecs  *uint64
	NotifyBlock   *bool
	NotifyAllow   *bool
	Theme         *string
	RetentionDays *uint64
}

// Snapshot es el estado exportable/importable del equipo.
type Snapshot struct {
	Printers map[string]rules.Rules `json:"printers"`
	Global   rules.Rules            `json:"global"`
	Params   Params                 `json:"params"`
	Quick    Quick                  `json:"quick"`
}

func (s Snapshot) clone() Snapshot {
	n := Snapshot{Printers: map[string]rules.Rules{}, Global: cloneRules(s.Global), Params: s.Params, Quick: cloneQuick(s.Quick)}
	for k, r := range s.Printers {
		n.Printers[k] = cloneRules(r)
	}
	return n
}

func cloneRules(r rules.Rules) rules.Rules {
	r.Apps, r.Sites, r.IPs = slices.Clone(r.Apps), slices.Clone(r.Sites), slices.Clone(r.IPs)
	if r.Mode != nil {
		m := *r.Mode
		r.Mode = &m
	}
	r.Normalize()
	return r
}

func cloneQuick(q Quick) Quick {
	n := Quick{Blocks: []QuickBlock{}, Paused: map[string]QuickMark{}}
	for _, b := range q.Blocks {
		b.IPs = slices.Clone(b.IPs)
		n.Blocks = append(n.Blocks, b)
	}
	for k, v := range q.Paused {
		n.Paused[k] = v
	}
	return n
}

// JSON del snapshot con la forma que usan los vectores (clave "paused_printers" como el archivo).
func (s Snapshot) value() map[string]any {
	return map[string]any{"printers": s.Printers, "global": s.Global, "params": s.Params,
		"quick": map[string]any{"blocks": s.Quick.Blocks, "paused_printers": s.Quick.Paused}}
}

// InvItem es una impresora del inventario.
type InvItem struct {
	Name string `json:"name"`
	Kind string `json:"kind"`
	URI  string `json:"uri"`
	// Detalle opcional (solo se exporta si se conoce; al importar el inventario es informativo y se ignora).
	Driver        string `json:"driver,omitempty"`
	ConnType      string `json:"conn_type,omitempty"`
	Host          string `json:"host,omitempty"`
	Shared        *bool  `json:"shared,omitempty"`
	Status        string `json:"status,omitempty"`
	HoldSupported *bool  `json:"hold_supported,omitempty"`
	Controlled    *bool  `json:"controlled,omitempty"`
}

type BackupMeta struct {
	Created    int64
	Host       string
	AppVersion string
}

type op int

const (
	opOmitir op = iota
	opCombinar
	opReemplazar
)

func (o op) String() string { return [...]string{"omitir", "combinar", "reemplazar"}[o] }

type Modes struct{ Reglas, Parametros, BloqueoRapido, Registro op }

func opOf(s string) (op, error) {
	switch s {
	case "combinar":
		return opCombinar, nil
	case "reemplazar":
		return opReemplazar, nil
	case "omitir":
		return opOmitir, nil
	}
	return 0, fmt.Errorf("Modo desconocido: %s", clipRunes(s, 40))
}

func allModes(o op) Modes { return Modes{o, o, o, o} }

// parseModes: "combinar"/"reemplazar" para todo, u objeto {categoria: modo} (lo que falte se omite).
func parseModes(v any) (Modes, error) {
	switch x := v.(type) {
	case nil:
		return allModes(opCombinar), nil
	case string:
		o, err := opOf(x)
		return allModes(o), err
	case map[string]any:
		var out [4]op
		for i, k := range []string{"reglas", "parametros", "bloqueo_rapido", "registro"} {
			switch s := x[k].(type) {
			case nil:
			case string:
				o, err := opOf(s)
				if err != nil {
					return Modes{}, err
				}
				out[i] = o
			default:
				return Modes{}, fmt.Errorf("Modo inválido")
			}
		}
		return Modes{out[0], out[1], out[2], out[3]}, nil
	}
	return Modes{}, fmt.Errorf("Modo inválido")
}

func clipRunes(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	return string([]rune(s)[:n])
}

type notes struct{ errors, warnings []string }

func (n *notes) err(s string) {
	if len(n.errors) < bkMaxNotes {
		n.errors = append(n.errors, s)
	}
}
func (n *notes) warn(s string) {
	if len(n.warnings) < bkMaxNotes {
		n.warnings = append(n.warnings, s)
	}
}

func sortedKeys[V any](m map[string]V) []string {
	ks := make([]string, 0, len(m))
	for k := range m {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	return ks
}

// unknownKey: las claves sensibles se nombran como tales; nunca se aplican.
func (n *notes) unknownKey(path, k string) {
	kk := clipRunes(k, 60)
	l := strings.ToLower(k)
	switch {
	case l == "__proto__" || l == "constructor" || l == "prototype":
		n.warn(path + ": clave peligrosa ignorada: " + kk)
	case slices.ContainsFunc([]string{"password", "passwd", "hash", "salt", "token", "secret", "session", "cookie", "credential", "apikey", "api_key"}, func(w string) bool { return strings.Contains(l, w) }):
		n.warn(path + ": campo sensible ignorado por seguridad: " + kk)
	default:
		n.warn(path + ": campo desconocido ignorado: " + kk)
	}
}

func (n *notes) unknown(path string, m map[string]any, known ...string) {
	for _, k := range sortedKeys(m) {
		if !slices.Contains(known, k) {
			n.unknownKey(path, k)
		}
	}
}

// bkDepthOK: profundidad máxima de anidamiento (ignora el contenido de las cadenas).
func bkDepthOK(b []byte, max int) bool {
	d, inS, esc := 0, false, false
	for _, c := range b {
		if inS {
			if esc {
				esc = false
			} else if c == '\\' {
				esc = true
			} else if c == '"' {
				inS = false
			}
			continue
		}
		switch c {
		case '"':
			inS = true
		case '[', '{':
			d++
			if d > max {
				return false
			}
		case ']', '}':
			if d > 0 {
				d--
			}
		}
	}
	return true
}

func badText(s string, max int) bool {
	return utf8.RuneCountInString(s) > max || strings.IndexFunc(s, unicode.IsControl) >= 0
}

func hostChar(c rune) bool {
	return c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_' || c == '.' ||
		(c >= 128 && !unicode.IsSpace(c) && !unicode.IsControl(c))
}

func checkApp(p string) string {
	t := strings.TrimSpace(p)
	if t == "" {
		return "patrón vacío"
	}
	if badText(p, 255) {
		return "demasiado largo o con caracteres de control"
	}
	if ext, ok := strings.CutPrefix(t, "*."); ok {
		if ext == "" || strings.IndexFunc(ext, func(c rune) bool {
			return !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_')
		}) >= 0 {
			return "extensión inválida"
		}
	}
	return ""
}

func checkSite(p string) string {
	if badText(p, 253) {
		return "demasiado largo o con caracteres de control"
	}
	h := rules.HostOf(p)
	if h == "" || strings.IndexFunc(h, func(c rune) bool { return !hostChar(c) }) >= 0 {
		return "dominio inválido"
	}
	return ""
}

func checkIP(p string) string {
	t := strings.TrimSpace(p)
	if badText(p, 64) {
		return "IP o CIDR inválido"
	}
	if base, n, ok := strings.Cut(t, "/"); ok {
		ip, ok := rules.ParseIP(base)
		if !ok {
			return "IP o CIDR inválido"
		}
		width := 128
		if ip.Is4() {
			width = 32
		}
		if n == "" || len(n) > 3 || strings.Trim(n, "0123456789") != "" {
			return "prefijo CIDR inválido"
		}
		if v, _ := strconv.Atoi(n); v > width {
			return "prefijo CIDR inválido"
		}
		return ""
	}
	if _, ok := rules.ParseIP(t); !ok {
		return "IP o CIDR inválido"
	}
	return ""
}

func checkName(n string) string {
	if strings.TrimSpace(n) == "" || badText(n, 255) {
		return "nombre de impresora inválido"
	}
	if strings.HasPrefix(n, "-") || n == "__proto__" || n == "constructor" || n == "prototype" {
		return "nombre de impresora no permitido"
	}
	return ""
}

func asUint(v any) (uint64, bool) {
	n, ok := v.(json.Number)
	if !ok {
		return 0, false
	}
	u, err := strconv.ParseUint(string(n), 10, 64)
	return u, err == nil
}

func asInt(v any) (int64, bool) {
	n, ok := v.(json.Number)
	if !ok {
		return 0, false
	}
	u, err := strconv.ParseInt(string(n), 10, 64)
	return u, err == nil
}

func rulesFrom(v any, path string, n *notes) (rules.Rules, bool) {
	var r rules.Rules
	m, ok := v.(map[string]any)
	if v == nil {
		r.Normalize()
		return r, true
	}
	if !ok {
		n.err(path + ": las reglas deben ser un objeto")
		return r, false
	}
	n.unknown(path, m, "enabled", "mode", "apps", "sites", "ips")
	good := true
	switch e := m["enabled"].(type) {
	case nil:
	case bool:
		r.Enabled = e
	default:
		n.err(path + ".enabled: debe ser verdadero o falso")
		good = false
	}
	switch mo := m["mode"].(type) {
	case nil:
	case string:
		if mo == rules.Block || mo == rules.Allow {
			r.Mode = &mo
		} else {
			n.err(path + ".mode: debe ser «block» o «allow»")
			good = false
		}
	default:
		n.err(path + ".mode: debe ser «block» o «allow»")
		good = false
	}
	checks := []struct {
		k     string
		check func(string) string
		dst   *[]string
	}{{"apps", checkApp, &r.Apps}, {"sites", checkSite, &r.Sites}, {"ips", checkIP, &r.IPs}}
	for _, c := range checks {
		out := []string{}
		switch a := m[c.k].(type) {
		case nil:
		case []any:
			if len(a) > bkMaxList {
				n.err(fmt.Sprintf("%s.%s: demasiados elementos (máx. %d)", path, c.k, bkMaxList))
				good = false
				break
			}
			for i, it := range a {
				s, isStr := it.(string)
				if !isStr {
					n.err(fmt.Sprintf("%s.%s[%d]: debe ser texto", path, c.k, i))
					good = false
				} else if why := c.check(s); why != "" {
					n.err(fmt.Sprintf("%s.%s[%d] «%s»: %s", path, c.k, i, clipRunes(s, 40), why))
					good = false
				} else {
					out = append(out, s)
				}
			}
		default:
			n.err(path + "." + c.k + ": debe ser una lista de textos")
			good = false
		}
		*c.dst = out
	}
	r.Normalize()
	return r, good
}

func paramsFrom(m map[string]any, n *notes) partialParams {
	n.unknown("parametros", m, "unknown_policy", "interval_secs", "notify_block", "notify_allow", "theme", "retention_days")
	var p partialParams
	if v := m["unknown_policy"]; v != nil {
		if s, ok := v.(string); ok && (s == "allow" || s == "block") {
			p.UnknownPolicy = &s
		} else {
			n.err("parametros.unknown_policy: debe ser «allow» o «block»")
		}
	}
	if v := m["theme"]; v != nil {
		if s, ok := v.(string); ok && (s == "light" || s == "dark" || s == "system") {
			p.Theme = &s
		} else {
			n.err("parametros.theme: debe ser «light», «dark» o «system»")
		}
	}
	for _, c := range []struct {
		k      string
		lo, hi uint64
		dst    **uint64
	}{{"interval_secs", 1, 60, &p.IntervalSecs}, {"retention_days", 1, 3650, &p.RetentionDays}} {
		if v := m[c.k]; v != nil {
			if x, ok := asUint(v); ok && x >= c.lo && x <= c.hi {
				*c.dst = &x
			} else {
				n.err(fmt.Sprintf("parametros.%s: debe ser un entero entre %d y %d", c.k, c.lo, c.hi))
			}
		}
	}
	for _, c := range []struct {
		k   string
		dst **bool
	}{{"notify_block", &p.NotifyBlock}, {"notify_allow", &p.NotifyAllow}} {
		if v := m[c.k]; v != nil {
			if b, ok := v.(bool); ok {
				*c.dst = &b
			} else {
				n.err("parametros." + c.k + ": debe ser verdadero o falso")
			}
		}
	}
	return p
}

func quickFrom(m map[string]any, n *notes) Quick {
	n.unknown("bloqueo_rapido", m, "blocks", "paused_printers", "version")
	q := Quick{Blocks: []QuickBlock{}, Paused: map[string]QuickMark{}}
	switch a := m["blocks"].(type) {
	case nil:
	case []any:
		if len(a) > bkMaxBlocks {
			n.err("bloqueo_rapido.blocks: lista inválida o demasiado grande")
			break
		}
		for i, b := range a {
			p := fmt.Sprintf("bloqueo_rapido.blocks[%d]", i)
			o, ok := b.(map[string]any)
			if !ok {
				n.err(p + ": debe ser un objeto")
				continue
			}
			host, _ := o["host"].(string)
			by, _ := o["by"].(string)
			t, tOK := asUint(o["time"])
			raw, ipsOK := o["ips"].([]any)
			ips := []string{}
			for _, x := range raw {
				s, ok := x.(string)
				if !ok {
					ipsOK = false
					break
				}
				ips = append(ips, s)
			}
			if host == "" || badText(host, 253) || strings.IndexFunc(host, unicode.IsSpace) >= 0 || badText(by, 255) || !tOK || !ipsOK {
				n.err(p + ": equipo, IPs, autor u hora inválidos")
				continue
			}
			if len(ips) > 64 || slices.ContainsFunc(ips, func(x string) bool { return checkIP(x) != "" }) {
				n.err(p + ": IP o CIDR inválido")
				continue
			}
			q.Blocks = append(q.Blocks, QuickBlock{Host: host, IPs: ips, By: by, Time: t})
		}
	default:
		n.err("bloqueo_rapido.blocks: lista inválida o demasiado grande")
	}
	switch o := m["paused_printers"].(type) {
	case nil:
	case map[string]any:
		if len(o) > bkMaxPrinters {
			n.err("bloqueo_rapido.paused_printers: debe ser un objeto de tamaño razonable")
			break
		}
		for _, k := range sortedKeys(o) {
			v, _ := o[k].(map[string]any)
			by, _ := v["by"].(string)
			t, tOK := asUint(v["time"])
			if checkName(k) != "" || badText(by, 255) || !tOK {
				n.err("bloqueo_rapido.paused_printers «" + clipRunes(k, 40) + "»: inválido")
				continue
			}
			q.Paused[k] = QuickMark{By: by, Time: t}
		}
	default:
		n.err("bloqueo_rapido.paused_printers: debe ser un objeto de tamaño razonable")
	}
	return q
}

func okEventID(s string) bool {
	if len(s) < 8 || len(s) > 64 {
		return false
	}
	for _, c := range s {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_') {
			return false
		}
	}
	return true
}

func eventFrom(v any, now int64) (Event, error) {
	var e Event
	m, ok := v.(map[string]any)
	if !ok {
		return e, fmt.Errorf("no es un objeto")
	}
	text := func(k string, max int) (string, error) {
		switch s := m[k].(type) {
		case nil:
			return "", nil
		case string:
			if utf8.RuneCountInString(s) <= max && !strings.ContainsRune(s, 0) {
				return s, nil
			}
		}
		return "", fmt.Errorf("%s inválido", k)
	}
	num := func(k string) (*int64, error) {
		if m[k] == nil {
			return nil, nil
		}
		if u, ok := asUint(m[k]); ok && u <= 1<<50 {
			x := int64(u)
			return &x, nil
		}
		return nil, fmt.Errorf("%s fuera de rango", k)
	}
	var err error
	str := func(dst *string, k string, max int) {
		if err == nil {
			*dst, err = text(k, max)
		}
	}
	str(&e.ID, "id", 64)
	if err == nil && !okEventID(e.ID) {
		err = fmt.Errorf("id inválido")
	}
	ts, tsOK := asInt(m["ts"])
	if err == nil && (!tsOK || ts < 1_000_000_000 || ts > now+86400) {
		err = fmt.Errorf("hora fuera de rango")
	}
	e.TS = ts
	str(&e.Decision, "decision", 16)
	if err == nil && e.Decision != "allow" && e.Decision != "block" && e.Decision != "notice" {
		err = fmt.Errorf("decisión inválida")
	}
	for _, f := range []struct {
		dst *string
		k   string
		max int
	}{{&e.Host, "host", 255}, {&e.IP, "ip", 255}, {&e.User, "user", 255}, {&e.Printer, "printer", 255}, {&e.Port, "port", 255},
		{&e.ConnType, "conn_type", 255}, {&e.Document, "document", 1000}, {&e.Site, "site", 255}, {&e.App, "app", 255},
		{&e.Reason, "reason", 500}, {&e.Status, "status", 255}, {&e.Source, "source", 255}} {
		str(f.dst, f.k, f.max)
	}
	if err == nil {
		e.Pages, err = num("pages")
	}
	if err == nil {
		e.Bytes, err = num("bytes")
	}
	return e, err
}

// Parsed es el contenido validado de un archivo de copia. Nada de esto se ha aplicado todavía.
type Parsed struct {
	Legacy           bool
	Version          int64
	Created          *int64
	Host, AppVersion string
	Global           *rules.Rules
	Printers         map[string]rules.Rules
	Params           *partialParams
	Quick            *Quick
	Events           []Event
	HasEvents        bool
	EventsDiscarded  int
	EventsDupInFile  int
	Inventory        []InvItem
	HasInventory     bool
	Errors, Warnings []string
}

func (p *Parsed) has(cat string) bool {
	switch cat {
	case "reglas":
		return p.Global != nil || p.Printers != nil
	case "parametros":
		return p.Params != nil
	case "bloqueo_rapido":
		return p.Quick != nil
	case "registro":
		return p.HasEvents
	case "inventario":
		return p.HasInventory
	}
	return false
}

func (p *Parsed) categories() []string {
	out := []string{}
	for _, c := range backupCats {
		if p.has(c) {
			out = append(out, c)
		}
	}
	return out
}

// sanitizeURI quita usuario/contraseña y consulta de un URI de impresora antes de exportarlo.
func sanitizeURI(u string) string {
	s := u
	if scheme, rest, ok := strings.Cut(u, "://"); ok {
		end := strings.IndexAny(rest, "/?#")
		if end < 0 {
			end = len(rest)
		}
		auth, tail := rest[:end], rest[end:]
		if i := strings.LastIndex(auth, "@"); i >= 0 {
			auth = auth[i+1:]
		}
		s = scheme + "://" + auth + tail
	}
	if i := strings.Index(s, "?"); i >= 0 {
		s = s[:i]
	}
	return clipRunes(s, 500)
}

func printersFrom(o map[string]any, n *notes) map[string]rules.Rules {
	if len(o) > bkMaxPrinters {
		n.err(fmt.Sprintf("reglas.printers: demasiadas impresoras (máx. %d)", bkMaxPrinters))
		return nil
	}
	out := map[string]rules.Rules{}
	for _, k := range sortedKeys(o) {
		if why := checkName(k); why != "" {
			n.err("impresora «" + clipRunes(k, 40) + "»: " + why)
			continue
		}
		if r, ok := rulesFrom(o[k], "reglas.printers["+clipRunes(k, 40)+"]", n); ok {
			out[k] = r
		}
	}
	return out
}

// parseBackup valida el archivo completo. Los fallos graves (tamaño, JSON, formato, versión) devuelven
// error; el resto se acumula en Errors (bloquean aplicar) y Warnings (solo informan).
func parseBackup(raw []byte, now int64) (*Parsed, error) {
	if len(raw) > backupMaxBytes {
		return nil, fmt.Errorf("El archivo es demasiado grande (máx. 10 MiB)")
	}
	raw = bytes.TrimPrefix(raw, []byte{0xEF, 0xBB, 0xBF})
	if !bkDepthOK(raw, backupMaxDepth) {
		return nil, fmt.Errorf("El archivo está demasiado anidado")
	}
	var v any
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	if !utf8.Valid(raw) || dec.Decode(&v) != nil || dec.Decode(&struct{}{}) != io.EOF {
		return nil, fmt.Errorf("El archivo no es un JSON válido")
	}
	top, ok := v.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("El archivo no es una copia de SpoolGuard (se esperaba un objeto)")
	}
	n := &notes{}
	p := &Parsed{}
	_, hasFormat := top["format"]
	legacy := !hasFormat && slices.ContainsFunc([]string{"printers", "global", "settings"}, func(k string) bool { _, ok := top[k]; return ok })
	if legacy {
		// Archivo de reglas de versiones anteriores (config.json completo): se toman reglas y ajustes permitidos.
		p.Legacy = true
		n.unknown("archivo", top, "printers", "global", "settings")
		p.Warnings = append(p.Warnings, "Formato antiguo de reglas: se importan reglas y ajustes; no se incluyen contraseña ni tokens")
		if g := top["global"]; g != nil {
			if r, ok := rulesFrom(g, "reglas.global", n); ok {
				p.Global = &r
			}
		}
		switch o := top["printers"].(type) {
		case nil:
		case map[string]any:
			p.Printers = printersFrom(o, n)
		default:
			n.err("printers: debe ser un objeto")
		}
		switch o := top["settings"].(type) {
		case nil:
		case map[string]any:
			// Solo los ajustes de la lista blanca; el resto (envío al servidor, pausa, contraseña) se ignora.
			allowed := map[string]any{}
			for _, k := range sortedKeys(o) {
				switch k {
				case "unknown_policy", "interval_secs", "notify_block", "notify_allow", "theme", "retention_days":
					allowed[k] = o[k]
				case "paused", "send_events", "server_url":
				default:
					n.unknownKey("settings", k)
				}
			}
			pp := paramsFrom(allowed, n)
			p.Params = &pp
		default:
			n.err("settings: debe ser un objeto")
		}
	} else {
		if f, _ := top["format"].(string); f != backupFormat {
			return nil, fmt.Errorf("El archivo no es una copia de SpoolGuard (formato desconocido)")
		}
		ver, vOK := asInt(top["version"])
		switch {
		case vOK && ver > backupVersion:
			return nil, fmt.Errorf("Versión de formato no soportada (%d): este programa admite hasta la %d; actualiza SpoolGuard", ver, backupVersion)
		case vOK && ver >= 1:
			p.Version = ver
		default:
			return nil, fmt.Errorf("Versión de formato inválida")
		}
		n.unknown("archivo", top, "format", "version", "created", "host", "app_version", "categories", "reglas", "parametros", "bloqueo_rapido", "registro", "inventario")
		if c, ok := asInt(top["created"]); ok {
			p.Created = &c
		}
		if s, ok := top["host"].(string); ok {
			p.Host = clipRunes(s, 255)
		}
		if s, ok := top["app_version"].(string); ok {
			p.AppVersion = clipRunes(s, 64)
		}
		switch a := top["categories"].(type) {
		case nil:
		case []any:
			for _, x := range a {
				if s, ok := x.(string); !ok || !slices.Contains(backupCats, s) {
					n.warn("categories: categoría desconocida ignorada")
					break
				}
			}
		default:
			n.err("categories: debe ser una lista")
		}
		switch m := top["reglas"].(type) {
		case nil:
		case map[string]any:
			n.unknown("reglas", m, "global", "printers")
			if g := m["global"]; g != nil {
				if r, ok := rulesFrom(g, "reglas.global", n); ok {
					p.Global = &r
				}
			}
			switch o := m["printers"].(type) {
			case nil:
			case map[string]any:
				p.Printers = printersFrom(o, n)
			default:
				n.err("reglas.printers: debe ser un objeto")
			}
		default:
			n.err("reglas: debe ser un objeto")
		}
		switch m := top["parametros"].(type) {
		case nil:
		case map[string]any:
			pp := paramsFrom(m, n)
			p.Params = &pp
		default:
			n.err("parametros: debe ser un objeto")
		}
		switch m := top["bloqueo_rapido"].(type) {
		case nil:
		case map[string]any:
			q := quickFrom(m, n)
			p.Quick = &q
		default:
			n.err("bloqueo_rapido: debe ser un objeto")
		}
		switch a := top["registro"].(type) {
		case nil:
		case []any:
			if len(a) > backupMaxEvents {
				n.err(fmt.Sprintf("registro: demasiados eventos (máx. %d)", backupMaxEvents))
				break
			}
			seen := map[string]bool{}
			firstBad := ""
			p.Events = make([]Event, 0, len(a))
			for _, x := range a {
				e, err := eventFrom(x, now)
				switch {
				case err != nil:
					p.EventsDiscarded++
					if firstBad == "" {
						firstBad = err.Error()
					}
				case seen[e.ID]:
					p.EventsDupInFile++
				default:
					seen[e.ID] = true
					p.Events = append(p.Events, e)
				}
			}
			if firstBad != "" {
				n.warn(fmt.Sprintf("registro: %d eventos descartados por inválidos (p. ej. %s)", p.EventsDiscarded, firstBad))
			}
			p.HasEvents = true
		default:
			n.err("registro: debe ser una lista")
		}
		switch a := top["inventario"].(type) {
		case nil:
		case []any:
			if len(a) > bkMaxPrinters {
				n.err("inventario: debe ser una lista de tamaño razonable")
				break
			}
			items := []InvItem{}
			for _, x := range a {
				o, _ := x.(map[string]any)
				s := func(k string) string { v, _ := o[k].(string); return clipRunes(v, 500) }
				if name := s("name"); checkName(name) == "" {
					items = append(items, InvItem{Name: name, Kind: clipRunes(s("kind"), 40), URI: sanitizeURI(s("uri"))})
				}
			}
			if len(items) != len(a) {
				n.warn("inventario: se ignoraron elementos inválidos")
			}
			p.Inventory, p.HasInventory = items, true
		default:
			n.err("inventario: debe ser una lista de tamaño razonable")
		}
	}
	p.Warnings = append(p.Warnings, n.warnings...)
	p.Errors = n.errors
	return p, nil
}

// BackupCtx es el contexto del equipo que importa.
type BackupCtx struct {
	Local       []string        // impresoras que existen en este equipo
	ExistingIDs map[string]bool // ids del archivo que ya están en el registro
	EventCount  int64           // eventos actuales (para avisar de lo que borra «reemplazar»)
	Server      bool
}

type Plan struct {
	Next          Snapshot
	Preview       map[string]any
	Events        []Event
	ReplaceEvents bool
	Applied       []string
	OK            bool
}

func union(a, b []string) []string {
	o := slices.Clone(a)
	for _, s := range b {
		if !slices.Contains(o, s) {
			o = append(o, s)
		}
	}
	return o
}

// mergeRules: listas unidas; activación y modo del archivo (el modo vacío conserva el actual).
func mergeRules(a, b rules.Rules) rules.Rules {
	r := rules.Rules{Enabled: b.Enabled, Mode: b.Mode, Apps: union(a.Apps, b.Apps), Sites: union(a.Sites, b.Sites), IPs: union(a.IPs, b.IPs)}
	if r.Mode == nil {
		r.Mode = a.Mode
	}
	return cloneRules(r)
}

func paramsMap(p Params) map[string]any {
	return map[string]any{"unknown_policy": p.UnknownPolicy, "interval_secs": p.IntervalSecs, "notify_block": p.NotifyBlock,
		"notify_allow": p.NotifyAllow, "theme": p.Theme, "retention_days": p.RetentionDays}
}

func ifNil[T any](p *T, def T) T {
	if p == nil {
		return def
	}
	return *p
}

// planImport calcula qué pasaría al importar p sobre cur, sin aplicar nada.
func planImport(cur Snapshot, p *Parsed, m Modes, ctx BackupCtx) Plan {
	next := cur.clone()
	warns := slices.Clone(p.Warnings)
	applied := []string{}
	local := map[string]bool{}
	for _, l := range ctx.Local {
		local[l] = true
	}
	on := func(o op, c string) bool { return o != opOmitir && p.has(c) }

	var reglas, parametros, quick, registro, inventario any
	if on(m.Reglas, "reglas") {
		applied = append(applied, "reglas")
		replace := m.Reglas == opReemplazar
		list := []any{}
		nuevas, modif, elim, iguales := 0, 0, 0, 0
		if p.Printers != nil {
			if replace {
				next.Printers = map[string]rules.Rules{}
				for k, r := range p.Printers {
					next.Printers[k] = cloneRules(r)
				}
			} else {
				for k, r := range p.Printers {
					if c, ok := cur.Printers[k]; ok {
						next.Printers[k] = mergeRules(c, r)
					} else {
						next.Printers[k] = cloneRules(r)
					}
				}
			}
			for _, k := range sortedKeys(p.Printers) {
				estado := "modificada"
				c, had := cur.Printers[k]
				switch {
				case !had:
					estado = "nueva"
					nuevas++
				case reflect.DeepEqual(c, next.Printers[k]):
					estado = "igual"
					iguales++
				default:
					modif++
				}
				list = append(list, map[string]any{"nombre": k, "estado": estado, "existe": local[k]})
			}
			if replace {
				for _, k := range sortedKeys(cur.Printers) {
					if _, ok := p.Printers[k]; !ok {
						elim++
						list = append(list, map[string]any{"nombre": k, "estado": "eliminada", "existe": local[k]})
					}
				}
			}
			var missing []string
			for _, k := range sortedKeys(p.Printers) {
				if !local[k] && len(missing) < 10 {
					missing = append(missing, k)
				}
			}
			if len(missing) > 0 {
				warns = append(warns, "Impresoras que no existen en este equipo ("+strings.Join(missing, ", ")+"): se importan sus reglas pero no se activa la retención de sus colas")
			}
		}
		if p.Global != nil {
			if replace {
				next.Global = cloneRules(*p.Global)
			} else {
				next.Global = mergeRules(cur.Global, *p.Global)
			}
		}
		global := "modificado"
		if reflect.DeepEqual(next.Global, cur.Global) {
			global = "igual"
		}
		reglas = map[string]any{"global": global, "impresoras": list, "nuevas": nuevas, "modificadas": modif, "eliminadas": elim, "iguales": iguales}
	}

	if on(m.Parametros, "parametros") {
		applied = append(applied, "parametros")
		pp := ifNil(p.Params, partialParams{})
		base := cur.Params
		if m.Parametros == opReemplazar {
			base = defaultParams()
		}
		next.Params = Params{
			UnknownPolicy: ifNil(pp.UnknownPolicy, base.UnknownPolicy), IntervalSecs: ifNil(pp.IntervalSecs, base.IntervalSecs),
			NotifyBlock: ifNil(pp.NotifyBlock, base.NotifyBlock), NotifyAllow: ifNil(pp.NotifyAllow, base.NotifyAllow),
			Theme: ifNil(pp.Theme, base.Theme), RetentionDays: ifNil(pp.RetentionDays, base.RetentionDays),
		}
		a, b := paramsMap(cur.Params), paramsMap(next.Params)
		cambios := []any{}
		for _, k := range sortedKeys(b) {
			if a[k] != b[k] {
				cambios = append(cambios, map[string]any{"campo": k, "de": a[k], "a": b[k]})
			}
		}
		parametros = map[string]any{"cambios": cambios}
	}

	if p.has("bloqueo_rapido") && !ctx.Server {
		warns = append(warns, "El bloqueo rápido solo existe en el servidor: se ignora en este equipo")
	} else if on(m.BloqueoRapido, "bloqueo_rapido") {
		applied = append(applied, "bloqueo_rapido")
		fq := cloneQuick(*p.Quick)
		replace := m.BloqueoRapido == opReemplazar
		if replace {
			next.Quick = fq
		} else {
			for _, b := range fq.Blocks {
				next.Quick.Blocks = slices.DeleteFunc(next.Quick.Blocks, func(x QuickBlock) bool { return x.Host == b.Host })
				next.Quick.Blocks = append(next.Quick.Blocks, b)
			}
			for k, v := range fq.Paused {
				next.Quick.Paused[k] = v
			}
		}
		hasHost := func(bs []QuickBlock, h string) bool {
			return slices.ContainsFunc(bs, func(x QuickBlock) bool { return x.Host == h })
		}
		nb, np, rb, rp := 0, 0, 0, 0
		for _, b := range fq.Blocks {
			if !hasHost(cur.Quick.Blocks, b.Host) {
				nb++
			}
		}
		for k := range fq.Paused {
			if _, ok := cur.Quick.Paused[k]; !ok {
				np++
			}
		}
		if replace {
			for _, b := range cur.Quick.Blocks {
				if !hasHost(fq.Blocks, b.Host) {
					rb++
				}
			}
			for k := range cur.Quick.Paused {
				if _, ok := fq.Paused[k]; !ok {
					rp++
				}
			}
		}
		quick = map[string]any{"bloqueos_nuevos": nb, "pausadas_nuevas": np, "bloqueos_eliminados": rb, "pausadas_eliminadas": rp}
		var gone []string
		for _, k := range sortedKeys(fq.Paused) {
			if !local[k] && len(gone) < 10 {
				gone = append(gone, k)
			}
		}
		if len(gone) > 0 {
			warns = append(warns, "Impresoras pausadas que no existen en este equipo ("+strings.Join(gone, ", ")+"): no se retienen")
		}
	}

	events := []Event{}
	replaceEvents := false
	if on(m.Registro, "registro") {
		applied = append(applied, "registro")
		replaceEvents = m.Registro == opReemplazar
		dup := p.EventsDupInFile
		for _, e := range p.Events {
			if !replaceEvents && ctx.ExistingIDs[e.ID] {
				dup++
			} else {
				events = append(events, e)
			}
		}
		borrar := int64(0)
		if replaceEvents {
			borrar = ctx.EventCount
		}
		registro = map[string]any{"a_importar": len(events), "duplicados": dup, "descartados": p.EventsDiscarded, "a_borrar": borrar}
	}

	if p.HasInventory {
		falta := 0
		for _, i := range p.Inventory {
			if !local[i.Name] {
				falta++
			}
		}
		if falta > 0 && len(ctx.Local) > 0 {
			warns = append(warns, fmt.Sprintf("Inventario: %d impresoras del archivo no existen en este equipo", falta))
		}
		inventario = map[string]any{"impresoras": len(p.Inventory)}
	}

	ok := len(p.Errors) == 0
	var created any
	if p.Created != nil {
		created = *p.Created
	}
	nz := func(s []string) []string {
		if s == nil {
			return []string{}
		}
		return s
	}
	pv := map[string]any{
		"ok": ok, "legacy": p.Legacy, "version": p.Version, "creado": created, "equipo": p.Host, "app_version": p.AppVersion,
		"categorias": p.categories(),
		"modos":      map[string]any{"reglas": m.Reglas.String(), "parametros": m.Parametros.String(), "bloqueo_rapido": m.BloqueoRapido.String(), "registro": m.Registro.String()},
		"reglas":     reglas, "parametros": parametros, "bloqueo_rapido": quick, "registro": registro, "inventario": inventario,
		"errores": nz(p.Errors), "avisos": nz(warns),
	}
	return Plan{Next: next, Preview: pv, Events: events, ReplaceEvents: replaceEvents, Applied: applied, OK: ok}
}

// exportBackup arma el archivo de copia a partir del estado y las categorías pedidas (solo lista blanca).
func exportBackup(snap Snapshot, cats []string, events []Event, inv []InvItem, meta BackupMeta) map[string]any {
	has := func(c string) bool { return slices.Contains(cats, c) }
	chosen := []string{}
	for _, c := range backupCats {
		if has(c) {
			chosen = append(chosen, c)
		}
	}
	o := map[string]any{"format": backupFormat, "version": backupVersion, "created": meta.Created, "host": meta.Host,
		"app_version": meta.AppVersion, "categories": chosen}
	if has("reglas") {
		o["reglas"] = map[string]any{"global": snap.Global, "printers": snap.Printers}
	}
	if has("parametros") {
		o["parametros"] = snap.Params
	}
	if has("bloqueo_rapido") {
		o["bloqueo_rapido"] = map[string]any{"blocks": snap.Quick.Blocks, "paused_printers": snap.Quick.Paused}
	}
	if has("registro") {
		out := make([]Event, len(events))
		for i, e := range events {
			e.Device = ""
			out[i] = e
		}
		o["registro"] = out
	}
	if has("inventario") {
		out := make([]InvItem, len(inv))
		for i, it := range inv {
			it.URI = sanitizeURI(it.URI)
			out[i] = it
		}
		o["inventario"] = out
	}
	return o
}
