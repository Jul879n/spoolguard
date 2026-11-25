package app

import (
	"context"
	"fmt"
	"strconv"
	"strings"
	"time"

	"controlimpresion/internal/cups"
	"controlimpresion/internal/discover"
)

// Detección de impresión directa (mejor esfuerzo). Cada intervalo consulta por IPP a las impresoras de red que
// tienen cola en este servidor y compara el contador de la propia impresora con lo que el servidor liberó.
// Fuente 1: printer-impressions-completed (Get-Printer-Attributes). Fuente 2: trabajos terminados que la
// impresora recuerda (Get-Jobs completed, job-impressions-completed). Si ninguna existe, la impresora figura
// como "no verificable". NO se probó con hardware real: ver docs/modo-servidor.md.

const (
	directGrace     = 120 * time.Second // un trabajo liberado puede imprimirse algo después
	directAttrName  = "printer-impressions-completed"
	directSrcPrint  = "printer-impressions-completed"
	directSrcJobs   = "job-impressions-completed"
	directMaxJobsSN = 4096 // tope de ids de trabajos recordados por impresora
)

type directPrinter struct {
	Printer  string `json:"printer"`
	Host     string `json:"host"`
	Source   string `json:"source"` // directSrcPrint | directSrcJobs | "" (no verificable)
	Total    int64  `json:"total"`  // último valor del contador (fuente 1)
	LastPoll int64  `json:"last_poll"`
	Error    string `json:"error"`
	Alerts   int    `json:"alerts"`

	prevPoll time.Time
	seen     map[int64]bool // fuente 2
	ready    bool           // ya hay línea base
}

func (s *Server) directStatus() []directPrinter {
	s.net.dmu.Lock()
	defer s.net.dmu.Unlock()
	out := []directPrinter{}
	for _, d := range s.net.direct {
		out = append(out, *d)
	}
	return out
}

// DirectWatch revisa cada 5 s si toca consultar (intervalo configurable; apagado por defecto).
func (s *Server) DirectWatch(ctx context.Context) {
	t := time.NewTicker(5 * time.Second)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
		n := s.loadNet()
		if !n.Direct.Enabled {
			s.net.dmu.Lock()
			s.net.direct, s.net.lastAt = nil, time.Time{} // al apagar se pierde la línea base
			s.net.dmu.Unlock()
			continue
		}
		s.net.dmu.Lock()
		due := time.Since(s.net.lastAt) >= time.Duration(n.Direct.IntervalSecs)*time.Second
		s.net.dmu.Unlock()
		if due {
			safely("detección directa", func() { s.DirectTick(ctx, time.Now()) })
		}
	}
}

type directTarget struct {
	printer, uri, host string
	port               int
}

// directTargets: colas cuyo dispositivo es una impresora de red consultable por IPP sin cifrado.
// ipps:// no se consulta (haría falta TLS con certificados de la impresora, casi siempre autofirmados).
func (s *Server) directTargets() []directTarget {
	names, err := cups.ListPrinters()
	if err != nil {
		return nil
	}
	ippPort := s.net.ports.IPP
	if ippPort == 0 {
		ippPort = discover.DefaultPorts.IPP
	}
	var out []directTarget
	for _, n := range names {
		uri := s.deviceURI(n)
		if strings.HasPrefix(strings.ToLower(uri), "ipps:") || strings.HasPrefix(strings.ToLower(uri), "https:") {
			continue
		}
		h := cups.NetHost(uri)
		if h == "" || cups.ConnType(uri) == "compartida" {
			continue
		}
		port := ippPort
		if cups.ConnType(uri) == "ipp" {
			if _, p, ok := cups.HostPort(uri); ok {
				if v, err := strconv.Atoi(p); err == nil {
					port = v
				}
			}
		}
		out = append(out, directTarget{n, uri, h, port})
	}
	return out
}

// DirectTick hace una pasada de consulta; `at` es la hora de la pasada (inyectable en pruebas).
func (s *Server) DirectTick(ctx context.Context, at time.Time) {
	tr := s.net.tr
	if tr == nil {
		tr = discover.HTTPTransport
	}
	s.net.dmu.Lock()
	s.net.lastAt = at
	if s.net.direct == nil {
		s.net.direct = map[string]*directPrinter{}
	}
	s.net.dmu.Unlock()
	keep := map[string]bool{}
	for _, t := range s.directTargets() {
		keep[t.printer] = true
		s.pollOne(ctx, tr, t, at)
	}
	s.net.dmu.Lock()
	for k := range s.net.direct {
		if !keep[k] {
			delete(s.net.direct, k)
		}
	}
	s.net.dmu.Unlock()
}

// reading es el resultado de una consulta: valor acumulado (fuente 1) o conjunto de trabajos (fuente 2).
type reading struct {
	source string
	total  int64
	jobs   []discover.CompletedJob
}

func readCounter(ctx context.Context, tr discover.Transport, t directTarget) (*reading, error) {
	attrs, err := discover.GetPrinterAttributes(ctx, tr, t.host, t.port, []string{directAttrName})
	if err != nil {
		return nil, err
	}
	if v := attrs[directAttrName]; len(v) > 0 {
		if n, err := strconv.ParseInt(v[0], 10, 64); err == nil && n >= 0 {
			return &reading{source: directSrcPrint, total: n}, nil
		}
	}
	jobs, err := discover.GetCompletedJobs(ctx, tr, t.host, t.port)
	if err != nil {
		return nil, fmt.Errorf("la impresora no expone contador por IPP: %w", err)
	}
	return &reading{source: directSrcJobs, jobs: jobs}, nil
}

func (s *Server) pollOne(ctx context.Context, tr discover.Transport, t directTarget, at time.Time) {
	s.net.dmu.Lock()
	st := s.net.direct[t.printer]
	if st == nil {
		st = &directPrinter{Printer: t.printer, Host: t.host}
		s.net.direct[t.printer] = st
	}
	s.net.dmu.Unlock()

	rd, err := readCounter(ctx, tr, t) // sin candado: es una consulta de red
	s.net.dmu.Lock()
	st.LastPoll = at.Unix()
	if err != nil {
		st.Error, st.Source, st.ready = "no verificable: "+err.Error(), "", false
		s.net.dmu.Unlock()
		return
	}
	st.Error = ""
	since := st.prevPoll
	// Línea base: la primera lectura (o un cambio de fuente) nunca genera alertas.
	if !st.ready || st.Source != rd.source {
		st.Source, st.ready, st.prevPoll = rd.source, true, at
		st.Total, st.seen = rd.total, map[int64]bool{}
		for _, j := range rd.jobs {
			st.seen[j.ID] = true
		}
		s.net.dmu.Unlock()
		return
	}
	var delta int64
	unit := "impresiones"
	switch rd.source {
	case directSrcPrint:
		delta = rd.total - st.Total
		st.Total = rd.total // si baja (reinicio del contador) solo se rebasa
	default:
		var nj, imp int64
		known := true
		for _, j := range rd.jobs {
			if st.seen[j.ID] {
				continue
			}
			if len(st.seen) < directMaxJobsSN {
				st.seen[j.ID] = true
			}
			nj++
			if j.Impressions >= 0 {
				imp += j.Impressions
			} else {
				known = false
			}
		}
		delta = imp
		if !known {
			delta, unit = nj, "trabajos"
		}
	}
	st.prevPoll = at
	s.net.dmu.Unlock()
	if delta <= 0 || s.explainedByServer(t.printer, since.Add(-directGrace), delta, unit) {
		return
	}
	s.net.dmu.Lock()
	st.Alerts++
	s.net.dmu.Unlock()
	e := Event{Printer: t.printer, Decision: "notice", Status: "aviso", Reason: "Impresión directa sospechosa",
		Document: fmt.Sprintf("Impresión directa sospechosa en %s: +%d %s según la impresora (%s) sin trabajos liberados por el servidor que lo expliquen", t.printer, delta, unit, at.Format("15:04")),
		Port:     sanitizeURI(t.uri), ConnType: cups.ConnType(t.uri)}
	if unit == "impresiones" {
		d := delta
		e.Pages = &d
	}
	s.appendEvent(e)
}

// explainedByServer: ¿lo que liberó el servidor para esa cola desde `from` explica el aumento?
// Sin trabajos liberados no hay explicación. Con trabajos, solo se descarta la alerta si no se puede
// comparar (alguno sin páginas) o si el aumento no supera lo liberado (en trabajos si la unidad es "trabajos").
func (s *Server) explainedByServer(printer string, from time.Time, delta int64, unit string) bool {
	f := from.Unix()
	rows, _, err := s.events.query(EventFilter{Printer: printer, From: &f, Decision: "allow"}, 1000, 0)
	if err != nil {
		return true // sin poder comprobar, mejor no acusar
	}
	if len(rows) == 0 {
		return false
	}
	if unit == "trabajos" {
		return delta <= int64(len(rows))
	}
	var sum int64
	for _, r := range rows {
		if r.Pages == nil {
			return true // no se puede comparar páginas con trabajos de tamaño desconocido
		}
		sum += *r.Pages
	}
	return delta <= sum
}
