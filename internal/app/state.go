// Package app: estado, worker y API HTTP del modo servidor.
// config.json y managed.json tienen el mismo formato que la app de escritorio; el registro vive en events.db (SQLite).
package app

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"controlimpresion/internal/cups"
	"controlimpresion/internal/discover"
	"controlimpresion/internal/rules"
)

type Settings struct {
	UnknownPolicy string `json:"unknown_policy"`
	IntervalSecs  uint64 `json:"interval_secs"`
	NotifyBlock   bool   `json:"notify_block"`
	NotifyAllow   bool   `json:"notify_allow"`
	Theme         string `json:"theme"`
	RetentionDays uint64 `json:"retention_days"`
	PasswordHash  string `json:"password_hash"`
	PasswordSalt  string `json:"password_salt"`
	Paused        bool   `json:"paused"`
}

func defaultSettings() Settings {
	return Settings{UnknownPolicy: "allow", IntervalSecs: 1, NotifyBlock: true, Theme: "system", RetentionDays: 90}
}

type Config struct {
	Printers map[string]*rules.Rules `json:"printers"`
	Global   rules.Rules             `json:"global"`
	Settings Settings                `json:"settings"`
}

func defaultConfig() Config {
	return Config{Printers: map[string]*rules.Rules{}, Settings: defaultSettings()}
}

func (c *Config) normalize() {
	if c.Printers == nil {
		c.Printers = map[string]*rules.Rules{}
	}
	for k, r := range c.Printers {
		if r == nil {
			r = &rules.Rules{}
			c.Printers[k] = r
		}
		r.Normalize()
	}
	c.Global.Normalize()
}

func (c Config) clone() Config {
	b, _ := json.Marshal(c)
	n := defaultConfig()
	_ = json.Unmarshal(b, &n)
	n.normalize()
	return n
}

type Activity struct {
	Time     uint64  `json:"time"`
	Printer  string  `json:"printer"`
	Document string  `json:"document"`
	User     *string `json:"user"`
	Origin   *string `json:"origin"`
	Allowed  bool    `json:"allowed"`
	Reason   string  `json:"reason"`
}

// Managed: cola retenida por la app y su estado previo (managed.json).
type Managed struct {
	PausadaAntes bool `json:"pausada_antes"`
}

// Quick guarda el bloqueo rápido (quick.json): quién y cuándo. Solo del servidor.
type Quick struct {
	Blocks  []QuickBlock         `json:"blocks"`
	Paused  map[string]QuickMark `json:"paused_printers"`
	Version int                  `json:"version"`
}
type QuickBlock struct {
	Host string   `json:"host"`
	IPs  []string `json:"ips"`
	By   string   `json:"by"`
	Time uint64   `json:"time"`
}
type QuickMark struct {
	By   string `json:"by"`
	Time uint64 `json:"time"`
}

type Server struct {
	dir       string
	mu        sync.Mutex // cfg, quick
	cfg       Config
	events    *eventStore
	retention int64 // días (CONTROL_LOG_RETENTION_DAYS); 0 = sin límite
	quick     Quick
	holdMu    sync.Mutex // managed.json y cambios de retención
	paused    atomic.Bool
	auth      *Auth
	ext       *extStore
	hub       rulesHub // avisa a /ext/events cuando cambia la configuración

	extDir   string // carpeta de la extensión a publicar; vacío = autodetectar (ver extDirs)
	extCache extPkgCache

	routes routeCache // device-uri por cola (para rellenar puerto y tipo de conexión de cada evento)
	net    netState   // descubrimiento de red y detección de impresión directa

	readFails map[uint32]int  // solo el worker: lecturas fallidas seguidas por trabajo
	released  map[uint32]bool // solo el worker: trabajos ya liberados que siguen en la cola
}

func now() uint64 { return uint64(time.Now().Unix()) }

func writeAtomic(path string, b []byte) error {
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

func hashPW(salt, pw string) string {
	h := sha256.Sum256([]byte(salt + pw))
	return hex.EncodeToString(h[:])
}

// hardenPerms deja la carpeta de datos en 0700 y sus archivos en 0600: instalaciones anteriores (o un volumen
// montado con otros permisos) podían dejar config.json, events.db o ext_tokens.json legibles por otros usuarios.
func hardenPerms(dir string) {
	_ = os.Chmod(dir, 0o700)
	ents, err := os.ReadDir(dir)
	if err != nil {
		return
	}
	for _, e := range ents {
		if e.Type().IsRegular() {
			_ = os.Chmod(filepath.Join(dir, e.Name()), 0o600)
		}
	}
}

func randHex(n int) string {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		panic(err)
	}
	return hex.EncodeToString(b)
}

// loadConfig: si está corrupta usa el respaldo y conserva el archivo dañado.
func loadConfig(dir string) (Config, string) {
	read := func(f string) (Config, bool) {
		b, err := os.ReadFile(filepath.Join(dir, f))
		c := defaultConfig()
		if err != nil || json.Unmarshal(b, &c) != nil {
			return c, false
		}
		c.normalize()
		return c, true
	}
	cur := filepath.Join(dir, "config.json")
	if c, ok := read("config.json"); ok {
		return c, ""
	}
	if _, err := os.Stat(cur); err != nil {
		return defaultConfig(), ""
	}
	_ = os.Rename(cur, cur+".corrupto-"+strconv.FormatUint(now(), 10))
	if c, ok := read("config.json.bak"); ok {
		b, _ := json.MarshalIndent(c, "", "  ")
		_ = os.WriteFile(cur, b, 0o600)
		return c, "Configuración dañada: se restauró el último respaldo válido"
	}
	return defaultConfig(), "Configuración dañada sin respaldo: se inició con valores por defecto"
}

// saveLocked guarda config.json (s.mu tomado) con respaldo de la última configuración válida.
func (s *Server) saveLocked() error {
	b, _ := json.MarshalIndent(s.cfg, "", "  ")
	cur := filepath.Join(s.dir, "config.json")
	if old, err := os.ReadFile(cur); err == nil {
		c := defaultConfig()
		if json.Unmarshal(old, &c) == nil {
			_ = os.WriteFile(cur+".bak", old, 0o600)
		}
	}
	err := writeAtomic(cur, b)
	if err == nil {
		s.hub.notify()
	}
	return err
}

func (s *Server) save() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.saveLocked()
}

func (s *Server) config() Config {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.cfg.clone()
}

func (s *Server) saveQuickLocked() error {
	s.quick.Version = 1
	b, _ := json.MarshalIndent(s.quick, "", "  ")
	return writeAtomic(filepath.Join(s.dir, "quick.json"), b)
}

func loadQuick(dir string) Quick {
	q := Quick{Blocks: []QuickBlock{}, Paused: map[string]QuickMark{}}
	if b, err := os.ReadFile(filepath.Join(dir, "quick.json")); err == nil {
		_ = json.Unmarshal(b, &q)
	}
	if q.Blocks == nil {
		q.Blocks = []QuickBlock{}
	}
	if q.Paused == nil {
		q.Paused = map[string]QuickMark{}
	}
	return q
}

func (s *Server) quickPaused(printer string) (QuickMark, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	m, ok := s.quick.Paused[printer]
	return m, ok
}

// --- managed.json (mismo formato que escritorio; acepta la lista antigua de nombres) ---

func parseManaged(b []byte) map[string]Managed {
	m := map[string]Managed{}
	if json.Unmarshal(b, &m) == nil {
		return m
	}
	var old []string
	m = map[string]Managed{}
	if json.Unmarshal(b, &old) == nil {
		for _, p := range old {
			m[p] = Managed{}
		}
	}
	return m
}

func (s *Server) managed() map[string]Managed {
	b, _ := os.ReadFile(filepath.Join(s.dir, "managed.json"))
	return parseManaged(b)
}

func (s *Server) applyHold(printer string, on bool) error { return s.setHold(printer, on, false) }

// setHold cambia la retención. Con on y la cola ya retenida por nosotros no repite lpadmin
// (cada cambio de configuración abre una ventana de carrera en cupsd), salvo force.
func (s *Server) setHold(printer string, on, force bool) error {
	s.holdMu.Lock()
	defer s.holdMu.Unlock()
	m := s.managed()
	if _, held := m[printer]; on && held && !force {
		return nil
	}
	if on {
		if err := cups.SetHold(s.dir, printer, true); err != nil {
			return err
		}
		if _, ok := m[printer]; !ok {
			m[printer] = Managed{}
		}
	} else {
		if err := cups.SetHold(s.dir, printer, false); err != nil {
			return err
		}
		delete(m, printer)
	}
	b, _ := json.Marshal(m)
	return writeAtomic(filepath.Join(s.dir, "managed.json"), b)
}

// freeQueue quita la retención y libera los trabajos pendientes.
func (s *Server) freeQueue(printer string) {
	_ = s.applyHold(printer, false)
	ids, _ := cups.HeldJobIDs(printer)
	for _, id := range ids {
		_ = cups.Release(id)
	}
}

func (s *Server) wantsHold(cfg *Config, printer string) bool {
	if cfg.Global.Enabled {
		return true
	}
	if r := cfg.Printers[printer]; r != nil && r.Enabled {
		return true
	}
	_, ok := s.quickPaused(printer)
	return ok
}

// syncHolds reajusta la retención; devuelve las colas que fallaron.
func (s *Server) syncHolds(cfg *Config, printers []string) []string {
	var failed []string
	for _, p := range printers {
		if s.wantsHold(cfg, p) {
			if s.applyHold(p, true) != nil {
				failed = append(failed, p)
			}
		} else if _, ok := s.managed()[p]; ok {
			s.freeQueue(p)
		}
	}
	return failed
}

// ReleaseAll libera todas las colas retenidas por el servidor.
func (s *Server) ReleaseAll() {
	names := make([]string, 0)
	for p := range s.managed() {
		names = append(names, p)
	}
	sort.Strings(names)
	for _, p := range names {
		s.freeQueue(p)
	}
}

// New carga el estado de dir y prepara la contraseña de administrador (primer arranque).
func New(dir string) (*Server, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, err
	}
	hardenPerms(dir)
	cfg, warn := loadConfig(dir)
	ev, err := openEvents(dir)
	if err != nil {
		return nil, fmt.Errorf("no se pudo abrir events.db: %w", err)
	}
	s := &Server{dir: dir, cfg: cfg, quick: loadQuick(dir), auth: newAuth(), ext: loadExt(dir), events: ev, retention: retentionDays()}
	if ports, err := discover.ParsePorts(os.Getenv("CONTROL_DISCOVER_PORTS")); err == nil { // solo para pruebas locales
		s.net.ports = ports
	}
	s.purgeEvents()
	s.paused.Store(cfg.Settings.Paused)
	if warn != "" {
		s.logNotice(warn)
	}
	if err := s.initPassword(); err != nil {
		return nil, err
	}
	return s, nil
}

// Reconcile reaplica la retención al arrancar y libera colas huérfanas.
func (s *Server) Reconcile() {
	cfg := s.config()
	names, _ := cups.ListPrinters()
	for _, p := range names {
		if s.wantsHold(&cfg, p) && s.setHold(p, true, true) != nil {
			s.logNotice("No se pudo retener la cola de " + p + " (¿no admite pausa?)")
		}
	}
	for p := range s.managed() {
		if !s.wantsHold(&cfg, p) {
			s.freeQueue(p)
		}
	}
	// docker/entrypoint.sh aplica Hold-New-Jobs a todas las colas al arrancar: se quita de las que no tienen control.
	for _, p := range names {
		if !s.wantsHold(&cfg, p) {
			_ = cups.HoldNewJobs(s.dir, p, false)
		}
	}
}
