package app

import (
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/netip"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"controlimpresion/internal/cups"
	"controlimpresion/internal/rules"
)

// apiErr se devuelve al cliente como {"error": "..."} con ese código HTTP.
type apiErr struct {
	code int
	msg  string
}

func (e *apiErr) Error() string { return e.msg }

func bad(msg string) error { return &apiErr{http.StatusBadRequest, msg} }
func notDesktop(c string) error {
	return &apiErr{http.StatusNotFound, c + ": solo disponible en la aplicación de escritorio"}
}

type handler func(s *Server, r *http.Request, args json.RawMessage) (any, error)

func decode(args json.RawMessage, v any) error {
	if len(args) == 0 {
		args = []byte("{}")
	}
	if err := json.Unmarshal(args, v); err != nil {
		return bad("argumentos inválidos: " + err.Error())
	}
	return nil
}

func inert(v any) handler {
	return func(*Server, *http.Request, json.RawMessage) (any, error) { return v, nil }
}
func desktopOnly(name string) handler {
	return func(*Server, *http.Request, json.RawMessage) (any, error) { return nil, notDesktop(name) }
}

// Comandos con los mismos nombres y formas que los invoke de la app de escritorio.
var commands map[string]handler

func init() {
	commands = map[string]handler{
		"list_printers": (*Server).listPrinters,
		"set_rules":     (*Server).setRules,
		"get_global_rules": func(s *Server, _ *http.Request, _ json.RawMessage) (any, error) {
			c := s.config()
			return c.Global, nil
		},
		"set_global_rules":     (*Server).setGlobalRules,
		"get_activity":         func(s *Server, _ *http.Request, _ json.RawMessage) (any, error) { return s.activity(), nil },
		"clear_log":            (*Server).clearLog,
		"query_events":         (*Server).queryEvents,
		"export_log_csv":       (*Server).exportLogCSV,
		"get_settings":         (*Server).getSettings,
		"set_settings":         (*Server).setSettings,
		"has_password":         inert(true),
		"verify_password":      (*Server).verifyPassword,
		"set_password":         (*Server).changePassword,
		"export_rules":         (*Server).exportRules,
		"import_rules":         (*Server).importRules,
		"export_backup":        (*Server).exportBackup,
		"preview_import":       (*Server).previewImport,
		"apply_import":         (*Server).applyImport,
		"read_backup_file":     desktopOnly("read_backup_file"),
		"get_paused":           func(s *Server, _ *http.Request, _ json.RawMessage) (any, error) { return s.paused.Load(), nil },
		"set_paused":           (*Server).setPaused,
		"list_apps":            inert([]string{"*.docx", "*.doc", "*.xlsx", "*.xls", "*.pptx", "*.pdf", "*.txt", "*.jpg", "*.png"}),
		"app_icon":             inert(nil),
		"get_autostart":        inert(false),
		"list_browsers":        inert([]any{}),
		"extension_status":     inert(nil),
		"set_autostart":        desktopOnly("set_autostart"),
		"open_extension":       desktopOnly("open_extension"),
		"install_extension":    desktopOnly("install_extension"),
		"export_extension_zip": desktopOnly("export_extension_zip"),
		"grant_printer_access": desktopOnly("grant_printer_access"),
		"write_text_file":      desktopOnly("write_text_file"),
		"read_text_file":       desktopOnly("read_text_file"),
		// Bloqueo rápido (solo servidor).
		"block_host":           (*Server).blockHost,
		"unblock_host":         (*Server).unblockHost,
		"pause_printer":        (*Server).pausePrinter,
		"create_ext_token":     (*Server).createExtToken,
		"list_ext_tokens":      (*Server).listExtTokens,
		"revoke_ext_token":     (*Server).revokeExtToken,
		"get_inventory":        (*Server).getInventory,
		"summarize_events":     (*Server).summarizeEvents,
		"get_network_settings": (*Server).getNetworkSettings,
		"set_network_settings": (*Server).setNetworkSettings,
		"discover_network":     (*Server).discoverNetwork,
		"discover_status":      (*Server).discoverStatus,
		"discover_cancel":      (*Server).discoverCancel,
		"get_quick":            func(s *Server, _ *http.Request, _ json.RawMessage) (any, error) { return s.quickState(), nil },
	}
}

func (s *Server) quickState() Quick {
	s.mu.Lock()
	defer s.mu.Unlock()
	q := Quick{Blocks: slices.Clone(s.quick.Blocks), Paused: map[string]QuickMark{}, Version: 1}
	for k, v := range s.quick.Paused {
		q.Paused[k] = v
	}
	return q
}

type printerView struct {
	Name  string           `json:"name"`
	Rules *rules.Rules     `json:"rules"`
	Info  cups.PrinterInfo `json:"info"`
	// Pausada con el bloqueo rápido (quién y cuándo en quick.json).
	QuickPaused bool `json:"quick_paused,omitempty"`
}

func (s *Server) listPrinters(_ *http.Request, _ json.RawMessage) (any, error) {
	names, err := cups.ListPrinters()
	if err != nil {
		return nil, err
	}
	cfg := s.config()
	usb := cups.USBDevices()
	s.mu.Lock()
	paused := make(map[string]bool, len(s.quick.Paused))
	for n := range s.quick.Paused {
		paused[n] = true
	}
	s.mu.Unlock()
	out := make([]printerView, len(names))
	var wg sync.WaitGroup
	for i, n := range names {
		r := cfg.Printers[n]
		if r == nil {
			r = &rules.Rules{}
			r.Normalize()
		}
		out[i] = printerView{Name: n, Rules: r, QuickPaused: paused[n]}
		wg.Add(1)
		go func() { defer wg.Done(); out[i].Info = cups.Info(s.dir, n, usb) }()
	}
	wg.Wait()
	return out, nil
}

func holdWarning(failed []string) any {
	if len(failed) == 0 {
		return nil
	}
	return "No se pudo cambiar la retención de: " + strings.Join(failed, ", ") + ". Es posible que no admitan pausa; el resto quedó configurado."
}

func (s *Server) knownPrinter(p string) error {
	names, err := cups.ListPrinters()
	if err != nil {
		return err
	}
	if strings.HasPrefix(p, "-") || !slices.Contains(names, p) {
		return bad("Impresora desconocida")
	}
	return nil
}

func (s *Server) setRules(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Printer string      `json:"printer"`
		Rules   rules.Rules `json:"rules"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	if err := s.knownPrinter(a.Printer); err != nil {
		return nil, err
	}
	a.Rules.Normalize()
	cfg := s.config()
	_, qp := s.quickPaused(a.Printer)
	hold := a.Rules.Enabled || cfg.Global.Enabled || qp
	var failed []string
	if s.applyHold(a.Printer, hold) != nil {
		failed = []string{a.Printer}
	}
	s.mu.Lock()
	s.cfg.Printers[a.Printer] = &a.Rules
	err := s.saveLocked()
	s.mu.Unlock()
	return holdWarning(failed), err
}

func (s *Server) setGlobalRules(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Rules rules.Rules `json:"rules"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	a.Rules.Normalize()
	s.mu.Lock()
	s.cfg.Global = a.Rules
	s.mu.Unlock()
	return s.afterGlobalChange()
}

// afterGlobalChange reajusta la retención de todas las colas y guarda.
func (s *Server) afterGlobalChange() (any, error) {
	cfg := s.config()
	names, err := cups.ListPrinters()
	if err != nil {
		return nil, err
	}
	failed := s.syncHolds(&cfg, names)
	return holdWarning(failed), s.save()
}

func (s *Server) getSettings(_ *http.Request, _ json.RawMessage) (any, error) {
	c := s.config()
	st := c.Settings
	st.PasswordHash, st.PasswordSalt = "set", ""
	return st, nil
}

func (s *Server) setSettings(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Settings json.RawMessage `json:"settings"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	n := s.cfg.Settings // los campos ausentes conservan su valor
	if err := json.Unmarshal(a.Settings, &n); err != nil {
		return nil, bad("ajustes inválidos")
	}
	n.PasswordHash, n.PasswordSalt = s.cfg.Settings.PasswordHash, s.cfg.Settings.PasswordSalt
	n.Paused = s.paused.Load()
	n.IntervalSecs = min(max(n.IntervalSecs, 1), 60)
	n.RetentionDays = min(max(n.RetentionDays, 1), 3650)
	if n.UnknownPolicy != "block" {
		n.UnknownPolicy = "allow"
	}
	s.cfg.Settings = n
	return nil, s.saveLocked()
}

func (s *Server) verifyPassword(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Password string `json:"password"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	return s.checkPassword(a.Password), nil
}

func (s *Server) changePassword(r *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Current string `json:"current"`
		New     string `json:"new"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	if !s.checkPassword(a.Current) {
		return nil, bad("Contraseña actual incorrecta")
	}
	if len(a.New) < 8 {
		return nil, bad("En modo servidor la contraseña es obligatoria y debe tener al menos 8 caracteres")
	}
	if err := s.setPassword(a.New); err != nil {
		return nil, err
	}
	s.auth.revokeExcept(requestToken(r))
	return nil, nil
}

func (s *Server) setPaused(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Paused bool `json:"paused"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	s.paused.Store(a.Paused)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cfg.Settings.Paused = a.Paused
	return nil, s.saveLocked()
}

// --- Bloqueo rápido ---

func who(r *http.Request) string { return "admin@" + clientIP(r) }

// resolveHost devuelve las IP/CIDR a bloquear para un host (IP, CIDR o nombre).
func resolveHost(h string) ([]string, error) {
	h = strings.TrimSpace(h)
	if h == "" || len(h) > 253 || strings.ContainsAny(h, " \t\r\n,;") {
		return nil, bad("Equipo inválido")
	}
	if base, n, ok := strings.Cut(h, "/"); ok {
		if _, err := netip.ParsePrefix(base + "/" + n); err != nil {
			return nil, bad("CIDR inválido")
		}
		return []string{h}, nil
	}
	if ip, ok := rules.ParseIP(h); ok {
		return []string{ip.String()}, nil
	}
	ips, err := net.LookupIP(h)
	if err != nil || len(ips) == 0 {
		return nil, bad("No se pudo resolver el equipo «" + h + "»; usa su IP")
	}
	var v4, v6 []string
	for _, ip := range ips {
		if x := ip.To4(); x != nil {
			v4 = append(v4, x.String())
		} else {
			v6 = append(v6, ip.String())
		}
	}
	out := v4
	if len(out) == 0 {
		out = v6
	}
	slices.Sort(out)
	return slices.Compact(out), nil
}

func (s *Server) blockHost(r *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Host string `json:"host"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	ips, err := resolveHost(a.Host)
	if err != nil {
		return nil, err
	}
	s.mu.Lock()
	g := &s.cfg.Global
	// La regla global debe estar en modo bloqueo: en modo permitir añadir una IP la autorizaría.
	if g.Mode != nil && *g.Mode == rules.Allow && (g.Enabled || len(g.Apps)+len(g.Sites)+len(g.IPs) > 0) {
		s.mu.Unlock()
		return nil, bad("La regla global está en modo «Permitir solo coincidencias»: cámbiala a bloquear para usar el bloqueo rápido")
	}
	blk := rules.Block
	g.Mode, g.Enabled = &blk, true
	for _, ip := range ips {
		if !slices.Contains(g.IPs, ip) {
			g.IPs = append(g.IPs, ip)
		}
	}
	s.quick.Blocks = slices.DeleteFunc(s.quick.Blocks, func(b QuickBlock) bool { return b.Host == strings.TrimSpace(a.Host) })
	s.quick.Blocks = append(s.quick.Blocks, QuickBlock{Host: strings.TrimSpace(a.Host), IPs: ips, By: who(r), Time: now()})
	err = s.saveQuickLocked()
	s.mu.Unlock()
	if err != nil {
		return nil, err
	}
	s.logNotice("Bloqueo rápido: " + strings.Join(ips, ", ") + " bloqueado por " + who(r))
	w, err := s.afterGlobalChange()
	return map[string]any{"host": strings.TrimSpace(a.Host), "ips": ips, "warning": w}, err
}

func (s *Server) unblockHost(r *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Host string `json:"host"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	host := strings.TrimSpace(a.Host)
	s.mu.Lock()
	remove := map[string]bool{host: true}
	if ip, ok := rules.ParseIP(host); ok {
		remove[ip.String()] = true
	}
	for _, b := range s.quick.Blocks {
		if b.Host == host {
			for _, ip := range b.IPs {
				remove[ip] = true
			}
		}
	}
	n := len(s.cfg.Global.IPs)
	s.cfg.Global.IPs = slices.DeleteFunc(s.cfg.Global.IPs, func(p string) bool { return remove[p] })
	found := n != len(s.cfg.Global.IPs)
	s.quick.Blocks = slices.DeleteFunc(s.quick.Blocks, func(b QuickBlock) bool { return b.Host == host })
	err := s.saveQuickLocked()
	s.mu.Unlock()
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, bad("«" + host + "» no está bloqueado")
	}
	s.logNotice("Bloqueo rápido: " + host + " desbloqueado por " + who(r))
	w, err := s.afterGlobalChange()
	return map[string]any{"host": host, "warning": w}, err
}

func (s *Server) pausePrinter(r *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Printer string `json:"printer"`
		Paused  bool   `json:"paused"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	if err := s.knownPrinter(a.Printer); err != nil {
		return nil, err
	}
	s.mu.Lock()
	if a.Paused {
		s.quick.Paused[a.Printer] = QuickMark{By: who(r), Time: now()}
	} else {
		delete(s.quick.Paused, a.Printer)
	}
	err := s.saveQuickLocked()
	s.mu.Unlock()
	if err != nil {
		return nil, err
	}
	cfg := s.config()
	failed := s.syncHolds(&cfg, []string{a.Printer})
	verb := "reanudada"
	if a.Paused {
		verb = "pausada"
	}
	s.logNotice("Bloqueo rápido: impresora " + a.Printer + " " + verb + " por " + who(r))
	return holdWarning(failed), nil
}

// --- HTTP ---

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(v)
}

func fail401(w http.ResponseWriter) {
	writeJSON(w, http.StatusUnauthorized, map[string]string{"error": "Sesión requerida"})
}

func (s *Server) login(w http.ResponseWriter, r *http.Request, args json.RawMessage) {
	ip := clientIP(r)
	if d := s.auth.reserve(ip); d > 0 {
		w.Header().Set("Retry-After", strconv.Itoa(int(d.Seconds())+1))
		writeJSON(w, http.StatusTooManyRequests, map[string]string{"error": "Demasiados intentos; espera " + strconv.Itoa(int(d.Minutes())+1) + " min"})
		return
	}
	var a struct {
		Password string `json:"password"`
	}
	if decode(args, &a) != nil || !s.checkPassword(a.Password) {
		time.Sleep(300 * time.Millisecond)
		fail401(w)
		return
	}
	s.auth.ok(ip)
	t := s.auth.create()
	http.SetCookie(w, &http.Cookie{Name: cookieName, Value: t, Path: "/", MaxAge: int(sessionTTL.Seconds()), HttpOnly: true, SameSite: http.SameSiteStrictMode, Secure: isSecure(r)})
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "token": t})
}

// API atiende POST /api/<comando>. Todo exige sesión salvo login.
func (s *Server) API(w http.ResponseWriter, r *http.Request) {
	name := strings.TrimPrefix(r.URL.Path, "/api/")
	if r.Method != http.MethodPost {
		w.Header().Set("Allow", "POST")
		writeJSON(w, http.StatusMethodNotAllowed, map[string]string{"error": "Método no permitido"})
		return
	}
	if !sameOrigin(r) {
		writeJSON(w, http.StatusForbidden, map[string]string{"error": "Origen no permitido"})
		return
	}
	// Sin sesión no se lee el cuerpo (las copias admiten hasta backupBodyMax; el resto 4 MiB).
	tok := requestToken(r)
	if name != "login" && !s.auth.valid(tok) {
		fail401(w)
		return
	}
	limit := int64(4 << 20)
	if name == "preview_import" || name == "apply_import" {
		limit = backupBodyMax
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, limit))
	if err != nil {
		writeJSON(w, http.StatusRequestEntityTooLarge, map[string]string{"error": "Petición demasiado grande"})
		return
	}
	if name == "login" {
		s.login(w, r, body)
		return
	}
	if name == "logout" {
		s.auth.revoke(tok)
		http.SetCookie(w, &http.Cookie{Name: cookieName, Value: "", Path: "/", MaxAge: -1, HttpOnly: true, SameSite: http.SameSiteStrictMode})
		writeJSON(w, http.StatusOK, nil)
		return
	}
	h, ok := commands[name]
	if !ok {
		writeJSON(w, http.StatusNotFound, map[string]string{"error": "Comando desconocido: " + name})
		return
	}
	res, err := h(s, r, body)
	if err != nil {
		var ae *apiErr
		code := http.StatusInternalServerError
		if errors.As(err, &ae) {
			code = ae.code
		}
		writeJSON(w, code, map[string]string{"error": err.Error()})
		return
	}
	writeJSON(w, http.StatusOK, res)
}
