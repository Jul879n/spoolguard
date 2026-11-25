package app

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sync"
	"time"
)

const (
	eventsPerMin       = 30 // reconexiones de /ext/events por token
	maxStreamsPerToken = 4
	eventsBeat         = 20 * time.Second
)

// rulesHub avisa a los flujos /ext/events cuando cambia la configuración (se cierra el canal y se crea otro).
type rulesHub struct {
	mu      sync.Mutex
	ch      chan struct{}
	streams map[string]int
}

func (h *rulesHub) wait() <-chan struct{} {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.ch == nil {
		h.ch = make(chan struct{})
	}
	return h.ch
}

func (h *rulesHub) notify() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.ch != nil {
		close(h.ch)
	}
	h.ch = make(chan struct{})
}

func (h *rulesHub) acquire(id string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.streams == nil {
		h.streams = map[string]int{}
	}
	if h.streams[id] >= maxStreamsPerToken {
		return false
	}
	h.streams[id]++
	return true
}

func (h *rulesHub) release(id string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.streams[id]--; h.streams[id] <= 0 {
		delete(h.streams, id)
	}
}

// extEvents es un flujo SSE: envía las reglas al conectar y cada vez que cambian (`event: rules`), con un
// latido cada 20 s. La extensión lo usa para no esperar al sondeo; el sondeo queda de respaldo.
func (s *Server) extEvents(w http.ResponseWriter, r *http.Request) {
	if !s.extAuth(w, r, http.MethodGet, "events", eventsPerMin) {
		return
	}
	tok := extBearer(r)
	id, _ := s.ext.lookup(tok)
	fl, ok := w.(http.Flusher)
	if !ok {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "Sin soporte de flujo"})
		return
	}
	if !s.hub.acquire(id) {
		w.Header().Set("Retry-After", "30")
		writeJSON(w, http.StatusTooManyRequests, map[string]string{"error": "Demasiados flujos abiertos"})
		return
	}
	defer s.hub.release(id)
	// WriteTimeout del servidor (60 s) cortaría el flujo: se quita solo para esta petición.
	_ = http.NewResponseController(w).SetWriteDeadline(time.Time{})
	h := w.Header()
	h.Set("Content-Type", "text/event-stream")
	h.Set("Cache-Control", "no-cache")
	h.Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	var last []byte
	send := func() bool {
		b, _ := json.Marshal(rulesForExt(s.config()))
		if bytes.Equal(b, last) {
			return true
		}
		last = b
		if _, err := fmt.Fprintf(w, "event: rules\ndata: %s\n\n", b); err != nil {
			return false
		}
		fl.Flush()
		return true
	}
	beat := time.NewTicker(eventsBeat)
	defer beat.Stop()
	for {
		sig := s.hub.wait() // antes de leer: un cambio entre la lectura y la espera no se pierde
		if !send() {
			return
		}
		select {
		case <-sig:
		case <-beat.C:
			if _, still := s.ext.lookup(tok); !still {
				return // token revocado
			}
			if _, err := io.WriteString(w, ": ping\n\n"); err != nil {
				return
			}
			fl.Flush()
		case <-r.Context().Done():
			return
		}
	}
}
