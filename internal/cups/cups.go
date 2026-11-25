// Package cups replica core/src/cups.rs y printers.rs (unix): lpstat, lpadmin, lp, cancel e ipptool.
// Los binarios se buscan en PATH (las pruebas inyectan falsos).
package cups

import (
	"context"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"controlimpresion/internal/rules"
)

const getJob = `{
	NAME "Get-Job-Attributes"
	OPERATION Get-Job-Attributes
	GROUP operation-attributes-tag
	ATTR charset attributes-charset utf-8
	ATTR language attributes-natural-language en
	ATTR uri job-uri $uri
	ATTR name requesting-user-name root
	ATTR keyword requested-attributes job-name,job-state,job-hold-until,job-originating-user-name,job-originating-host-name,document-name-supplied,job-k-octets,job-impressions
}
`

const getPrinter = `{
	NAME "Get-Printer-Attributes"
	OPERATION Get-Printer-Attributes
	GROUP operation-attributes-tag
	ATTR charset attributes-charset utf-8
	ATTR language attributes-natural-language en
	ATTR uri printer-uri $uri
	ATTR keyword requested-attributes device-uri,printer-state,printer-state-reasons,printer-is-accepting-jobs,printer-make-and-model,printer-is-shared
}
`

// runOut ejecuta cmd y devuelve su salida aunque termine con código distinto de 0.
func runOut(cmd string, args ...string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	var stderr strings.Builder
	c := exec.CommandContext(ctx, cmd, args...)
	c.Stderr = &stderr
	out, err := c.Output()
	if err != nil {
		if _, ok := err.(*exec.ExitError); ok {
			return string(out), fmt.Errorf("%s: %s", cmd, strings.TrimSpace(stderr.String()))
		}
		return string(out), fmt.Errorf("%s: %v", cmd, err)
	}
	return string(out), nil
}

func run(cmd string, args ...string) (string, error) {
	out, err := runOut(cmd, args...)
	if err != nil {
		return "", err
	}
	return out, nil
}

// ValidQueue indica si name es un nombre de cola seguro para pasarlo como argumento o dentro de una URI IPP:
// sin guion inicial (se leería como opción), sin controles, espacios, `/`, `\`, `#` ni `?`. Los nombres reales de
// CUPS ya cumplen esto; es una defensa en profundidad por si un nombre llega de una copia de seguridad o de la red.
func ValidQueue(name string) bool {
	if name == "" || len(name) > 255 || name[0] == '-' {
		return false
	}
	for _, r := range name {
		if r < 0x20 || r == 0x7f || r == ' ' || r == '/' || r == '\\' || r == '#' || r == '?' {
			return false
		}
	}
	return true
}

var errQueue = fmt.Errorf("nombre de cola inválido")

// ListPrinters: `lpstat -e` lista los destinos.
func ListPrinters() ([]string, error) {
	out, err := run("lpstat", "-e")
	if err != nil {
		return nil, err
	}
	var v []string
	for _, l := range strings.Split(out, "\n") {
		if l = strings.TrimSpace(l); l != "" {
			v = append(v, l)
		}
	}
	return v, nil
}

// SetHold activa o quita la retención por defecto de la cola. Además aplica (o quita) Hold-New-Jobs,
// que retiene también los trabajos enviados con `job-hold-until=no-hold`. workDir guarda el archivo de prueba de ipptool.
func SetHold(workDir, printer string, hold bool) error {
	if !ValidQueue(printer) {
		return errQueue
	}
	v := "job-hold-until-default=no-hold"
	if hold {
		v = "job-hold-until-default=indefinite"
	}
	if _, err := run("lpadmin", "-p", printer, "-o", v); err != nil {
		return err
	}
	// Mejor esfuerzo: si falla, la retención por defecto sigue vigente (solo no cubre no-hold).
	_ = HoldNewJobs(workDir, printer, hold)
	return nil
}

// cupsSocket devuelve el socket local de cupsd si existe (la operación exige @SYSTEM por TCP, no por el socket local).
func cupsSocket() string {
	c := []string{"/run/cups/cups.sock", "/var/run/cups/cups.sock"}
	if s := os.Getenv("CUPS_SERVER"); strings.HasPrefix(s, "/") {
		c = append([]string{s}, c...)
	}
	for _, p := range c {
		if st, err := os.Stat(p); err == nil && st.Mode()&os.ModeSocket != 0 {
			return p
		}
	}
	return ""
}

// HoldNewJobs aplica Hold-New-Jobs (on) o Release-Held-New-Jobs (off) a la cola por el socket local de cupsd.
// No sobrevive al reinicio de cupsd: hay que reaplicarlo (entrypoint.sh y Reconcile).
func HoldNewJobs(workDir, printer string, on bool) error {
	if !ValidQueue(printer) {
		return errQueue
	}
	op := "Release-Held-New-Jobs"
	if on {
		op = "Hold-New-Jobs"
	}
	if err := os.MkdirAll(workDir, 0o700); err != nil {
		return err
	}
	test := filepath.Join(workDir, "hold-new-jobs-"+strings.ToLower(op[:4])+".test")
	body := fmt.Sprintf("{\n\tNAME %q\n\tOPERATION %s\n\tGROUP operation-attributes-tag\n\tATTR charset attributes-charset utf-8\n\tATTR language attributes-natural-language en\n\tATTR uri printer-uri ipp://localhost/printers/%s\n\tATTR name requesting-user-name root\n}\n", op, op, printer)
	if err := os.WriteFile(test, []byte(body), 0o600); err != nil {
		return err
	}
	uri := "ipp://localhost/printers/" + printer
	if sock := cupsSocket(); sock != "" {
		uri = "ipp://" + strings.ReplaceAll(sock, "/", "%2F") + "/printers/" + printer
	}
	_, err := run("ipptool", "-t", uri, test)
	return err
}

// HeldJobIDs: IDs de trabajos pendientes de la cola (`lpstat -o <cola>`).
func HeldJobIDs(printer string) ([]uint32, error) {
	if !ValidQueue(printer) {
		return nil, errQueue
	}
	out, err := run("lpstat", "-o", printer)
	if err != nil {
		return nil, err
	}
	var ids []uint32
	for _, l := range strings.Split(out, "\n") {
		f := strings.Fields(l)
		if len(f) == 0 {
			continue
		}
		tok := f[0][strings.LastIndexByte(f[0], '-')+1:]
		if n, err := strconv.ParseUint(tok, 10, 32); err == nil {
			ids = append(ids, uint32(n))
		}
	}
	return ids, nil
}

type HeldJob struct {
	ID    uint32
	Held  bool
	State string // job-state de IPP: pending-held, pending, processing...
	Job   rules.Job
	// Solo lo que CUPS informa (nil = no se sabe). job-k-octets está en KiB (se multiplica por 1024: es aproximado)
	// y job-impressions suele ser 0 hasta que el filtro procesa el trabajo; un 0 se trata como "desconocido".
	Pages *int64
	Bytes *int64
}

// posInt interpreta un entero de ipptool; ok es false si falta, no es número o es <= 0.
func posInt(v string) (int64, bool) {
	n, err := strconv.ParseInt(strings.TrimSpace(v), 10, 64)
	return n, err == nil && n > 0
}

// parseAttrs lee la salida `ipptool -tv`: los atributos de la respuesta vienen tras los de la petición.
func parseAttrs(out string) map[string]string {
	a := map[string]string{}
	inResp := false
	for _, l := range strings.Split(out, "\n") {
		l = strings.TrimSpace(l)
		if strings.HasPrefix(l, "job-uri") {
			inResp = true
		}
		if !inResp {
			continue
		}
		if k, v, ok := strings.Cut(l, " = "); ok {
			name, _, _ := strings.Cut(k, " (")
			if _, dup := a[name]; !dup {
				a[name] = v
			}
		}
	}
	return a
}

func isIPv4(s string) bool {
	ip := net.ParseIP(s)
	return ip != nil && ip.To4() != nil && !strings.Contains(s, ":")
}

func resolveV4(h string) string {
	if isIPv4(h) || h == "localhost" {
		return h
	}
	ips, err := net.LookupIP(h)
	if err == nil {
		for _, ip := range ips {
			if v4 := ip.To4(); v4 != nil {
				return v4.String()
			}
		}
	}
	return h
}

// JobInfo lee los atributos IPP del trabajo. workDir guarda el archivo de prueba de ipptool.
func JobInfo(workDir string, id uint32) (*HeldJob, error) {
	if err := os.MkdirAll(workDir, 0o700); err != nil {
		return nil, err
	}
	test := filepath.Join(workDir, "get-job.test")
	if err := os.WriteFile(test, []byte(getJob), 0o600); err != nil {
		return nil, err
	}
	out, err := runOut("ipptool", "-tv", fmt.Sprintf("ipp://localhost/jobs/%d", id), test)
	if err != nil {
		// Los trabajos de una cola cliente (otro cupsd) traen atributos duplicados: ipptool marca [FAIL]
		// y sale con código 1, pero la respuesta es válida. Se tolera si el estado fue successful-ok.
		if !strings.Contains(out, "status-code = successful-ok") {
			return nil, err
		}
	}
	a := parseAttrs(out)
	var origin *string
	if h, ok := a["job-originating-host-name"]; ok {
		r := resolveV4(h)
		if r != "localhost" && !strings.HasPrefix(r, "127.") {
			origin = &r
		}
	}
	doc, ok := a["job-name"]
	if !ok {
		doc = a["document-name-supplied"]
	}
	var user *string
	if u, ok := a["job-originating-user-name"]; ok {
		user = &u
	}
	h := &HeldJob{ID: id, Held: a["job-state"] == "pending-held", State: a["job-state"], Job: rules.Job{Document: doc, User: user, OriginIP: origin}}
	if n, ok := posInt(a["job-impressions"]); ok {
		h.Pages = &n
	}
	if n, ok := posInt(a["job-k-octets"]); ok && n < 1<<40 {
		b := n * 1024
		h.Bytes = &b
	}
	return h, nil
}

func Release(id uint32) error {
	_, err := run("lp", "-i", strconv.FormatUint(uint64(id), 10), "-H", "resume")
	return err
}

// VerdictsDir es donde el servidor deja el veredicto de cada trabajo para el filtro/backend de SpoolGuard
// (docker/spoolguard-backend o scripts/guard-cups.sh). Variable de entorno CONTROL_VERDICTS_DIR para cambiarla.
func VerdictsDir() string {
	if d := os.Getenv("CONTROL_VERDICTS_DIR"); d != "" {
		return d
	}
	return "/var/lib/spoolguard/verdicts"
}

// WriteVerdict deja `allow` o `deny` para el trabajo antes de liberarlo o cancelarlo. Sin la carpeta (protección no
// instalada) no hace nada.
func WriteVerdict(printer string, id uint32, allowed bool) {
	dir := VerdictsDir()
	if st, err := os.Stat(dir); err != nil || !st.IsDir() || !ValidQueue(printer) {
		return
	}
	v := "deny"
	if allowed {
		v = "allow"
	}
	tmp := filepath.Join(dir, fmt.Sprintf(".%s-%d.tmp", printer, id))
	if os.WriteFile(tmp, []byte(v), 0o644) == nil {
		if os.Rename(tmp, filepath.Join(dir, fmt.Sprintf("%s-%d", printer, id))) != nil {
			_ = os.Remove(tmp)
		}
	}
	if ents, err := os.ReadDir(dir); err == nil { // veredictos de más de un día
		for _, e := range ents {
			if info, err := e.Info(); err == nil && time.Since(info.ModTime()) > 24*time.Hour {
				_ = os.Remove(filepath.Join(dir, e.Name()))
			}
		}
	}
}

// stripGuard quita el prefijo del backend de SpoolGuard (`spoolguard:socket://…` → `socket://…`).
func stripGuard(u string) string { return strings.TrimPrefix(u, "spoolguard:") }

// Hold retiene un trabajo pendiente que CUPS no retuvo (carrera con un cambio de retención en la cola).
func Hold(id uint32) error {
	_, err := run("lp", "-i", strconv.FormatUint(uint64(id), 10), "-H", "hold")
	return err
}

func Cancel(id uint32) error {
	_, err := run("cancel", strconv.FormatUint(uint64(id), 10))
	return err
}

// PrinterInfo es el mismo JSON que printers::PrinterInfo.
type PrinterInfo struct {
	Kind          string `json:"kind"`
	URI           string `json:"uri"`
	Status        string `json:"status"`
	Detail        string `json:"detail"`
	HoldSupported bool   `json:"hold_supported"`
	// Inventario: modelo (printer-make-and-model), tipo de conexión fino, host del dispositivo si es de red
	// y si la cola está compartida (nil = CUPS no lo informó).
	Driver   string `json:"driver"`
	ConnType string `json:"conn_type"`
	Host     string `json:"host"`
	Shared   *bool  `json:"shared"`
}

func hostPort(uri string) (string, string, bool) {
	scheme, rest, ok := strings.Cut(uri, "://")
	if !ok {
		return "", "", false
	}
	auth := rest
	if i := strings.IndexAny(auth, "/?"); i >= 0 {
		auth = auth[:i]
	}
	if i := strings.LastIndexByte(auth, '@'); i >= 0 {
		auth = auth[i+1:]
	}
	host, port := auth, ""
	if i := strings.LastIndexByte(auth, ':'); i >= 0 && i < len(auth)-1 {
		if _, err := strconv.ParseUint(auth[i+1:], 10, 16); err == nil {
			host, port = auth[:i], auth[i+1:]
		}
	}
	if port == "" {
		port = map[string]string{"socket": "9100", "lpd": "515", "ipps": "443", "https": "443"}[scheme]
		if port == "" {
			port = "631"
		}
	}
	return strings.Trim(host, "[]"), port, true
}

func reachable(host, port string) bool {
	c, err := net.DialTimeout("tcp", net.JoinHostPort(host, port), 800*time.Millisecond)
	if err != nil {
		return false
	}
	c.Close()
	return true
}

func classify(uri string) string {
	scheme, _, _ := strings.Cut(uri, "://")
	switch scheme {
	case "usb", "hp", "hpfax":
		return "usb"
	case "dnssd", "ipp", "ipps", "socket", "lpd", "http", "https", "mdns":
		return "red"
	case "smb":
		return "compartida"
	}
	return "otra"
}

// USBDevices: una sola consulta a lpinfo; nil si no existe (se asume conectada).
func USBDevices() []string {
	out, err := run("lpinfo", "--include-schemes", "usb", "-v")
	if err != nil {
		return nil
	}
	return strings.Split(out, "\n")
}

// Info consulta tipo y estado de conexión de una cola.
func Info(workDir, name string, usb []string) PrinterInfo {
	if !ValidQueue(name) {
		return PrinterInfo{Kind: "desconocido", Status: "desconocido", HoldSupported: true}
	}
	_ = os.MkdirAll(workDir, 0o700)
	safe := strings.NewReplacer("/", "_", "\\", "_", ".", "_").Replace(name)
	test := filepath.Join(workDir, "get-printer-"+safe+".test")
	_ = os.WriteFile(test, []byte(getPrinter), 0o600)
	out, _ := run("ipptool", "-tv", "ipp://localhost/printers/"+name, test)
	get := func(k string) string { return ippAttr(out, k) }
	uri, state, reasons := stripGuard(get("device-uri")), get("printer-state"), get("printer-state-reasons")
	accepting := get("printer-is-accepting-jobs") != "false"
	kind := classify(uri)
	linkOK := true
	switch kind {
	case "usb":
		linkOK = usb == nil
		key, _, _ := strings.Cut(uri, "?")
		for _, l := range usb {
			if strings.Contains(l, key) {
				linkOK = true
			}
		}
	case "red":
		h, p, ok := hostPort(uri)
		linkOK = ok && reachable(h, p)
	}
	status := "lista"
	switch {
	case uri == "":
		status = "desconocido"
	case !linkOK:
		status = "desconectada"
	case state == "stopped" || !accepting:
		status = "detenida"
	case state == "processing":
		status = "ocupada"
	}
	pi := PrinterInfo{Kind: kind, URI: uri, Status: status, Detail: reasons, HoldSupported: true,
		Driver: get("printer-make-and-model"), ConnType: ConnType(uri), Host: NetHost(uri)}
	switch get("printer-is-shared") {
	case "true":
		t := true
		pi.Shared = &t
	case "false":
		f := false
		pi.Shared = &f
	}
	return pi
}

// ippAttr devuelve el último valor del atributo `k` (nombre exacto) en la salida de `ipptool -tv`.
func ippAttr(out, k string) string {
	v := ""
	for _, l := range strings.Split(out, "\n") {
		if a, b, ok := strings.Cut(strings.TrimSpace(l), " = "); ok {
			if name, _, _ := strings.Cut(a, " ("); name == k {
				v = b
			}
		}
	}
	return v
}

// DeviceURI consulta solo el device-uri de una cola (sin comprobar conectividad). Vacío si no se sabe.
func DeviceURI(workDir, name string) string {
	if !ValidQueue(name) {
		return ""
	}
	_ = os.MkdirAll(workDir, 0o700)
	safe := strings.NewReplacer("/", "_", "\\", "_", ".", "_").Replace(name)
	test := filepath.Join(workDir, "get-uri-"+safe+".test")
	_ = os.WriteFile(test, []byte(strings.Replace(getPrinter, "device-uri,printer-state,printer-state-reasons,printer-is-accepting-jobs,printer-make-and-model,printer-is-shared", "device-uri", 1)), 0o600)
	out, _ := runOut("ipptool", "-tv", "ipp://localhost/printers/"+name, test)
	return stripGuard(ippAttr(out, "device-uri"))
}

// ConnType clasifica el device-uri de CUPS: usb, socket, ipp, lpd, red (dnssd/mdns: no se sabe el protocolo),
// compartida (smb), archivo (cups-pdf, file), local (serie/paralelo/bluetooth) o desconocido.
func ConnType(uri string) string {
	scheme, _, ok := strings.Cut(strings.TrimSpace(uri), ":")
	if !ok {
		return "desconocido"
	}
	switch strings.ToLower(scheme) {
	case "usb", "hp", "hpfax", "ippusb":
		return "usb"
	case "socket":
		return "socket"
	case "ipp", "ipps", "http", "https":
		return "ipp"
	case "lpd":
		return "lpd"
	case "dnssd", "mdns":
		return "red"
	case "smb":
		return "compartida"
	case "file", "cups-pdf":
		return "archivo"
	case "serial", "parallel", "scsi", "bluetooth":
		return "local"
	}
	return "desconocido"
}

// NetHost devuelve el host/IP del dispositivo cuando la URI es de red directa (socket, ipp, lpd, smb, http); vacío si no.
func NetHost(uri string) string {
	switch ConnType(uri) {
	case "socket", "ipp", "lpd", "compartida":
		if h, _, ok := hostPort(uri); ok {
			return h
		}
	}
	return ""
}

// HostPort extrae host y puerto (con el puerto por defecto del esquema) de una URI de dispositivo.
func HostPort(uri string) (string, string, bool) { return hostPort(uri) }
