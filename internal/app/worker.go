package app

import (
	"context"
	"strconv"
	"time"

	"controlimpresion/internal/cups"
	"controlimpresion/internal/rules"
)

// Worker evalúa los trabajos retenidos. Sondeo adaptativo igual que escritorio:
// 200 ms con actividad reciente (5 s tras ver trabajos), intervalo configurado en reposo.
func (s *Server) Worker(ctx context.Context) {
	busyUntil := time.Now()
	lastPurge := time.Now()
	noHold := map[string]bool{} // colas que no admiten retención: se intenta una vez
	for {
		cfg := s.config()
		idle := time.Duration(max(cfg.Settings.IntervalSecs, 1)) * time.Second
		wait := idle
		if time.Now().Before(busyUntil) {
			wait = 200 * time.Millisecond
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(wait):
		}
		if time.Since(lastPurge) >= time.Hour {
			lastPurge = time.Now()
			s.purgeEvents()
		}
		cfg = s.config()
		safely("worker", func() {
			if s.tick(&cfg, noHold) {
				busyUntil = time.Now().Add(5 * time.Second)
			}
		})
	}
}

// tick hace una pasada; devuelve true si había trabajos pendientes.
func (s *Server) tick(cfg *Config, noHold map[string]bool) bool {
	s.mu.Lock()
	hasQuick := len(s.quick.Paused) > 0
	s.mu.Unlock()
	anyRule := cfg.Global.Enabled || hasQuick
	for _, r := range cfg.Printers {
		anyRule = anyRule || r.Enabled
	}
	if !anyRule {
		return false
	}
	names, err := cups.ListPrinters()
	if err != nil {
		return false
	}
	known := s.managed()
	var targets []string
	for _, p := range names {
		if !s.wantsHold(cfg, p) {
			continue
		}
		if _, ok := known[p]; !ok && !noHold[p] {
			// Cola nueva (o sin retener): se retiene antes de evaluar.
			if s.applyHold(p, true) != nil {
				noHold[p] = true
				s.logNotice("No se pudo retener la cola de " + p + " (¿no admite pausa?)")
			}
		}
		targets = append(targets, p)
	}
	busy := false
	none := &rules.Rules{}
	seen := map[uint32]bool{}
	listed := true
	for _, p := range targets {
		r := cfg.Printers[p]
		if r == nil {
			r = none
		}
		ids, err := cups.HeldJobIDs(p)
		if err != nil {
			listed = false
			continue
		}
		for _, id := range ids {
			seen[id] = true
		}
		if len(ids) > 0 {
			busy = true
		}
		for _, id := range ids {
			s.evaluate(cfg, p, r, id)
		}
	}
	if listed {
		// Olvida los trabajos liberados que ya salieron de las colas.
		for id := range s.released {
			if !seen[id] {
				delete(s.released, id)
			}
		}
		for id := range s.readFails {
			if !seen[id] {
				delete(s.readFails, id)
			}
		}
	}
	return busy
}

// maxReadFails: lecturas fallidas seguidas antes de dar el trabajo por ilegible (descarta fallos transitorios de ipptool).
const maxReadFails = 3

// unreadable aplica la política de desconocidos a un trabajo retenido que no se pudo leer, y lo registra.
// Nunca queda retenido en silencio.
func (s *Server) unreadable(cfg *Config, printer string, id uint32, cause error) {
	if s.readFails == nil {
		s.readFails = map[uint32]int{}
	}
	s.readFails[id]++
	if s.readFails[id] < maxReadFails {
		return
	}
	var allowed bool
	var reason string
	if mark, qp := s.quickPaused(printer); qp {
		allowed, reason = false, "Impresora pausada por "+mark.By
	} else if s.paused.Load() {
		allowed, reason = true, "Control pausado"
	} else {
		allowed = cfg.Settings.UnknownPolicy != "block"
		reason = "No se pudo leer el trabajo (política de desconocidos)"
	}
	var err error
	cups.WriteVerdict(printer, id, allowed)
	if allowed {
		err = cups.Release(id)
	} else {
		err = cups.Cancel(id)
	}
	delete(s.readFails, id)
	if err != nil {
		// Ya no existe (cancelado por su dueño) o cupsd falla: se avisa una vez y se reintenta si sigue ahí.
		s.logNotice("No se pudo leer el trabajo " + strconv.FormatUint(uint64(id), 10) + " de " + printer + ": " + cause.Error())
		return
	}
	if allowed {
		if s.released == nil {
			s.released = map[uint32]bool{}
		}
		s.released[id] = true
	}
	ev := activityEvent(Activity{Time: now(), Printer: printer, Document: "Trabajo " + strconv.FormatUint(uint64(id), 10) + ": no se pudo leer (" + cause.Error() + ")", Allowed: allowed, Reason: reason}, hostName)
	s.fillRoute(&ev)
	s.appendEvent(ev)
}

func (s *Server) evaluate(cfg *Config, printer string, r *rules.Rules, id uint32) {
	if s.released[id] {
		// Ya liberado y a la espera de turno (impresora ocupada): `lpstat -o` aún lo lista como pendiente;
		// no se vuelve a retener ni a evaluar (se registraba una y otra vez).
		return
	}
	h, err := cups.JobInfo(s.dir, id)
	if err != nil {
		s.unreadable(cfg, printer, id, err)
		return
	}
	delete(s.readFails, id)
	if h.State == "pending" || h.State == "processing" {
		// CUPS no lo retuvo (carrera con lpadmin o el cliente pidió job-hold-until=no-hold): se retiene ahora,
		// aunque ya esté en proceso, y se evalúa en el siguiente ciclo.
		_ = cups.Hold(id)
		return
	}
	if !h.Held {
		return
	}
	if h.Job.Site == nil {
		// Intención reciente informada por la extensión (mismo criterio que la app de escritorio).
		if site := findSite(s.dir, h.Job.Document, now()); site != "" {
			h.Job.Site = &site
		}
	}
	var allowed bool
	var reason string
	mark, qp := s.quickPaused(printer)
	switch {
	case qp:
		allowed, reason = false, "Impresora pausada por "+mark.By
	case s.paused.Load():
		allowed, reason = true, "Control pausado"
	case h.Job.Document == "":
		allowed, reason = cfg.Settings.UnknownPolicy != "block", "Datos insuficientes"
	default:
		allowed, reason = rules.ExplainBoth(&cfg.Global, r, &h.Job)
	}
	cups.WriteVerdict(printer, id, allowed)
	if allowed {
		err = cups.Release(id)
	} else {
		err = cups.Cancel(id)
	}
	if err != nil {
		return
	}
	if allowed {
		if s.released == nil {
			s.released = map[uint32]bool{}
		}
		s.released[id] = true
	}
	ev := activityEvent(Activity{Time: now(), Printer: printer, Document: h.Job.Document, User: h.Job.User, Origin: h.Job.OriginIP, Allowed: allowed, Reason: reason}, hostName)
	ev.Site, ev.App = deref(h.Job.Site), deref(h.Job.App)
	ev.Pages, ev.Bytes = h.Pages, h.Bytes
	s.fillRoute(&ev)
	s.appendEvent(ev)
}
