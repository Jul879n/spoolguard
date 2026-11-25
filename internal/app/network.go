package app

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/netip"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"controlimpresion/internal/cups"
	"controlimpresion/internal/discover"
)

// Descubrimiento de red (opcional) y detección de impresión directa (opcional, apagada por defecto).
// Nada de esto se ejecuta sin que el administrador lo inicie o lo active; ver docs/modo-servidor.md.

const (
	netFile            = "network.json"
	directDefaultSecs  = 300
	directMinSecs      = 30
	directMaxSecs      = 86400
	discoverMaxRunTime = 5 * time.Minute
)

type directSettings struct {
	Enabled      bool `json:"enabled"`
	IntervalSecs int  `json:"interval_secs"`
}

// netSettings se guarda en network.json (aparte de config.json: no forma parte del formato compartido con escritorio).
type netSettings struct {
	CIDRs  []string       `json:"cidrs"` // por defecto vacío
	Direct directSettings `json:"direct"`
}

type netState struct {
	mu    sync.Mutex // job y ajustes
	job   *discJob
	tr    discover.Transport // nil = HTTP real (las pruebas inyectan uno simulado)
	ports discover.Ports
	dial  discover.Dialer
	// detección de impresión directa
	dmu    sync.Mutex
	direct map[string]*directPrinter
	lastAt time.Time
}

func (s *Server) loadNet() netSettings {
	var n netSettings
	if b, err := os.ReadFile(filepath.Join(s.dir, netFile)); err == nil {
		_ = json.Unmarshal(b, &n)
	}
	if n.CIDRs == nil {
		n.CIDRs = []string{}
	}
	if n.Direct.IntervalSecs == 0 {
		n.Direct.IntervalSecs = directDefaultSecs
	}
	return n
}

func (s *Server) getNetworkSettings(_ *http.Request, _ json.RawMessage) (any, error) {
	return map[string]any{"cidrs": s.loadNet().CIDRs, "direct": s.loadNet().Direct, "direct_available": true,
		"direct_status": s.directStatus(), "limits": map[string]int{"max_hosts": discover.MaxHosts, "min_prefix": discover.MinPrefix}}, nil
}

func (s *Server) setNetworkSettings(r *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		CIDRs  []string       `json:"cidrs"`
		Direct directSettings `json:"direct"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	var clean []string
	for _, c := range a.CIDRs {
		if c = strings.TrimSpace(c); c != "" {
			clean = append(clean, c)
		}
	}
	if len(clean) > 0 {
		// Se valida la forma (prefijo, IPv4, tope). El permiso para rangos públicos se pide en cada consulta.
		if _, err := discover.ParseCIDRs(clean, true); err != nil {
			return nil, bad(err.Error())
		}
	}
	if a.Direct.IntervalSecs == 0 {
		a.Direct.IntervalSecs = directDefaultSecs
	}
	if a.Direct.IntervalSecs < directMinSecs || a.Direct.IntervalSecs > directMaxSecs {
		return nil, bad("El intervalo debe estar entre " + strconv.Itoa(directMinSecs) + " y " + strconv.Itoa(directMaxSecs) + " segundos")
	}
	old := s.loadNet()
	n := netSettings{CIDRs: clean, Direct: a.Direct}
	b, _ := json.MarshalIndent(n, "", "  ")
	if err := writeAtomic(filepath.Join(s.dir, netFile), b); err != nil {
		return nil, err
	}
	if old.Direct.Enabled != n.Direct.Enabled {
		state := "desactivada"
		if n.Direct.Enabled {
			state = "activada"
		}
		s.logNotice("Detección de impresión directa " + state + " por " + who(r) + " (cada " + strconv.Itoa(n.Direct.IntervalSecs) + " s)")
	}
	return nil, nil
}

// --- descubrimiento ---

// DeviceView es un dispositivo encontrado y su relación con las colas controladas.
type DeviceView struct {
	discover.Device
	Queue       string `json:"queue"`       // cola del sistema que apunta a esta IP (vacío = ninguna)
	Managed     string `json:"managed"`     // controlada | cola_sin_control | sin_cola
	Explanation string `json:"explanation"` // en español, para el administrador
}

type discJob struct {
	Running  bool         `json:"running"`
	Started  int64        `json:"started"`
	Finished int64        `json:"finished"`
	By       string       `json:"by"`
	Ranges   []string     `json:"ranges"`
	Hosts    int          `json:"hosts"`
	Done     int          `json:"done"`
	Total    int          `json:"total"`
	Devices  []DeviceView `json:"devices"`
	Error    string       `json:"error"`
	cancel   context.CancelFunc
}

func (s *Server) discoverNetwork(r *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		CIDRs         []string `json:"cidrs"`
		ConfirmPublic bool     `json:"confirm_public"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	if len(a.CIDRs) == 0 {
		a.CIDRs = s.loadNet().CIDRs
	}
	hosts, err := discover.ParseCIDRs(a.CIDRs, a.ConfirmPublic)
	if err != nil {
		return nil, bad(err.Error())
	}
	var ranges []string
	for _, c := range a.CIDRs {
		if c = strings.TrimSpace(c); c != "" {
			ranges = append(ranges, c)
		}
	}
	by := who(r)
	s.net.mu.Lock()
	if s.net.job != nil && s.net.job.Running {
		s.net.mu.Unlock()
		return nil, bad("Ya hay un descubrimiento en curso")
	}
	ctx, cancel := context.WithTimeout(context.Background(), discoverMaxRunTime)
	job := &discJob{Running: true, Started: time.Now().Unix(), By: by, Ranges: ranges, Hosts: len(hosts), Total: len(hosts) * 3, Devices: []DeviceView{}, cancel: cancel}
	s.net.job = job
	s.net.mu.Unlock()
	s.logNotice(fmt.Sprintf("Descubrimiento de red iniciado por %s. Rangos: %s (%d hosts; solo TCP a %s)", by, strings.Join(ranges, ", "), len(hosts), portsText(s.net.ports)))
	go safely("descubrimiento", func() {
		defer func() { // si runDiscovery entró en pánico, el trabajo no debe quedar «en curso» para siempre
			s.net.mu.Lock()
			if job.Running {
				job.Running, job.Finished, job.Error = false, time.Now().Unix(), "error interno durante el descubrimiento"
			}
			s.net.mu.Unlock()
		}()
		s.runDiscovery(ctx, job, hosts)
	})
	return s.discoverStatus(nil, nil)
}

func portsText(p discover.Ports) string {
	if p == (discover.Ports{}) {
		p = discover.DefaultPorts
	}
	return fmt.Sprintf("%d/%d/%d", p.Raw, p.LPD, p.IPP)
}

func (s *Server) runDiscovery(ctx context.Context, job *discJob, hosts []netip.Addr) {
	defer job.cancel()
	sc := &discover.Scanner{Ports: s.net.ports, Dial: s.net.dial, Transport: s.net.tr, Progress: func(d, t int) {
		s.net.mu.Lock()
		job.Done, job.Total = d, t
		s.net.mu.Unlock()
	}}
	devs := sc.Scan(ctx, hosts)
	views := s.classify(devs)
	cancelled := ctx.Err() != nil
	s.net.mu.Lock()
	job.Devices, job.Running, job.Finished = views, false, time.Now().Unix()
	if cancelled {
		job.Error = "Descubrimiento interrumpido (cancelado o tiempo máximo): el resultado puede estar incompleto"
	}
	s.net.mu.Unlock()
	risk := 0
	for _, v := range views {
		if v.Managed == "sin_cola" {
			risk++
		}
	}
	s.logNotice(fmt.Sprintf("Descubrimiento de red terminado (iniciado por %s): %d dispositivos con puertos de impresión abiertos, %d sin cola en este servidor", job.By, len(views), risk))
}

func (s *Server) discoverStatus(_ *http.Request, _ json.RawMessage) (any, error) {
	s.net.mu.Lock()
	defer s.net.mu.Unlock()
	if s.net.job == nil {
		return map[string]any{"running": false, "devices": []DeviceView{}, "ranges": []string{}}, nil
	}
	j := *s.net.job
	j.cancel = nil
	return j, nil
}

func (s *Server) discoverCancel(_ *http.Request, _ json.RawMessage) (any, error) {
	s.net.mu.Lock()
	defer s.net.mu.Unlock()
	if s.net.job != nil && s.net.job.Running {
		s.net.job.cancel()
	}
	return nil, nil
}

// queueHosts devuelve, por IP, las colas del sistema cuyo dispositivo es de red (resolviendo nombres con tope corto).
func (s *Server) queueHosts() map[string][]string {
	out := map[string][]string{}
	names, err := cups.ListPrinters()
	if err != nil {
		return out
	}
	for _, n := range names {
		h := cups.NetHost(s.deviceURI(n))
		if h == "" {
			continue
		}
		ips := []string{h}
		if _, err := netip.ParseAddr(h); err != nil {
			ips = nil
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			addrs, _ := net.DefaultResolver.LookupNetIP(ctx, "ip4", h)
			cancel()
			for _, a := range addrs {
				ips = append(ips, a.String())
			}
		}
		for _, ip := range ips {
			out[ip] = append(out[ip], n)
		}
	}
	return out
}

func (s *Server) classify(devs []discover.Device) []DeviceView {
	byIP := s.queueHosts()
	cfg := s.config()
	out := make([]DeviceView, 0, len(devs))
	for _, d := range devs {
		v := DeviceView{Device: d}
		qs := byIP[d.IP]
		sort.Strings(qs)
		var ctl []string
		for _, q := range qs {
			if s.wantsHold(&cfg, q) {
				ctl = append(ctl, q)
			}
		}
		switch {
		case len(ctl) > 0:
			v.Queue, v.Managed = strings.Join(ctl, ", "), "controlada"
			v.Explanation = "Gestionada por la cola controlada «" + v.Queue + "». Aun así acepta conexiones directas por " + portList(d.Ports) + " de cualquier equipo que la alcance: restringe en la impresora o en el firewall que solo el servidor pueda conectarse."
		case len(qs) > 0:
			v.Queue, v.Managed = strings.Join(qs, ", "), "cola_sin_control"
			v.Explanation = "Existe la cola «" + v.Queue + "» hacia este dispositivo, pero el control está apagado para ella: los trabajos no se evalúan. Activa el control de la cola y restringe la impresión directa."
		default:
			v.Managed = "sin_cola"
			v.Explanation = "Ninguna cola de este servidor apunta a esta IP. Responde en " + portList(d.Ports) + ": cualquier equipo de la red puede imprimir en ella directamente, sin pasar por el control (riesgo de impresión directa)."
			if !d.IPP {
				v.Explanation += " No se pudo confirmar que sea una impresora (no respondió IPP): verifícalo."
			}
		}
		out = append(out, v)
	}
	return out
}

func portList(ps []int) string {
	s := make([]string, len(ps))
	for i, p := range ps {
		s[i] = strconv.Itoa(p)
	}
	return strings.Join(s, ", ")
}
