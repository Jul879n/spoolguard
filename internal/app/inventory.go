package app

import (
	"encoding/json"
	"net/http"
	"sync"
	"time"

	"controlimpresion/internal/cups"
)

// Inventario de impresoras y puertos. Mismo contenido que la categoría "inventario" de la copia de seguridad
// (InvItem lleva name/kind/uri de siempre y, opcionalmente, el resto). Solo se informa lo que CUPS entrega.

// routeCache recuerda el device-uri de cada cola unos segundos: el worker lo consulta por cada trabajo.
type routeCache struct {
	mu sync.Mutex
	m  map[string]routeEntry
}
type routeEntry struct {
	uri string
	at  time.Time
}

const routeTTL = 60 * time.Second

func (s *Server) deviceURI(printer string) string {
	s.routes.mu.Lock()
	if e, ok := s.routes.m[printer]; ok && time.Since(e.at) < routeTTL {
		s.routes.mu.Unlock()
		return e.uri
	}
	s.routes.mu.Unlock()
	uri := cups.DeviceURI(s.dir, printer)
	if uri != "" { // un fallo transitorio no debe borrar lo ya sabido
		s.routes.mu.Lock()
		if s.routes.m == nil {
			s.routes.m = map[string]routeEntry{}
		}
		s.routes.m[printer] = routeEntry{uri, time.Now()}
		s.routes.mu.Unlock()
	}
	return uri
}

// fillRoute rellena el puerto/URI de salida (sin usuario, contraseña ni consulta) y el tipo de conexión del evento.
// Vacío si la cola no informa device-uri: nunca se inventa.
func (s *Server) fillRoute(e *Event) {
	if e.Printer == "" || e.Printer == "—" {
		return
	}
	if uri := s.deviceURI(e.Printer); uri != "" {
		e.Port, e.ConnType = sanitizeURI(uri), cups.ConnType(uri)
	}
}

// InventoryRow es una fila del inventario (impresora o cola) con estructura estable.
type InventoryRow struct {
	Name          string `json:"name"`
	Driver        string `json:"driver"`    // modelo/driver que informa la cola
	URI           string `json:"uri"`       // device-uri o puerto
	Kind          string `json:"kind"`      // usb | red | compartida | otra
	ConnType      string `json:"conn_type"` // usb | socket | ipp | lpd | red | compartida | archivo | local | desconocido
	Host          string `json:"host"`      // IP/host del dispositivo si es de red
	Shared        *bool  `json:"shared"`    // nil = no informado
	Status        string `json:"status"`
	HoldSupported bool   `json:"hold_supported"` // retención soportada
	Controlled    bool   `json:"controlled"`     // control activo (regla propia, global o pausa rápida)
}

func (s *Server) inventory() ([]InventoryRow, error) {
	v, err := s.listPrinters(nil, nil)
	if err != nil {
		return nil, err
	}
	cfg := s.config()
	out := []InventoryRow{}
	for _, p := range v.([]printerView) {
		out = append(out, InventoryRow{Name: p.Name, Driver: p.Info.Driver, URI: sanitizeURI(p.Info.URI), Kind: p.Info.Kind, ConnType: p.Info.ConnType,
			Host: p.Info.Host, Shared: p.Info.Shared, Status: p.Info.Status, HoldSupported: p.Info.HoldSupported, Controlled: s.wantsHold(&cfg, p.Name)})
	}
	return out, nil
}

func (s *Server) getInventory(_ *http.Request, _ json.RawMessage) (any, error) {
	return s.inventory()
}

// invItems adapta el inventario a la categoría "inventario" de la copia de seguridad.
func (s *Server) invItems() ([]InvItem, error) {
	rows, err := s.inventory()
	if err != nil {
		return nil, err
	}
	out := make([]InvItem, len(rows))
	for i, r := range rows {
		hs, ctl := r.HoldSupported, r.Controlled
		out[i] = InvItem{Name: r.Name, Kind: r.Kind, URI: r.URI, Driver: r.Driver, ConnType: r.ConnType, Host: r.Host,
			Shared: r.Shared, Status: r.Status, HoldSupported: &hs, Controlled: &ctl}
	}
	return out, nil
}

func connTypeOf(uri string) string { return cups.ConnType(uri) }
