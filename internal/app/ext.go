package app

import (
	"bytes"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"controlimpresion/internal/rules"
)

// Extensión de navegador en modo servidor: token propio (distinto de la contraseña de administrador),
// reglas de sitios (/ext/rules) e intenciones de impresión (/ext/intent, formato de core/src/intents.rs).

const (
	extTokensFile  = "ext_tokens.json"
	intentsFile    = "intents.jsonl"
	extTokenPrefix = "sgx_"
	devTokenPrefix = "sgm_" // token de equipo: solo puede enviar eventos (POST /api/events/ingest)
	kindExt        = "ext"
	kindDevice     = "device"
	maxExtTokens   = 50
	extBodyMax     = 4 << 10 // 4 KB
	intentMaxAge   = 120     // s: igual que MAX_AGE_SECS de escritorio
	intentAgentWin = 15      // s: igual que AGENT_WINDOW_SECS
	intentMaxLines = 200
	rulesPerMin    = 120 // /ext/rules por token (la extensión consulta cada 15 s)
	intentPerMin   = 30  // /ext/intent por token
)

type extToken struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	Hash    string `json:"hash"` // SHA-256 (hex) del token; el token solo se muestra al crearlo
	Created uint64 `json:"created"`
	Kind    string `json:"kind,omitempty"` // "" o "ext" = extensión de navegador; "device" = equipo de escritorio
}

func (t extToken) kind() string {
	if t.Kind == kindDevice {
		return kindDevice
	}
	return kindExt
}

type rateWin struct {
	start time.Time
	n     int
}

type extStore struct {
	mu     sync.Mutex
	dir    string
	tokens []extToken
	used   map[string]uint64 // id -> último uso (solo en memoria)
	rate   map[string]*rateWin
	now    func() time.Time
	fileMu sync.Mutex // intents.jsonl
}

func loadExt(dir string) *extStore {
	e := &extStore{dir: dir, used: map[string]uint64{}, rate: map[string]*rateWin{}, now: time.Now}
	if b, err := os.ReadFile(filepath.Join(dir, extTokensFile)); err == nil {
		_ = json.Unmarshal(b, &e.tokens)
	}
	return e
}

func (e *extStore) saveLocked() error {
	b, _ := json.MarshalIndent(e.tokens, "", "  ")
	return writeAtomic(filepath.Join(e.dir, extTokensFile), b)
}

func hashExtToken(t string) string {
	h := sha256.Sum256([]byte(t))
	return hex.EncodeToString(h[:])
}

// lookup devuelve el id de un token de extensión válido.
func (e *extStore) lookup(tok string) (string, bool) {
	t, ok := e.lookupKind(tok, kindExt)
	return t.ID, ok
}

// lookupKind devuelve el token válido del tipo pedido (un token de equipo no sirve para la extensión
// ni al revés). Recorre todos y compara en tiempo constante.
func (e *extStore) lookupKind(tok, kind string) (extToken, bool) {
	if tok == "" || len(tok) > 200 {
		return extToken{}, false
	}
	got := []byte(hashExtToken(tok))
	e.mu.Lock()
	defer e.mu.Unlock()
	var hit extToken
	found := false
	for _, t := range e.tokens {
		if subtle.ConstantTimeCompare(got, []byte(t.Hash)) == 1 && t.kind() == kind {
			hit, found = t, true
		}
	}
	if found {
		e.used[hit.ID] = uint64(e.now().Unix())
	}
	return hit, found
}

// allow aplica una ventana fija de 1 minuto por token y ruta.
func (e *extStore) allow(id, route string, limit int) bool {
	e.mu.Lock()
	defer e.mu.Unlock()
	k := id + "|" + route
	n := e.now()
	w := e.rate[k]
	if w == nil || n.Sub(w.start) >= time.Minute {
		w = &rateWin{start: n}
		e.rate[k] = w
	}
	w.n++
	return w.n <= limit
}

// --- comandos de administrador (requieren sesión) ---

func (s *Server) createExtToken(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Name string `json:"name"`
		Kind string `json:"kind"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	kind, prefix := kindExt, extTokenPrefix
	switch a.Kind {
	case "", kindExt:
	case kindDevice:
		kind, prefix = kindDevice, devTokenPrefix
	default:
		return nil, bad("Tipo de token desconocido")
	}
	name := strings.TrimSpace(a.Name)
	if name == "" || utf8.RuneCountInString(name) > 60 {
		return nil, bad("El nombre es obligatorio (máx. 60 caracteres)")
	}
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		panic(err)
	}
	tok := prefix + base64.RawURLEncoding.EncodeToString(b)
	e := s.ext
	e.mu.Lock()
	defer e.mu.Unlock()
	if len(e.tokens) >= maxExtTokens {
		return nil, bad("Demasiados tokens; revoca alguno")
	}
	t := extToken{ID: randHex(6), Name: name, Hash: hashExtToken(tok), Created: now()}
	if kind == kindDevice {
		t.Kind = kindDevice
	}
	e.tokens = append(e.tokens, t)
	if err := e.saveLocked(); err != nil {
		e.tokens = e.tokens[:len(e.tokens)-1]
		return nil, err
	}
	return map[string]any{"id": t.ID, "name": t.Name, "created": t.Created, "kind": kind, "token": tok}, nil
}

type extTokenView struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Created  uint64 `json:"created"`
	LastUsed uint64 `json:"last_used"`
	Kind     string `json:"kind"`
}

func (s *Server) listExtTokens(_ *http.Request, _ json.RawMessage) (any, error) {
	e := s.ext
	e.mu.Lock()
	defer e.mu.Unlock()
	out := make([]extTokenView, 0, len(e.tokens))
	for _, t := range e.tokens {
		out = append(out, extTokenView{ID: t.ID, Name: t.Name, Created: t.Created, LastUsed: e.used[t.ID], Kind: t.kind()})
	}
	return out, nil
}

func (s *Server) revokeExtToken(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		ID string `json:"id"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	e := s.ext
	e.mu.Lock()
	defer e.mu.Unlock()
	for i, t := range e.tokens {
		if t.ID == a.ID {
			old := e.tokens
			e.tokens = append(append([]extToken{}, old[:i]...), old[i+1:]...)
			if err := e.saveLocked(); err != nil {
				e.tokens = old
				return nil, err
			}
			delete(e.used, t.ID)
			return nil, nil
		}
	}
	return nil, &apiErr{http.StatusNotFound, "Token desconocido"}
}

// --- rutas públicas /ext/* (token Bearer de extensión) ---

// extOrigin acepta solo orígenes de extensiones de navegador: chrome-extension://<id> y moz-extension://<id>.
func extOrigin(o string) bool {
	for _, p := range []string{"chrome-extension://", "moz-extension://"} {
		if rest, ok := strings.CutPrefix(o, p); ok {
			if rest == "" || len(rest) > 64 {
				return false
			}
			for _, c := range rest {
				if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-') {
					return false
				}
			}
			return true
		}
	}
	return false
}

// extCORS valida el origen (si lo hay) y fija las cabeceras CORS sin cookies ni credenciales.
func extCORS(w http.ResponseWriter, r *http.Request) bool {
	o := r.Header.Get("Origin")
	if o == "" {
		return true // curl u otros clientes que no son navegadores: solo vale el token
	}
	if !extOrigin(o) {
		writeJSON(w, http.StatusForbidden, map[string]string{"error": "Origen no permitido"})
		return false
	}
	h := w.Header()
	h.Set("Access-Control-Allow-Origin", o)
	h.Add("Vary", "Origin")
	return true
}

func extBearer(r *http.Request) string {
	if h := r.Header.Get("Authorization"); strings.HasPrefix(h, "Bearer ") {
		return strings.TrimSpace(h[7:])
	}
	return ""
}

// extAuth responde el preflight y valida CORS, método, token y frecuencia; true si se puede atender.
func (s *Server) extAuth(w http.ResponseWriter, r *http.Request, method, route string, limit int) bool {
	if !extCORS(w, r) {
		return false
	}
	if r.Method == http.MethodOptions {
		h := w.Header()
		h.Set("Access-Control-Allow-Methods", method+", OPTIONS")
		h.Set("Access-Control-Allow-Headers", "Authorization, Content-Type")
		h.Set("Access-Control-Max-Age", "600")
		w.WriteHeader(http.StatusNoContent)
		return false
	}
	if r.Method != method {
		w.Header().Set("Allow", method+", OPTIONS")
		writeJSON(w, http.StatusMethodNotAllowed, map[string]string{"error": "Método no permitido"})
		return false
	}
	id, ok := s.ext.lookup(extBearer(r))
	if !ok {
		w.Header().Set("WWW-Authenticate", "Bearer")
		writeJSON(w, http.StatusUnauthorized, map[string]string{"error": "Token inválido o revocado"})
		return false
	}
	if !s.ext.allow(id, route, limit) {
		w.Header().Set("Retry-After", "60")
		writeJSON(w, http.StatusTooManyRequests, map[string]string{"error": "Demasiadas peticiones"})
		return false
	}
	return true
}

func (s *Server) extRules(w http.ResponseWriter, r *http.Request) {
	if !s.extAuth(w, r, http.MethodGet, "rules", rulesPerMin) {
		return
	}
	writeJSON(w, http.StatusOK, rulesForExt(s.config()))
}

func normSite(d string) string {
	d = strings.TrimSpace(d)
	d = strings.TrimPrefix(d, "*.")
	return strings.ToLower(strings.Trim(d, "."))
}

// activeSites excluye los sitios con la regla suspendida.
func activeSites(r *rules.Rules) []string {
	var out []string
	for _, s := range r.Sites {
		if !r.IsPaused(s) {
			out = append(out, s)
		}
	}
	return out
}

func pushUnique(out []string, sites []string) []string {
	for _, s := range sites {
		if s = normSite(s); s != "" && !contains(out, s) {
			out = append(out, s)
		}
	}
	return out
}

func contains(l []string, s string) bool {
	for _, x := range l {
		if x == s {
			return true
		}
	}
	return false
}

// rulesForExt porta rules_from_config de core/src/bin/native_host.rs: sitios de las reglas habilitadas
// de cada impresora y de la global; `allow_only` es null si ninguna regla activa está en modo permitir.
func rulesForExt(cfg Config) map[string]any {
	block, allow := []string{}, []string{}
	anyAllow := false
	names := make([]string, 0, len(cfg.Printers))
	for n := range cfg.Printers {
		names = append(names, n)
	}
	sort.Strings(names)
	all := make([]*rules.Rules, 0, len(names)+1)
	for _, n := range names {
		if r := cfg.Printers[n]; r != nil {
			all = append(all, r)
		}
	}
	g := cfg.Global
	all = append(all, &g)
	for _, r := range all {
		if !r.Enabled {
			continue
		}
		if r.Mode != nil && *r.Mode == rules.Allow {
			anyAllow = true
			allow = pushUnique(allow, activeSites(r))
		} else {
			block = pushUnique(block, activeSites(r))
		}
	}
	var ao any
	if anyAllow {
		ao = allow
	}
	return map[string]any{"block": block, "allow_only": ao}
}

type intentLine struct {
	Time  uint64 `json:"time"`
	Host  string `json:"host"`
	Title string `json:"title"`
	Kind  string `json:"kind"`
}

func (s *Server) extIntent(w http.ResponseWriter, r *http.Request) {
	if !s.extAuth(w, r, http.MethodPost, "intent", intentPerMin) {
		return
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, extBodyMax))
	if err != nil {
		writeJSON(w, http.StatusRequestEntityTooLarge, map[string]string{"error": "Petición demasiado grande"})
		return
	}
	var a struct {
		URL   string `json:"url"`
		Title string `json:"title"`
		Kind  string `json:"kind"`
	}
	if json.Unmarshal(body, &a) != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "intención inválida"})
		return
	}
	// Entrada no confiable: solo http(s) y largos acotados (igual que el host nativo).
	lu := strings.ToLower(a.URL)
	if !(strings.HasPrefix(lu, "http://") || strings.HasPrefix(lu, "https://")) || len(a.URL) > 300 || utf8.RuneCountInString(a.Title) > 300 {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "intención inválida"})
		return
	}
	kind := ""
	if a.Kind == "agent" {
		kind = "agent"
	}
	if err := recordIntent(s.ext, a.URL, a.Title, kind, now()); err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": err.Error()})
		return
	}
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// hostOfURL porta host_of_url de core/src/intents.rs.
func hostOfURL(u string) string {
	_, rest, ok := strings.Cut(u, "://")
	if !ok {
		return ""
	}
	if i := strings.IndexAny(rest, "/?#"); i >= 0 {
		rest = rest[:i]
	}
	if i := strings.LastIndex(rest, "@"); i >= 0 {
		rest = rest[i+1:]
	}
	if strings.HasPrefix(rest, "[") {
		rest = rest[1:]
		rest, _, _ = strings.Cut(rest, "]")
		return strings.ToLower(rest)
	}
	rest, _, _ = strings.Cut(rest, ":")
	return strings.ToLower(rest)
}

// recordIntent añade una línea a intents.jsonl (mismo formato que core/src/intents.rs) y recorta el archivo.
func recordIntent(e *extStore, url, title, kind string, t uint64) error {
	host := hostOfURL(url)
	if host == "" {
		return nil
	}
	e.fileMu.Lock()
	defer e.fileMu.Unlock()
	path := filepath.Join(e.dir, intentsFile)
	if b, err := os.ReadFile(path); err == nil {
		lines := strings.Split(strings.TrimRight(string(b), "\n"), "\n")
		if len(lines) > intentMaxLines {
			if err := os.WriteFile(path, []byte(strings.Join(lines[len(lines)-intentMaxLines/2:], "\n")+"\n"), 0o600); err != nil {
				return err
			}
		}
	}
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(intentLine{Time: t, Host: host, Title: title, Kind: kind})
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = f.Write(buf.Bytes())
	return err
}

// findSite porta find_site de core/src/intents.rs: dominio de la intención reciente cuyo título coincide
// con el documento; si no, el de la última orden a un agente local (QZ Tray) de los últimos segundos.
func findSite(dir, document string, t uint64) string {
	doc := strings.ToLower(document)
	if doc == "" {
		return ""
	}
	b, err := os.ReadFile(filepath.Join(dir, intentsFile))
	if err != nil {
		return ""
	}
	lines := strings.Split(string(b), "\n")
	var recent []intentLine
	for i := len(lines) - 1; i >= 0; i-- {
		var it intentLine
		if json.Unmarshal([]byte(lines[i]), &it) != nil {
			continue
		}
		if sub(t, it.Time) <= intentMaxAge {
			recent = append(recent, it)
		}
	}
	for _, it := range recent {
		ti := strings.ToLower(strings.TrimSpace(it.Title))
		if utf8.RuneCountInString(ti) >= 8 && (strings.Contains(doc, ti) || strings.Contains(ti, doc)) {
			return it.Host
		}
	}
	for _, it := range recent {
		if it.Kind == "agent" && sub(t, it.Time) <= intentAgentWin {
			return it.Host
		}
	}
	return ""
}

func sub(a, b uint64) uint64 {
	if a < b {
		return 0
	}
	return a - b
}
