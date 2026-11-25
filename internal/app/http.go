package app

import (
	"io/fs"
	"net/http"
)

const csp = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors 'none'"

// Handler sirve /api (con sesión) y la interfaz estática (ui) sin depender de archivos externos.
func (s *Server) Handler(ui fs.FS) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/api/", s.API)
	mux.HandleFunc("/api/events/ingest", s.eventsIngest) // token de equipo, sin sesión
	mux.HandleFunc("/ext/rules", s.extRules)
	mux.HandleFunc("/ext/events", s.extEvents)
	mux.HandleFunc("/ext/intent", s.extIntent)
	mux.HandleFunc("/ext/version", s.extVersion)
	mux.HandleFunc("/ext/package.zip", s.extPackageZip)
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) { w.Write([]byte("ok")) })
	mux.Handle("/", http.FileServerFS(ui))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("Content-Security-Policy", csp)
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("X-Frame-Options", "DENY")
		h.Set("Referrer-Policy", "no-referrer")
		h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=(), usb=(), payment=()")
		h.Set("Cross-Origin-Opener-Policy", "same-origin")
		if len(r.URL.Path) < 5 || r.URL.Path[:5] != "/api/" {
			h.Set("Cache-Control", "no-cache")
		}
		mux.ServeHTTP(w, r)
	})
}
