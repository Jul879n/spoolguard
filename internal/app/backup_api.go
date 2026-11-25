package app

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"controlimpresion/internal/cups"
	"controlimpresion/internal/rules"
)

// Copia de seguridad en el servidor: comandos export_backup, preview_import y apply_import (solo administrador
// con sesión; el cuerpo admite hasta backupBodyMax y el archivo hasta backupMaxBytes).
const (
	appVersion    = "0.1.0"
	backupBodyMax = 12 << 20
)

// snapshot toma la lista blanca de campos del estado actual (nunca contraseña, tokens ni sesión).
func (s *Server) snapshot() Snapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	c := s.cfg.clone()
	snap := Snapshot{Printers: map[string]rules.Rules{}, Global: c.Global, Quick: cloneQuick(s.quick)}
	for k, r := range c.Printers {
		snap.Printers[k] = *r
	}
	st := c.Settings
	snap.Params = Params{UnknownPolicy: st.UnknownPolicy, IntervalSecs: st.IntervalSecs, NotifyBlock: st.NotifyBlock,
		NotifyAllow: st.NotifyAllow, Theme: st.Theme, RetentionDays: st.RetentionDays}
	return snap.clone()
}

func (s *Server) localPrinters() []string {
	names, _ := cups.ListPrinters()
	return names
}

type exportArgs struct {
	Categories []string `json:"categories"`
	From       *int64   `json:"from"`
	To         *int64   `json:"to"`
	MaxEvents  int      `json:"max_events"`
}

// buildBackup arma el archivo de copia (también lo usa export_rules con solo reglas y parámetros).
func (s *Server) buildBackup(a exportArgs) (map[string]any, error) {
	if len(a.Categories) == 0 {
		return nil, bad("Elige al menos una categoría")
	}
	for _, c := range a.Categories {
		if !slices.Contains(backupCats, c) {
			return nil, bad("Categoría desconocida: " + clipRunes(c, 40))
		}
	}
	var evs []Event
	if slices.Contains(a.Categories, "registro") {
		n := a.MaxEvents
		if n <= 0 {
			n = backupDefEvents
		}
		n = min(n, backupMaxEvents)
		rows, _, err := s.events.query(EventFilter{From: a.From, To: a.To}, n, 0)
		if err != nil {
			return nil, err
		}
		slices.Reverse(rows) // más antiguo primero
		evs = rows
	}
	var inv []InvItem
	if slices.Contains(a.Categories, "inventario") {
		var err error
		if inv, err = s.invItems(); err != nil {
			return nil, err
		}
	}
	return exportBackup(s.snapshot(), a.Categories, evs, inv, BackupMeta{Created: int64(now()), Host: hostName, AppVersion: appVersion}), nil
}

func (s *Server) exportBackup(r *http.Request, args json.RawMessage) (any, error) {
	var a exportArgs
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	f, err := s.buildBackup(a)
	if err != nil {
		return nil, err
	}
	b, _ := json.MarshalIndent(f, "", "  ")
	s.logNotice("Exportación de copia de seguridad por " + who(r) + ". Categorías: " + strings.Join(a.Categories, ", "))
	return string(b), nil
}

type importArgs struct {
	JSON string `json:"json"`
	Modo any    `json:"modo"`
}

func (s *Server) planFrom(a importArgs) (Plan, *Parsed, Modes, error) {
	var zero Plan
	m, err := parseModes(a.Modo)
	if err != nil {
		return zero, nil, m, bad(err.Error())
	}
	p, err := parseBackup([]byte(a.JSON), int64(now()))
	if err != nil {
		return zero, nil, m, bad(err.Error())
	}
	ctx := BackupCtx{Local: s.localPrinters(), ExistingIDs: map[string]bool{}, EventCount: s.events.count(), Server: true}
	if p.HasEvents {
		ids := make([]string, len(p.Events))
		for i, e := range p.Events {
			ids[i] = e.ID
		}
		if ctx.ExistingIDs, err = s.events.existingIDs(ids); err != nil {
			return zero, nil, m, err
		}
	}
	return planImport(s.snapshot(), p, m, ctx), p, m, nil
}

func (s *Server) previewImport(_ *http.Request, args json.RawMessage) (any, error) {
	var a importArgs
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	pl, _, _, err := s.planFrom(a)
	if err != nil {
		return nil, err
	}
	return pl.Preview, nil
}

func copyIfExists(src, dst string) error {
	b, err := os.ReadFile(src)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	return writeAtomic(dst, b)
}

func (s *Server) applyImport(r *http.Request, args json.RawMessage) (any, error) {
	var a importArgs
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	return s.applyBackup(a, who(r))
}

// applyBackup aplica la importación de forma atómica: primero guarda config.json.bak (y quick.json.bak) y,
// si algo falla, deja la configuración como estaba.
func (s *Server) applyBackup(a importArgs, actor string) (any, error) {
	pl, p, m, err := s.planFrom(a)
	if err != nil {
		return nil, err
	}
	if !pl.OK {
		return nil, bad("El archivo tiene errores y no se aplicó nada: " + strings.Join(firstN(pl.Preview["errores"], 3), "; "))
	}
	if len(pl.Applied) == 0 {
		return nil, bad("No hay nada que importar con las opciones elegidas")
	}
	_ = p
	s.mu.Lock()
	prevCfg, prevQuick := s.cfg.clone(), cloneQuick(s.quick)
	prevQuick.Version = s.quick.Version
	for _, f := range [][2]string{{"config.json", "config.json.bak"}, {"quick.json", "quick.json.bak"}} {
		if err := copyIfExists(filepath.Join(s.dir, f[0]), filepath.Join(s.dir, f[1])); err != nil {
			s.mu.Unlock()
			return nil, fmt.Errorf("no se pudo guardar la copia previa: %w", err)
		}
	}
	// Deshacer: restaura memoria y archivos sin pasar por saveLocked (que sobrescribiría el .bak con lo importado).
	rollback := func() {
		s.cfg, s.quick = prevCfg, prevQuick
		b, _ := json.MarshalIndent(s.cfg, "", "  ")
		_ = writeAtomic(filepath.Join(s.dir, "config.json"), b)
		_ = s.saveQuickLocked()
	}
	if slices.Contains(pl.Applied, "reglas") || slices.Contains(pl.Applied, "parametros") {
		next := pl.Next
		s.cfg.Printers = map[string]*rules.Rules{}
		for k, r := range next.Printers {
			r := r
			s.cfg.Printers[k] = &r
		}
		s.cfg.Global = next.Global
		st := &s.cfg.Settings // solo la lista blanca: contraseña y demás no se tocan
		st.UnknownPolicy, st.IntervalSecs, st.NotifyBlock = next.Params.UnknownPolicy, next.Params.IntervalSecs, next.Params.NotifyBlock
		st.NotifyAllow, st.Theme, st.RetentionDays = next.Params.NotifyAllow, next.Params.Theme, next.Params.RetentionDays
		if err := s.saveLocked(); err != nil {
			rollback()
			s.mu.Unlock()
			return nil, err
		}
	}
	if slices.Contains(pl.Applied, "bloqueo_rapido") {
		s.quick.Blocks, s.quick.Paused = pl.Next.Quick.Blocks, pl.Next.Quick.Paused
		if err := s.saveQuickLocked(); err != nil {
			rollback()
			s.mu.Unlock()
			return nil, err
		}
	}
	s.mu.Unlock()
	imported := 0
	if slices.Contains(pl.Applied, "registro") {
		if imported, err = s.events.importEvents(pl.Events, pl.ReplaceEvents); err != nil {
			s.mu.Lock()
			rollback()
			s.mu.Unlock()
			return nil, fmt.Errorf("no se pudo importar el registro (la configuración quedó como estaba): %w", err)
		}
	}
	// Retención: reaplica según las reglas nuevas, solo en las colas que existen aquí.
	cfg := s.config()
	failed := s.syncHolds(&cfg, s.localPrinters())
	mode := func(o op) string { return o.String() }
	s.logNotice(fmt.Sprintf("Importación de copia de seguridad por %s. Categorías: %s. Modo: reglas %s, parámetros %s, bloqueo rápido %s, registro %s (%d eventos)",
		actor, strings.Join(pl.Applied, ", "), mode(m.Reglas), mode(m.Parametros), mode(m.BloqueoRapido), mode(m.Registro), imported))
	return map[string]any{"warning": holdWarning(failed), "avisos": pl.Preview["avisos"], "aplicado": pl.Applied, "eventos_importados": imported, "preview": pl.Preview}, nil
}

// firstN devuelve hasta max textos de un valor del preview.
func firstN(v any, max int) []string {
	out, _ := v.([]string)
	return out[:min(len(out), max)]
}

// export_rules / import_rules: compatibilidad con la interfaz anterior. Usan el formato y la validación
// nuevos, restringidos a reglas y parámetros (nunca contraseña ni tokens).
func (s *Server) exportRules(r *http.Request, _ json.RawMessage) (any, error) {
	f, err := s.buildBackup(exportArgs{Categories: []string{"reglas", "parametros"}})
	if err != nil {
		return nil, err
	}
	b, _ := json.MarshalIndent(f, "", "  ")
	return string(b), nil
}

func (s *Server) importRules(r *http.Request, args json.RawMessage) (any, error) {
	var a importArgs
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	a.Modo = map[string]any{"reglas": "reemplazar", "parametros": "reemplazar"}
	res, err := s.applyBackup(a, who(r))
	if err != nil {
		return nil, err
	}
	return res.(map[string]any)["warning"], nil
}
