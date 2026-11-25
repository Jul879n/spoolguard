package app

import (
	"bytes"
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	_ "modernc.org/sqlite" // SQLite sin CGO
)

// Registro de eventos central (events.db): los eventos del worker CUPS y los que envían los equipos
// de escritorio. Mismo esquema y mismos límites que core/src/events.rs. Todo SQL usa parámetros
// enlazados; los únicos fragmentos de SQL armados dinámicamente son constantes de este archivo.

const (
	eventsSchemaVersion = 1
	ingestMaxEvents     = 500
	ingestMaxBody       = 1 << 20 // 1 MiB
	ingestPerMin        = 60      // peticiones por token y minuto
	maxID               = 64
	maxShort            = 255
	maxDoc              = 1000
	maxReason           = 500
	defaultRetention    = 365
)

type Event struct {
	ID       string `json:"id"`
	TS       int64  `json:"ts"`
	Host     string `json:"host"`
	IP       string `json:"ip"`
	User     string `json:"user"`
	Printer  string `json:"printer"`
	Port     string `json:"port"`
	ConnType string `json:"conn_type"`
	Document string `json:"document"`
	Site     string `json:"site"`
	App      string `json:"app"`
	Pages    *int64 `json:"pages"`
	Bytes    *int64 `json:"bytes"`
	Decision string `json:"decision"` // allow | block | notice
	Reason   string `json:"reason"`
	Status   string `json:"status"` // liberado | cancelado | aviso
	Source   string `json:"source"` // desktop | server
	// Device es la etiqueta del token de equipo que envió el evento (la pone el servidor, no el cliente).
	Device string `json:"device,omitempty"`
}

type EventFilter struct {
	From     *int64 `json:"from"`
	To       *int64 `json:"to"`
	User     string `json:"user"`
	Printer  string `json:"printer"`
	Host     string `json:"host"`
	Decision string `json:"decision"` // ok | bad | allow | block | notice
	Text     string `json:"text"`
}

type eventStore struct {
	db  *sql.DB
	dir string
}

// newEventID: UUID v7 (ordenable por hora).
func newEventID(ms int64) string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(err)
	}
	var t [8]byte
	binary.BigEndian.PutUint64(t[:], uint64(ms))
	copy(b[:6], t[2:])
	b[6] = 0x70 | b[6]&0x0f
	b[8] = 0x80 | b[8]&0x3f
	h := hex.EncodeToString(b[:])
	return h[:8] + "-" + h[8:12] + "-" + h[12:16] + "-" + h[16:20] + "-" + h[20:]
}

func openEvents(dir string) (*eventStore, error) {
	path := filepath.Join(dir, "events.db")
	// Se crea antes con 0600: SQLite crea -wal y -shm con los permisos del archivo principal.
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, err
	}
	f.Close()
	if err := os.Chmod(path, 0o600); err != nil {
		return nil, err
	}
	u := url.URL{Path: filepath.ToSlash(path)}
	dsn := "file:" + u.EscapedPath() + "?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)&_pragma=synchronous(NORMAL)&_pragma=foreign_keys(1)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(4)
	st := &eventStore{db: db, dir: dir}
	if err := st.migrate(); err != nil {
		db.Close()
		return nil, err
	}
	if err := st.importJSONL(); err != nil {
		db.Close()
		return nil, err
	}
	return st, nil
}

func (st *eventStore) Close() error { return st.db.Close() }

func (st *eventStore) migrate() error {
	var v int
	if err := st.db.QueryRow("PRAGMA user_version").Scan(&v); err != nil {
		return err
	}
	if v > eventsSchemaVersion {
		return fmt.Errorf("events.db es de una versión más nueva (%d); actualiza el servidor", v)
	}
	if v < 1 {
		_, err := st.db.Exec(`
CREATE TABLE IF NOT EXISTS events (
  id TEXT PRIMARY KEY,
  ts INTEGER NOT NULL,
  host TEXT NOT NULL DEFAULT '',
  ip TEXT NOT NULL DEFAULT '',
  user TEXT NOT NULL DEFAULT '',
  printer TEXT NOT NULL DEFAULT '',
  port TEXT NOT NULL DEFAULT '',
  conn_type TEXT NOT NULL DEFAULT '',
  document TEXT NOT NULL DEFAULT '',
  site TEXT NOT NULL DEFAULT '',
  app TEXT NOT NULL DEFAULT '',
  pages INTEGER,
  bytes INTEGER,
  decision TEXT NOT NULL DEFAULT 'notice',
  reason TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT '',
  source TEXT NOT NULL DEFAULT 'server',
  device TEXT NOT NULL DEFAULT '',
  recv INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS idx_events_user ON events(user, ts);
CREATE INDEX IF NOT EXISTS idx_events_printer ON events(printer, ts);
CREATE INDEX IF NOT EXISTS idx_events_decision ON events(decision, ts);
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
PRAGMA user_version = 1;`)
		return err
	}
	return nil
}

const evCols = "id, ts, host, ip, user, printer, port, conn_type, document, site, app, pages, bytes, decision, reason, status, source, device"

type execer interface {
	ExecContext(ctx context.Context, q string, args ...any) (sql.Result, error)
}

// insertEvent es idempotente por id (el primero que llega se conserva).
func insertEvent(x execer, e *Event) (inserted bool, err error) {
	res, err := x.ExecContext(context.Background(),
		"INSERT INTO events ("+evCols+", recv) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING",
		e.ID, e.TS, e.Host, e.IP, e.User, e.Printer, e.Port, e.ConnType, e.Document, e.Site, e.App, e.Pages, e.Bytes,
		e.Decision, e.Reason, e.Status, e.Source, e.Device, time.Now().Unix())
	if err != nil {
		return false, err
	}
	n, _ := res.RowsAffected()
	return n > 0, nil
}

func (st *eventStore) add(e Event) error {
	clampEvent(&e)
	_, err := insertEvent(st.db, &e)
	return err
}

func clip(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	return string([]rune(s)[:n])
}

// clampEvent recorta los textos propios (los eventos de equipos se validan, no se recortan).
func clampEvent(e *Event) {
	e.ID = clip(e.ID, maxID)
	for _, f := range []*string{&e.Host, &e.IP, &e.User, &e.Printer, &e.Port, &e.ConnType, &e.Site, &e.App, &e.Status, &e.Source, &e.Device} {
		*f = clip(*f, maxShort)
	}
	e.Document = clip(e.Document, maxDoc)
	e.Reason = clip(e.Reason, maxReason)
	switch e.Decision {
	case "allow", "block", "notice":
	default:
		e.Decision = "notice"
	}
}

func okText(s string, max int) bool {
	return utf8.ValidString(s) && utf8.RuneCountInString(s) <= max && !strings.ContainsRune(s, 0)
}

func okID(s string) bool {
	if len(s) < 8 || len(s) > maxID {
		return false
	}
	for _, c := range s {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_') {
			return false
		}
	}
	return true
}

// validateIngest comprueba un evento recibido de un equipo: debe cumplir los límites (no se recorta).
func validateIngest(e *Event, nowSecs int64) error {
	if !okID(e.ID) {
		return errors.New("id inválido")
	}
	if e.TS < 1_000_000_000 || e.TS > nowSecs+86400 {
		return errors.New("hora fuera de rango")
	}
	for _, f := range []string{e.Host, e.IP, e.User, e.Printer, e.Port, e.ConnType, e.Site, e.App, e.Status} {
		if !okText(f, maxShort) {
			return errors.New("campo demasiado largo o inválido")
		}
	}
	if !okText(e.Document, maxDoc) || !okText(e.Reason, maxReason) {
		return errors.New("campo demasiado largo o inválido")
	}
	switch e.Decision {
	case "allow", "block", "notice":
	default:
		return errors.New("decisión inválida")
	}
	for _, n := range []*int64{e.Pages, e.Bytes} {
		if n != nil && (*n < 0 || *n > 1<<50) {
			return errors.New("número fuera de rango")
		}
	}
	return nil
}

// ingest guarda un lote en una transacción. Devuelve los ids aceptados (incluidos los que ya existían)
// y los rechazados por inválidos. device es la etiqueta del token (no la decide el cliente).
func (st *eventStore) ingest(device string, evs []Event) (accepted, rejected []string, err error) {
	accepted, rejected = []string{}, []string{}
	tx, err := st.db.BeginTx(context.Background(), nil)
	if err != nil {
		return nil, nil, err
	}
	defer tx.Rollback()
	n := time.Now().Unix()
	for i := range evs {
		e := &evs[i]
		if validateIngest(e, n) != nil {
			if okID(e.ID) {
				rejected = append(rejected, e.ID)
			}
			continue
		}
		e.Source, e.Device = "desktop", clip(device, maxShort)
		if _, err := insertEvent(tx, e); err != nil {
			return nil, nil, err
		}
		accepted = append(accepted, e.ID)
	}
	return accepted, rejected, tx.Commit()
}

func likeEscape(s string) string {
	r := strings.NewReplacer(`\`, `\\`, `%`, `\%`, `_`, `\_`)
	return "%" + r.Replace(s) + "%"
}

func (f EventFilter) where() (string, []any) {
	var c []string
	var a []any
	if f.From != nil {
		c, a = append(c, "ts >= ?"), append(a, *f.From)
	}
	if f.To != nil {
		c, a = append(c, "ts <= ?"), append(a, *f.To)
	}
	if f.User != "" {
		c, a = append(c, "user = ?"), append(a, f.User)
	}
	if f.Printer != "" {
		c, a = append(c, "printer = ?"), append(a, f.Printer)
	}
	if f.Host != "" {
		c, a = append(c, "(host = ? OR device = ?)"), append(a, f.Host, f.Host)
	}
	switch f.Decision {
	case "", "all":
	case "ok":
		c = append(c, "decision <> 'block'")
	case "bad":
		c = append(c, "decision = 'block'")
	default:
		c, a = append(c, "decision = ?"), append(a, f.Decision)
	}
	if t := strings.TrimSpace(f.Text); t != "" {
		like := likeEscape(t)
		var parts []string
		for _, col := range []string{"document", "user", "printer", "host", "device", "ip", "site", "app", "reason"} {
			parts = append(parts, col+` LIKE ? ESCAPE '\'`)
			a = append(a, like)
		}
		c = append(c, "("+strings.Join(parts, " OR ")+")")
	}
	if len(c) == 0 {
		return "", nil
	}
	return " WHERE " + strings.Join(c, " AND "), a
}

func scanEvent(r interface{ Scan(...any) error }) (Event, error) {
	var e Event
	err := r.Scan(&e.ID, &e.TS, &e.Host, &e.IP, &e.User, &e.Printer, &e.Port, &e.ConnType, &e.Document, &e.Site, &e.App,
		&e.Pages, &e.Bytes, &e.Decision, &e.Reason, &e.Status, &e.Source, &e.Device)
	return e, err
}

// query devuelve una página (del más reciente al más antiguo) y el total que cumple el filtro.
func (st *eventStore) query(f EventFilter, limit, offset int) ([]Event, int64, error) {
	w, a := f.where()
	var total int64
	if err := st.db.QueryRow("SELECT COUNT(*) FROM events"+w, a...).Scan(&total); err != nil {
		return nil, 0, err
	}
	rows, err := st.db.Query("SELECT "+evCols+" FROM events"+w+" ORDER BY ts DESC, id DESC LIMIT ? OFFSET ?", append(a, limit, offset)...)
	if err != nil {
		return nil, 0, err
	}
	defer rows.Close()
	out := []Event{}
	for rows.Next() {
		e, err := scanEvent(rows)
		if err != nil {
			return nil, 0, err
		}
		out = append(out, e)
	}
	return out, total, rows.Err()
}

func (st *eventStore) printers() []string {
	rows, err := st.db.Query("SELECT DISTINCT printer FROM events WHERE printer <> '' ORDER BY printer LIMIT 500")
	if err != nil {
		return []string{}
	}
	defer rows.Close()
	out := []string{}
	for rows.Next() {
		var p string
		if rows.Scan(&p) == nil {
			out = append(out, p)
		}
	}
	return out
}

func (st *eventStore) count() (n int64) {
	_ = st.db.QueryRow("SELECT COUNT(*) FROM events").Scan(&n)
	return
}

// purge borra lo anterior a days días (0 = sin límite). Devuelve cuántos.
func (st *eventStore) purge(days int64) (int64, error) {
	if days <= 0 {
		return 0, nil
	}
	return st.purgeBefore(time.Now().Unix() - days*86400)
}

func (st *eventStore) purgeBefore(ts int64) (int64, error) {
	res, err := st.db.Exec("DELETE FROM events WHERE ts < ?", ts)
	if err != nil {
		return 0, err
	}
	return res.RowsAffected()
}

func (st *eventStore) clear() error {
	_, err := st.db.Exec("DELETE FROM events")
	return err
}

// csvCell neutraliza las fórmulas de hoja de cálculo (= + - @ al inicio).
func csvCell(v string) string {
	p := ""
	if v != "" && strings.ContainsRune("=+-@\t\r", rune(v[0])) {
		p = "'"
	}
	return `"` + p + strings.ReplaceAll(v, `"`, `""`) + `"`
}

// csvMaxRows acota la memoria de una exportación (el CSV se arma en memoria y se responde de una vez).
const csvMaxRows = 200_000

func (st *eventStore) csv(f EventFilter) (string, error) {
	w, a := f.where()
	rows, err := st.db.Query("SELECT "+evCols+" FROM events"+w+" ORDER BY ts DESC, id DESC", a...)
	if err != nil {
		return "", err
	}
	defer rows.Close()
	var b strings.Builder
	b.WriteString("hora_unix,estado,documento,impresora,usuario,origen,motivo,equipo,puerto,tipo_conexion,sitio,aplicacion,paginas,bytes,decision,estado_final,origen_evento,id\n")
	num := func(p *int64) string {
		if p == nil {
			return ""
		}
		return strconv.FormatInt(*p, 10)
	}
	n := 0
	for rows.Next() {
		if n++; n > csvMaxRows {
			return "", fmt.Errorf("demasiados eventos para exportar de una vez (máx. %d): acota el filtro por fecha, usuario o impresora", csvMaxRows)
		}
		e, err := scanEvent(rows)
		if err != nil {
			return "", err
		}
		st := "permitido"
		if e.Decision == "block" {
			st = "bloqueado"
		}
		equipo := e.Device
		if equipo == "" {
			equipo = e.Host
		}
		fmt.Fprintf(&b, "%d,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n", e.TS, st, csvCell(e.Document), csvCell(e.Printer),
			csvCell(e.User), csvCell(e.IP), csvCell(e.Reason), csvCell(equipo), csvCell(e.Port), csvCell(e.ConnType), csvCell(e.Site),
			csvCell(e.App), num(e.Pages), num(e.Bytes), e.Decision, csvCell(e.Status), e.Source, e.ID)
	}
	return b.String(), rows.Err()
}

// SummaryRow agrupa los trabajos por una clave (usuario, equipo, impresora o decisión).
type SummaryRow struct {
	Key       string `json:"key"`
	Jobs      int64  `json:"jobs"`
	Allowed   int64  `json:"allowed"`
	Blocked   int64  `json:"blocked"`
	Pages     *int64 `json:"pages"`      // suma de las páginas conocidas; nil si ningún trabajo las informó
	PagesJobs int64  `json:"pages_jobs"` // trabajos con páginas conocidas (el total es parcial si es menor que jobs)
}

// Expresiones SQL constantes por agrupación (nunca se arma SQL con texto del cliente).
var summaryExpr = map[string]string{
	"user":     "user",
	"host":     "CASE WHEN device <> '' THEN device ELSE host END",
	"printer":  "printer",
	"decision": "decision",
}

// summarize agrupa los trabajos (los avisos del sistema no cuentan) que cumplen el filtro.
func (st *eventStore) summarize(f EventFilter, by string) ([]SummaryRow, error) {
	expr, ok := summaryExpr[by]
	if !ok {
		return nil, fmt.Errorf("agrupación desconocida")
	}
	w, a := f.where()
	if w == "" {
		w = " WHERE decision <> 'notice'"
	} else {
		w += " AND decision <> 'notice'"
	}
	q := "SELECT " + expr + " AS k, COUNT(*), SUM(decision <> 'block'), SUM(decision = 'block'), SUM(pages), COUNT(pages) FROM events" + w +
		" GROUP BY k ORDER BY COUNT(*) DESC, k LIMIT 1000"
	rows, err := st.db.Query(q, a...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []SummaryRow{}
	for rows.Next() {
		var r SummaryRow
		if err := rows.Scan(&r.Key, &r.Jobs, &r.Allowed, &r.Blocked, &r.Pages, &r.PagesJobs); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// --- migración desde log.jsonl ---

// importJSONL renombra log.jsonl a log.jsonl.migrado (nunca lo borra) e importa todo log.jsonl.migrado*
// que aún no se haya importado. Idempotente aunque el proceso se interrumpa a medias.
func (st *eventStore) importJSONL() error {
	src := filepath.Join(st.dir, "log.jsonl")
	if fi, err := os.Stat(src); err == nil && fi.Mode().IsRegular() {
		dst := filepath.Join(st.dir, "log.jsonl.migrado")
		for n := 0; ; n++ {
			if _, err := os.Lstat(dst); err != nil {
				break
			}
			dst = filepath.Join(st.dir, fmt.Sprintf("log.jsonl.migrado-%d-%d", time.Now().Unix(), n))
		}
		if err := os.Rename(src, dst); err != nil {
			return err
		}
	}
	ents, err := os.ReadDir(st.dir)
	if err != nil {
		return err
	}
	var names []string
	for _, e := range ents {
		if strings.HasPrefix(e.Name(), "log.jsonl.migrado") {
			names = append(names, e.Name())
		}
	}
	sort.Strings(names)
	for _, n := range names {
		if err := st.importFile(n); err != nil {
			return err
		}
	}
	return nil
}

func (st *eventStore) importFile(name string) error {
	key := "imported:" + name
	var have string
	if err := st.db.QueryRow("SELECT v FROM meta WHERE k = ?", key).Scan(&have); err == nil {
		return nil
	} else if !errors.Is(err, sql.ErrNoRows) {
		return err
	}
	b, err := os.ReadFile(filepath.Join(st.dir, name))
	if err != nil {
		return err
	}
	tx, err := st.db.BeginTx(context.Background(), nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	host, _ := os.Hostname()
	n := 0
	for _, l := range bytes.Split(b, []byte("\n")) {
		var a Activity
		if len(bytes.TrimSpace(l)) == 0 || json.Unmarshal(l, &a) != nil {
			continue
		}
		e := activityEvent(a, host)
		e.ID = newEventID(int64(a.Time) * 1000)
		clampEvent(&e)
		if _, err := insertEvent(tx, &e); err != nil {
			return err
		}
		n++
	}
	if _, err := tx.Exec("INSERT OR REPLACE INTO meta (k, v) VALUES (?, ?)", key, strconv.Itoa(n)); err != nil {
		return err
	}
	return tx.Commit()
}

// activityEvent convierte una línea del formato antiguo en un evento del servidor.
func activityEvent(a Activity, host string) Event {
	e := Event{TS: int64(a.Time), Host: host, Printer: a.Printer, Document: a.Document, Reason: a.Reason, Source: "server"}
	if a.User != nil {
		e.User = *a.User
	}
	if a.Origin != nil {
		e.IP = *a.Origin
	}
	switch {
	case a.Reason == "Aviso del sistema":
		e.Decision, e.Status = "notice", "aviso"
	case a.Allowed:
		e.Decision, e.Status = "allow", "liberado"
	default:
		e.Decision, e.Status = "block", "cancelado"
	}
	return e
}

// retentionDays lee CONTROL_LOG_RETENTION_DAYS: por defecto 365; 0 = sin límite; inválido = 365.
func retentionDays() int64 {
	v := strings.TrimSpace(os.Getenv("CONTROL_LOG_RETENTION_DAYS"))
	if v == "" {
		return defaultRetention
	}
	n, err := strconv.ParseInt(v, 10, 64)
	if err != nil || n < 0 || n > 36500 {
		return defaultRetention
	}
	return n
}

// --- API de administrador (con sesión) ---

type eventRow struct {
	ID       string `json:"id"`
	Time     int64  `json:"time"`
	Printer  string `json:"printer"`
	Document string `json:"document"`
	User     string `json:"user"`
	Origin   string `json:"origin"`
	Allowed  bool   `json:"allowed"`
	Reason   string `json:"reason"`
	Host     string `json:"host"`
	Device   string `json:"device"`
	Port     string `json:"port"`
	ConnType string `json:"conn_type"`
	Site     string `json:"site"`
	App      string `json:"app"`
	Pages    *int64 `json:"pages"`
	Bytes    *int64 `json:"bytes"`
	Decision string `json:"decision"`
	Status   string `json:"status"`
	Source   string `json:"source"`
}

func rowOf(e Event) eventRow {
	return eventRow{ID: e.ID, Time: e.TS, Printer: e.Printer, Document: e.Document, User: e.User, Origin: e.IP, Allowed: e.Decision != "block",
		Reason: e.Reason, Host: e.Host, Device: e.Device, Port: e.Port, ConnType: e.ConnType, Site: e.Site, App: e.App, Pages: e.Pages,
		Bytes: e.Bytes, Decision: e.Decision, Status: e.Status, Source: e.Source}
}

func (s *Server) queryEvents(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Filter EventFilter `json:"filter"`
		Page   int         `json:"page"`
		Size   int         `json:"size"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	if a.Size == 0 {
		a.Size = 25
	}
	a.Size = min(max(a.Size, 1), 500)
	a.Page = max(a.Page, 1)
	rows, total, err := s.events.query(a.Filter, a.Size, (a.Page-1)*a.Size)
	if err != nil {
		return nil, err
	}
	out := make([]eventRow, len(rows))
	for i, e := range rows {
		out[i] = rowOf(e)
	}
	return map[string]any{"rows": out, "total": total, "printers": s.events.printers()}, nil
}

func (s *Server) summarizeEvents(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Filter EventFilter `json:"filter"`
		By     string      `json:"by"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	rows, err := s.events.summarize(a.Filter, a.By)
	if err != nil {
		return nil, bad(err.Error())
	}
	return rows, nil
}

func (s *Server) exportLogCSV(_ *http.Request, args json.RawMessage) (any, error) {
	var a struct {
		Filter EventFilter `json:"filter"`
	}
	if err := decode(args, &a); err != nil {
		return nil, err
	}
	return s.events.csv(a.Filter)
}

// --- POST /api/events/ingest (token de equipo) ---

func (s *Server) eventsIngest(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		w.Header().Set("Allow", "POST")
		writeJSON(w, http.StatusMethodNotAllowed, map[string]string{"error": "Método no permitido"})
		return
	}
	t, ok := s.ext.lookupKind(extBearer(r), kindDevice)
	if !ok {
		w.Header().Set("WWW-Authenticate", "Bearer")
		writeJSON(w, http.StatusUnauthorized, map[string]string{"error": "Token inválido o revocado"})
		return
	}
	if !s.ext.allow(t.ID, "ingest", ingestPerMin) {
		w.Header().Set("Retry-After", "60")
		writeJSON(w, http.StatusTooManyRequests, map[string]string{"error": "Demasiadas peticiones"})
		return
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, ingestMaxBody))
	if err != nil {
		writeJSON(w, http.StatusRequestEntityTooLarge, map[string]string{"error": "Lote demasiado grande (máx. 1 MiB)"})
		return
	}
	var in struct {
		Events []Event `json:"events"`
	}
	if json.Unmarshal(body, &in) != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "lote inválido"})
		return
	}
	if len(in.Events) > ingestMaxEvents {
		writeJSON(w, http.StatusRequestEntityTooLarge, map[string]string{"error": "Demasiados eventos (máx. " + strconv.Itoa(ingestMaxEvents) + ")"})
		return
	}
	acc, rej, err := s.events.ingest(t.Name, in.Events)
	if err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "no se pudo guardar el lote"})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"accepted": acc, "rejected": rej})
}

// --- registro propio del servidor ---

var hostName = func() string { h, _ := os.Hostname(); return h }()

// appendEvent guarda un evento del propio servidor (worker CUPS).
func (s *Server) appendEvent(e Event) {
	e.ID, e.TS, e.Host, e.Source = newEventID(time.Now().UnixMilli()), int64(now()), hostName, "server"
	if err := s.events.add(e); err != nil {
		fmt.Fprintln(os.Stderr, "control-server: no se pudo registrar el evento:", err)
	}
}

func (s *Server) appendLog(a Activity) {
	e := activityEvent(a, hostName)
	s.appendEvent(e)
}

func (s *Server) logNotice(text string) {
	s.appendLog(Activity{Time: now(), Printer: "—", Document: text, Allowed: true, Reason: "Aviso del sistema"})
}

// activity devuelve los últimos eventos, el más antiguo primero (compatibilidad con get_activity).
func (s *Server) activity() []Activity {
	rows, _, err := s.events.query(EventFilter{}, 2000, 0)
	out := make([]Activity, 0, len(rows))
	if err != nil {
		return out
	}
	for i := len(rows) - 1; i >= 0; i-- {
		e := rows[i]
		a := Activity{Time: uint64(e.TS), Printer: e.Printer, Document: e.Document, Allowed: e.Decision != "block", Reason: e.Reason}
		if e.User != "" {
			u := e.User
			a.User = &u
		}
		if e.IP != "" {
			o := e.IP
			a.Origin = &o
		}
		out = append(out, a)
	}
	return out
}

func (s *Server) clearLog(_ *http.Request, _ json.RawMessage) (any, error) {
	return nil, s.events.clear()
}

// purgeEvents aplica la retención al arrancar y cada hora.
func (s *Server) purgeEvents() {
	if n, err := s.events.purge(s.retention); err != nil {
		fmt.Fprintln(os.Stderr, "control-server: limpieza del registro:", err)
	} else if n > 0 {
		fmt.Fprintf(os.Stderr, "control-server: retención: %d eventos antiguos eliminados\n", n)
	}
}

func deref(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

// existingIDs devuelve cuáles de los ids ya están en el registro.
func (st *eventStore) existingIDs(ids []string) (map[string]bool, error) {
	out := map[string]bool{}
	for i := 0; i < len(ids); i += 500 {
		chunk := ids[i:min(i+500, len(ids))]
		args := make([]any, len(chunk))
		for j, id := range chunk {
			args[j] = id
		}
		rows, err := st.db.Query("SELECT id FROM events WHERE id IN (?"+strings.Repeat(",?", len(chunk)-1)+")", args...)
		if err != nil {
			return nil, err
		}
		for rows.Next() {
			var id string
			if rows.Scan(&id) == nil {
				out[id] = true
			}
		}
		rows.Close()
	}
	return out, nil
}

// importEvents inserta eventos ya validados con sus ids (idempotente: los existentes no se repiten) en una
// sola transacción; con replace borra antes el registro actual. Todo o nada.
func (st *eventStore) importEvents(evs []Event, replace bool) (int, error) {
	tx, err := st.db.Begin()
	if err != nil {
		return 0, err
	}
	defer tx.Rollback()
	if replace {
		if _, err := tx.Exec("DELETE FROM events"); err != nil {
			return 0, err
		}
	}
	n := 0
	for i := range evs {
		e := evs[i]
		e.Device = ""
		ok, err := insertEvent(tx, &e)
		if err != nil {
			return 0, err
		}
		if ok {
			n++
		}
	}
	return n, tx.Commit()
}
