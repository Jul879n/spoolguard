//! Tipo y estado de conexión de cada impresora (independiente del idioma del SO).

use serde::Serialize;
#[cfg(unix)]
use std::net::{TcpStream, ToSocketAddrs};
use std::{path::Path, process::Command, time::Duration};

#[derive(Serialize, Clone)]
pub struct PrinterInfo {
    /// "usb" | "red" | "wifi" | "compartida" | "otra"
    pub kind: String,
    pub uri: String,
    /// "lista" | "ocupada" | "detenida" | "desconectada" | "desconocido"
    pub status: String,
    pub detail: String,
    /// false si la cola no puede retener trabajos por pausa (puertos PORTPROMPT:, XPSPort:, nul).
    pub hold_supported: bool,
    /// Inventario: modelo/driver que informa la cola (vacío = no informado).
    pub driver: String,
    /// usb | socket | ipp | lpd | wsd | red | compartida | archivo | local | desconocido.
    pub conn_type: String,
    /// IP/host del dispositivo si es de red.
    pub host: String,
    /// None = no informado.
    pub shared: Option<bool>,
}

impl Default for PrinterInfo {
    fn default() -> Self {
        PrinterInfo { kind: String::new(), uri: String::new(), status: String::new(), detail: String::new(), hold_supported: true, driver: String::new(), conn_type: String::new(), host: String::new(), shared: None }
    }
}

/// Host/IP del dispositivo de una cola de red (vacío si no es de red o no se sabe), sin probar la conexión.
pub fn queue_host(work_dir: &Path, name: &str) -> String {
    #[cfg(unix)]
    {
        control_core::inventory::net_host(&control_core::cups::device_uri(work_dir, name))
    }
    #[cfg(windows)]
    {
        info(work_dir, name, &[]).host
    }
}

/// Puerto/URI de salida (sin credenciales ni consulta) y tipo de conexión de una cola, para rellenar cada evento.
/// Se recuerda 60 s por cola. Vacío si el sistema no lo informa.
pub fn route(work_dir: &Path, name: &str) -> (String, String) {
    let cached = route_cache().lock().unwrap_or_else(|e| e.into_inner()).get(name).map(|(t, u, c)| (t.elapsed() < Duration::from_secs(60), u.clone(), c.clone()));
    if let Some((true, u, c)) = cached {
        return (u, c);
    }
    route_miss(work_dir, name, cached.map(|(_, u, c)| (u, c)))
}

type RouteCache = std::collections::HashMap<String, (std::time::Instant, String, String)>;

fn route_cache() -> &'static std::sync::Mutex<RouteCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<RouteCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[cfg(unix)]
fn route_miss(work_dir: &Path, name: &str, _stale: Option<(String, String)>) -> (String, String) {
    let u = control_core::cups::device_uri(work_dir, name);
    let c = control_core::inventory::conn_type_uri(&u);
    let (uri, conn) = (control_core::backup::sanitize_uri(&u), if u.is_empty() { String::new() } else { c.to_string() });
    if !uri.is_empty() {
        route_cache().lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), (std::time::Instant::now(), uri.clone(), conn.clone()));
    }
    (uri, conn)
}

/// Windows: no bloquea nunca. `info` lanza PowerShell (`Get-Printer`) y con el spooler ocupado imprimiendo espera hasta
/// minutos (medido: 89 s en ráfagas), tiempo en que el hilo de evaluación no libera ni cancela nada. Se devuelve el valor
/// anterior (o vacío la primera vez) y se refresca en un hilo aparte.
#[cfg(windows)]
fn route_miss(work_dir: &Path, name: &str, stale: Option<(String, String)>) -> (String, String) {
    static REFRESHING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    {
        let mut r = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        if !r.iter().any(|n| n == name) {
            r.push(name.to_string());
            let (dir, n) = (work_dir.to_path_buf(), name.to_string());
            let spawned = std::thread::Builder::new().name("route-refresh".into()).spawn(move || {
                let i = info(&dir, &n, &[]);
                if !i.uri.is_empty() {
                    route_cache().lock().unwrap_or_else(|e| e.into_inner()).insert(n.clone(), (std::time::Instant::now(), i.uri, i.conn_type));
                }
                REFRESHING.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| x != &n);
            });
            if spawned.is_err() {
                r.retain(|x| x != name);
            }
        }
    }
    stale.unwrap_or_default()
}

/// Tipo de conexión según el puerto de Windows. "red" solo si el puerto es de red real
/// (TCP/IP estándar, WSD, IPP/HTTP, IP o host:puerto), no por contener un punto ni ser una ruta de archivo.
#[cfg_attr(not(windows), allow(dead_code))]
fn classify_port(port: &str, host_address: &str) -> &'static str {
    let up = port.trim().to_uppercase();
    let net_scheme = ["HTTP://", "HTTPS://", "IPP://", "IPPS://"].iter().any(|p| up.starts_with(p));
    let host_port = up.rsplit_once(':').is_some_and(|(h, p)| {
        !h.is_empty() && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && h.chars().all(|c| c.is_ascii_alphanumeric() || ".-_[]:".contains(c))
    });
    if up.starts_with("USB") {
        "usb"
    } else if up.starts_with("\\\\") {
        "compartida"
    } else if up.starts_with("IP_") || up.starts_with("WSD") || net_scheme || host_port || !host_address.trim().is_empty() || up.parse::<std::net::Ipv4Addr>().is_ok() {
        "red"
    } else {
        "otra"
    }
}

/// Estos puertos imprimen aunque la cola esté en pausa, así que la retención no funciona.
#[cfg_attr(not(windows), allow(dead_code))]
fn port_can_hold(port: &str) -> bool {
    let up = port.trim().to_uppercase();
    !(up.starts_with("PORTPROMPT") || up == "XPSPORT:" || up == "NUL:" || up == "NUL")
}

/// El fax compartido de Windows (puerto SHRFAX: / driver "Microsoft Shared Fax Driver") no admite pausa.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_fax(port: &str, driver: &str) -> bool {
    port.trim().to_uppercase().starts_with("SHRFAX") || driver.to_lowercase().contains("shared fax")
}

#[cfg(unix)]
const GET_PRINTER: &str = "{
	NAME \"Get-Printer-Attributes\"
	OPERATION Get-Printer-Attributes
	GROUP operation-attributes-tag
	ATTR charset attributes-charset utf-8
	ATTR language attributes-natural-language en
	ATTR uri printer-uri $uri
	ATTR keyword requested-attributes device-uri,printer-state,printer-state-reasons,printer-is-accepting-jobs,printer-make-and-model,printer-is-shared
}
";

#[cfg(unix)]
fn host_port(uri: &str) -> Option<(String, u16)> {
    let (scheme, rest) = uri.split_once("://")?;
    let auth = rest.split(['/', '?']).next()?;
    let auth = auth.rsplit('@').next()?;
    let (host, port) = match auth.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h, p.parse().ok()?),
        _ => (auth, match scheme { "socket" => 9100, "lpd" => 515, "ipps" | "https" => 443, _ => 631 }),
    };
    Some((host.trim_matches(['[', ']']).to_string(), port))
}

#[cfg(unix)]
fn reachable(host: &str, port: u16) -> bool {
    (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .map(|a| TcpStream::connect_timeout(&a, Duration::from_millis(800)).is_ok())
        .unwrap_or(false)
}

#[cfg(unix)]
fn classify(uri: &str) -> &'static str {
    let scheme = uri.split("://").next().unwrap_or("");
    match scheme {
        "usb" | "hp" | "hpfax" => "usb",
        "dnssd" | "ipp" | "ipps" | "socket" | "lpd" | "http" | "https" | "mdns" => {
            // Sin forma fiable de distinguir WiFi de Ethernet desde la cola: se informa "red".
            "red"
        }
        "smb" => "compartida",
        _ => "otra",
    }
}

/// Dispositivos USB presentes (una sola consulta; limitar a usb evita los ~15 s de la búsqueda en red).
#[cfg(unix)]
pub fn usb_devices() -> Vec<String> {
    Command::new("lpinfo").args(["--include-schemes", "usb", "-v"]).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default()
}
#[cfg(windows)]
pub fn usb_devices() -> Vec<String> {
    vec![]
}

#[cfg(unix)]
fn usb_connected(uri: &str, usb: &[String]) -> bool {
    let key = uri.split('?').next().unwrap_or(uri);
    usb.iter().any(|l| l.contains(key))
}

#[cfg(unix)]
pub fn info(work_dir: &Path, name: &str, usb: &[String]) -> PrinterInfo {
    if !control_core::cups::valid_queue(name) {
        return PrinterInfo { status: "desconocido".into(), kind: "desconocido".into(), hold_supported: true, ..Default::default() };
    }
    let _ = std::fs::create_dir_all(work_dir);
    let test = work_dir.join(format!("get-printer-{}.test", name.replace(['/', '\\', '.'], "_")));
    let _ = std::fs::write(&test, GET_PRINTER);
    let out = Command::new("ipptool")
        .args(["-tv", &format!("ipp://localhost/printers/{name}"), test.to_str().unwrap_or("")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let get = |k: &str| out.lines().filter_map(|l| l.trim().split_once(" = ").filter(|(a, _)| a.split(" (").next() == Some(k)).map(|(_, v)| v.to_string())).next_back().unwrap_or_default();
    let uri = get("device-uri");
    let state = get("printer-state");
    let reasons = get("printer-state-reasons");
    let accepting = get("printer-is-accepting-jobs") != "false";
    let kind = classify(&uri);
    let link_ok = match kind {
        "usb" => usb_connected(&uri, usb),
        "red" => host_port(&uri).map(|(h, p)| reachable(&h, p)).unwrap_or(false),
        _ => true,
    };
    let status = if uri.is_empty() { "desconocido" }
        else if !link_ok { "desconectada" }
        else if state == "stopped" || !accepting { "detenida" }
        else if state == "processing" { "ocupada" }
        else { "lista" };
    let shared = match get("printer-is-shared").as_str() { "true" => Some(true), "false" => Some(false), _ => None };
    PrinterInfo {
        kind: kind.into(), status: status.into(), detail: reasons, hold_supported: true, driver: get("printer-make-and-model"),
        conn_type: control_core::inventory::conn_type_uri(&uri).into(), host: control_core::inventory::net_host(&uri), shared, uri,
    }
}

/// Windows: evita que cada subproceso abra una ventana de consola (CREATE_NO_WINDOW).
#[cfg(windows)]
pub fn no_window(c: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000)
}

/// Windows: una sola invocación de PowerShell para todas las impresoras, cacheada ~5 s
/// (list_printers sondea en paralelo y cada actualización antes lanzaba un PowerShell por impresora).
#[cfg(windows)]
pub fn info(_work_dir: &Path, name: &str, _usb: &[String]) -> PrinterInfo {
    use std::{collections::HashMap, sync::Mutex, time::Instant};
    static CACHE: Mutex<Option<(Instant, HashMap<String, PrinterInfo>)>> = Mutex::new(None);
    // El candado se mantiene durante la consulta: los demás hilos esperan y leen el resultado fresco.
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if c.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(5)) {
        *c = Some((Instant::now(), query_all()));
    }
    c.as_ref().and_then(|(_, m)| m.get(name).cloned()).unwrap_or_else(|| PrinterInfo { status: "desconocido".into(), ..Default::default() })
}

#[cfg(windows)]
fn query_all() -> std::collections::HashMap<String, PrinterInfo> {
    // UTF-8 explícito: con la página OEM los nombres con tildes no coincidirían con los de list_printers.
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ports=@{}; $nums=@{}; Get-PrinterPort | ForEach-Object { $ports[$_.Name]=$_.PrinterHostAddress; $nums[$_.Name]=$_.PortNumber }; \
        ConvertTo-Json -Compress -InputObject @(Get-Printer | ForEach-Object { [pscustomobject]@{ Name=$_.Name; PortName=$_.PortName; DriverName=$_.DriverName; PrinterStatus=[int]$_.PrinterStatus; HostAddress=$ports[$_.PortName]; PortNumber=$nums[$_.PortName]; Shared=[bool]$_.Shared } })";
    let out = no_window(Command::new("powershell").args(["-NoProfile", "-Command", script])).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(out.trim().trim_start_matches('\u{feff}')).unwrap_or_default();
    let rows = match v { serde_json::Value::Array(a) => a, o @ serde_json::Value::Object(_) => vec![o], _ => vec![] };
    rows.iter().filter_map(|r| {
        let name = r["Name"].as_str()?.to_string();
        let port = r["PortName"].as_str().unwrap_or("").to_string();
        let kind = classify_port(&port, r["HostAddress"].as_str().unwrap_or(""));
        let status = match r["PrinterStatus"].as_u64() {
            Some(0) => "lista",
            Some(1) | Some(2) => "detenida",
            Some(3) | Some(4) => "ocupada",
            _ => "desconocido",
        };
        let hold_supported = port_can_hold(&port) && !is_fax(&port, r["DriverName"].as_str().unwrap_or(""));
        let host_addr = r["HostAddress"].as_str().unwrap_or("");
        let conn_type = control_core::inventory::conn_type_port(&port, host_addr, r["PortNumber"].as_u64().unwrap_or(0) as u32);
        let host = if matches!(conn_type, "socket" | "lpd" | "ipp" | "red") { control_core::inventory::host_of_port(&port, host_addr) } else { String::new() };
        Some((name, PrinterInfo {
            kind: kind.into(), status: status.into(), detail: String::new(), hold_supported, driver: r["DriverName"].as_str().unwrap_or("").to_string(),
            conn_type: conn_type.into(), host, shared: r["Shared"].as_bool(), uri: port,
        }))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clasifica_puertos() {
        assert_eq!(classify_port("USB001", ""), "usb");
        assert_eq!(classify_port("IP_192.168.1.5", ""), "red");
        assert_eq!(classify_port("WSD-1234-abcd", ""), "red");
        assert_eq!(classify_port("192.168.1.5", ""), "red");
        assert_eq!(classify_port("192.168.1.5:9100", ""), "red");
        assert_eq!(classify_port("impresora01:9100", ""), "red");
        assert_eq!(classify_port("http://10.0.0.2:631/ipp/print", ""), "red");
        assert_eq!(classify_port("COM1_net", "10.0.0.7"), "red");
        assert_eq!(classify_port(r"\\srv\hp", ""), "compartida");
        // Un punto o una ruta de archivo no son red.
        assert_eq!(classify_port(r"C:\e2e\out\prx.prn", ""), "otra");
        assert_eq!(classify_port("salida.v2.prn", ""), "otra");
        for p in ["LPT1:", "COM1:", "FILE:", "PORTPROMPT:", "nul:", "XPSPort:"] {
            assert_eq!(classify_port(p, ""), "otra", "{p}");
        }
    }

    #[test]
    fn fax_sin_retencion() {
        assert!(is_fax("SHRFAX:", "Cualquiera"));
        assert!(is_fax("shrfax:", ""));
        assert!(is_fax("OTRO:", "Microsoft Shared Fax Driver"));
        assert!(!is_fax("IP_1.2.3.4", "Generic / Text Only"));
        assert!(!is_fax(r"C:\x.prn", "Microsoft Print To PDF"));
    }

    #[test]
    fn puertos_sin_retencion() {
        for p in ["PORTPROMPT:", "portprompt:", "XPSPort:", "nul:"] {
            assert!(!port_can_hold(p), "{p}");
        }
        for p in ["FILE:", "USB001", r"C:\x.prn", "IP_1.2.3.4", "LPT1:"] {
            assert!(port_can_hold(p), "{p}");
        }
    }
}
