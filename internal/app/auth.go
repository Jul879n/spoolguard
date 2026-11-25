package app

import (
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"fmt"
	"net"
	"net/http"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

const (
	cookieName   = "ci_session"
	sessionTTL   = 12 * time.Hour
	maxFails     = 5
	lockDuration = 15 * time.Minute
)

type fail struct {
	n     int
	first time.Time // inicio de la ventana de conteo
	until time.Time
}

// Auth guarda sesiones (solo en memoria: un reinicio las invalida) y el límite de intentos por IP.
type Auth struct {
	mu       sync.Mutex
	sessions map[[32]byte]time.Time
	fails    map[string]*fail
}

func newAuth() *Auth {
	return &Auth{sessions: map[[32]byte]time.Time{}, fails: map[string]*fail{}}
}

func tokenKey(t string) [32]byte { return sha256.Sum256([]byte(t)) }

func (a *Auth) create() string {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		panic(err)
	}
	t := base64.RawURLEncoding.EncodeToString(b)
	a.mu.Lock()
	defer a.mu.Unlock()
	now := time.Now()
	for k, exp := range a.sessions { // purga vencidas
		if now.After(exp) {
			delete(a.sessions, k)
		}
	}
	a.sessions[tokenKey(t)] = now.Add(sessionTTL)
	return t
}

func (a *Auth) valid(t string) bool {
	if t == "" {
		return false
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	exp, ok := a.sessions[tokenKey(t)]
	return ok && time.Now().Before(exp)
}

func (a *Auth) revoke(t string) {
	a.mu.Lock()
	delete(a.sessions, tokenKey(t))
	a.mu.Unlock()
}

// revokeExcept cierra todas las sesiones salvo la indicada (cambio de contraseña).
func (a *Auth) revokeExcept(t string) {
	keep := tokenKey(t)
	a.mu.Lock()
	for k := range a.sessions {
		if k != keep {
			delete(a.sessions, k)
		}
	}
	a.mu.Unlock()
}

// reserve cuenta un intento de inicio de sesión ANTES de comprobar la contraseña, de forma atómica: si se
// contara después, N peticiones simultáneas pasarían todas el control de bloqueo y darían N intentos.
// Devuelve el tiempo de bloqueo restante (0 = puede intentar). El intento maxFails+1 ya queda bloqueado.
func (a *Auth) reserve(ip string) time.Duration {
	a.mu.Lock()
	defer a.mu.Unlock()
	now := time.Now()
	f := a.fails[ip]
	if f == nil {
		if len(a.fails) > 10000 { // límite de memoria
			a.fails = map[string]*fail{}
		}
		f = &fail{first: now}
		a.fails[ip] = f
	}
	if now.Before(f.until) {
		return time.Until(f.until)
	}
	if !f.until.IsZero() || now.Sub(f.first) > lockDuration { // bloqueo cumplido o ventana vencida
		f.n, f.until, f.first = 0, time.Time{}, now
	}
	f.n++
	if f.n > maxFails {
		f.until = now.Add(lockDuration)
		f.n = 0
		return lockDuration
	}
	return 0
}

func (a *Auth) ok(ip string) {
	a.mu.Lock()
	delete(a.fails, ip)
	a.mu.Unlock()
}

// checkPassword compara en tiempo constante. Acepta el formato PBKDF2 y el SHA-256 antiguo
// (sal + contraseña); en este último caso, si coincide, migra transparentemente al formato nuevo.
func (s *Server) checkPassword(pw string) bool {
	s.mu.Lock()
	salt, want := s.cfg.Settings.PasswordSalt, s.cfg.Settings.PasswordHash
	s.mu.Unlock()
	if want == "" {
		return false // el servidor nunca queda sin contraseña
	}
	pwSem <- struct{}{} // el hash es lento a propósito: acota cuántos corren a la vez
	defer func() { <-pwSem }()
	if isKDF(want) {
		return kdfVerify(pw, want)
	}
	if subtle.ConstantTimeCompare([]byte(hashPW(salt, pw)), []byte(want)) != 1 {
		return false
	}
	s.mu.Lock()
	if s.cfg.Settings.PasswordHash == want { // nadie la cambió mientras tanto
		s.cfg.Settings.PasswordSalt, s.cfg.Settings.PasswordHash = "", kdfHash(pw)
		_ = s.saveLocked()
	}
	s.mu.Unlock()
	return true
}

func (s *Server) setPassword(pw string) error {
	h := kdfHash(pw)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cfg.Settings.PasswordSalt = ""
	s.cfg.Settings.PasswordHash = h
	return s.saveLocked()
}

// initPassword define la contraseña de administrador en el primer arranque:
// CONTROL_ADMIN_PASSWORD, o el archivo CONTROL_ADMIN_PASSWORD_FILE (por defecto <datos>/admin_password),
// o una aleatoria que se imprime una sola vez en el registro del proceso.
func (s *Server) initPassword() error {
	if s.cfg.Settings.PasswordHash != "" {
		return nil
	}
	pw := os.Getenv("CONTROL_ADMIN_PASSWORD")
	if pw == "" {
		f := os.Getenv("CONTROL_ADMIN_PASSWORD_FILE")
		if f == "" {
			f = filepath.Join(s.dir, "admin_password")
		}
		if b, err := os.ReadFile(f); err == nil {
			pw = strings.TrimRight(string(b), "\r\n")
		}
	}
	if pw == "" {
		pw = randHex(8)
		fmt.Fprintf(os.Stderr, "control-server: no hay contraseña de administrador; se generó una inicial (cámbiala en Ajustes): %s\n", pw)
	}
	return s.setPassword(pw)
}

// pwSem limita las comprobaciones de contraseña simultáneas (PBKDF2 cuesta ~0,2 s de CPU cada una).
var pwSem = make(chan struct{}, 4)

// clientIP: con CONTROL_TRUST_PROXY=1 se usa la ÚLTIMA entrada de X-Forwarded-For (la que añadió el proxy de
// confianza). La primera la escribe el propio cliente y permitiría saltarse el bloqueo por IP.
func clientIP(r *http.Request) string {
	if os.Getenv("CONTROL_TRUST_PROXY") == "1" {
		if f := r.Header.Get("X-Forwarded-For"); f != "" {
			parts := strings.Split(f, ",")
			if ip, err := netip.ParseAddr(strings.TrimSpace(parts[len(parts)-1])); err == nil {
				return ip.String()
			}
		}
	}
	h, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return h
}

func requestToken(r *http.Request) string {
	if h := r.Header.Get("Authorization"); strings.HasPrefix(h, "Bearer ") {
		return strings.TrimSpace(h[7:])
	}
	if c, err := r.Cookie(cookieName); err == nil {
		return c.Value
	}
	return ""
}

func isSecure(r *http.Request) bool {
	return r.TLS != nil || r.Header.Get("X-Forwarded-Proto") == "https"
}

// sameOrigin rechaza peticiones de otro origen (defensa CSRF adicional a SameSite=Strict).
func sameOrigin(r *http.Request) bool {
	o := r.Header.Get("Origin")
	if o == "" {
		return true
	}
	i := strings.Index(o, "://")
	return i >= 0 && o[i+3:] == r.Host
}
