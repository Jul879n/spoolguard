//! Descubrimiento opcional de impresoras en subredes que el administrador configura a mano.
//! Solo conexiones TCP a 9100/515/631 (sin SNMP, sin exploits) y, si el 631 responde, una consulta IPP
//! Get-Printer-Attributes. Todo acotado: prefijo >= /24, tope de hosts, 8 en paralelo, timeouts cortos.
//! Mismo comportamiento que `internal/discover` del servidor.

use serde::Serialize;
use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

pub const MAX_HOSTS: usize = 512;
pub const MAX_CIDRS: usize = 16;
pub const MIN_PREFIX: u8 = 24;
pub const PARALLEL: usize = 8;
pub const DIAL_TIMEOUT: Duration = Duration::from_millis(500);
const IPP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_IPP_REPLY: usize = 1 << 20;

/// Puertos que se prueban (raw, lpd, ipp). Solo las pruebas los cambian.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ports {
    pub raw: u16,
    pub lpd: u16,
    pub ipp: u16,
}
pub const DEFAULT_PORTS: Ports = Ports { raw: 9100, lpd: 515, ipp: 631 };

/// Valida los rangos y devuelve los hosts a probar. Rechaza: no IPv4, prefijo menor que /24 (por ejemplo /8),
/// direcciones reservadas, más de `MAX_HOSTS` hosts en total y rangos públicos salvo `allow_public`.
pub fn parse_cidrs(cidrs: &[String], allow_public: bool) -> Result<Vec<Ipv4Addr>, String> {
    let list: Vec<&str> = cidrs.iter().map(|c| c.trim()).filter(|c| !c.is_empty()).collect();
    if list.is_empty() {
        return Err("no hay subredes configuradas: indica al menos un rango (por ejemplo 192.168.1.0/24)".into());
    }
    if list.len() > MAX_CIDRS {
        return Err(format!("demasiados rangos (máx. {MAX_CIDRS})"));
    }
    let mut hosts: Vec<Ipv4Addr> = vec![];
    for c in list {
        if c.len() > 64 {
            return Err("rango inválido".into());
        }
        let (addr, bits) = match c.split_once('/') {
            Some((a, b)) => (a, b.parse::<u8>().map_err(|_| format!("CIDR inválido: \"{c}\""))?),
            None => (c, 32),
        };
        if addr.contains(':') {
            return Err(format!("\"{c}\": solo IPv4"));
        }
        let ip: Ipv4Addr = addr.parse().map_err(|_| format!("dirección inválida: \"{c}\""))?;
        if bits > 32 {
            return Err(format!("CIDR inválido: \"{c}\""));
        }
        if bits < MIN_PREFIX {
            return Err(format!("\"{c}\": el prefijo debe ser /{MIN_PREFIX} a /32 (máx. 256 direcciones por rango)"));
        }
        let mask: u32 = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
        let first = u32::from(ip) & mask;
        let last = first | !mask;
        let net = Ipv4Addr::from(first);
        if net.is_unspecified() || net.is_multicast() || net.octets()[0] == 0 || net.octets()[0] >= 240 {
            return Err(format!("\"{c}\": rango reservado o no válido"));
        }
        if !(net.is_private() || net.is_loopback() || net.is_link_local() || allow_public) {
            return Err(format!("\"{c}\" es un rango público: requiere confirmación explícita"));
        }
        for n in first..=last {
            if bits <= 30 && (n == first || n == last) {
                continue; // red y difusión
            }
            let h = Ipv4Addr::from(n);
            if !hosts.contains(&h) {
                hosts.push(h);
                if hosts.len() > MAX_HOSTS {
                    return Err(format!("demasiados hosts (máx. {MAX_HOSTS} por consulta)"));
                }
            }
        }
    }
    Ok(hosts)
}

/// Un host con algún puerto de impresión abierto y lo que se supo de él.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Device {
    pub ip: String,
    pub ports: Vec<u16>,
    /// Respondió Get-Printer-Attributes.
    pub ipp: bool,
    pub model: String,
    pub name: String,
    pub info: String,
    pub location: String,
    pub state: String,
}

/// Transporte IPP inyectable: (host, puerto, petición) -> respuesta.
pub type Transport<'a> = &'a (dyn Fn(&str, u16, &[u8]) -> Result<Vec<u8>, String> + Sync);
/// Prueba de conexión inyectable: ¿acepta TCP?
pub type Dial<'a> = &'a (dyn Fn(SocketAddr) -> bool + Sync);

pub fn tcp_open(a: SocketAddr) -> bool {
    TcpStream::connect_timeout(&a, DIAL_TIMEOUT).is_ok()
}

/// Prueba `ports` en cada host con `PARALLEL` hilos como máximo y devuelve los dispositivos con algún puerto abierto.
pub fn scan(hosts: &[Ipv4Addr], ports: Ports, dial: Dial, transport: Transport, cancel: &AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Vec<Device> {
    let plist = [ports.raw, ports.lpd, ports.ipp];
    let jobs: Vec<(Ipv4Addr, u16)> = hosts.iter().flat_map(|h| plist.iter().map(move |p| (*h, *p))).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let open: Mutex<Vec<(Ipv4Addr, u16)>> = Mutex::new(vec![]);
    std::thread::scope(|sc| {
        for _ in 0..PARALLEL {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= jobs.len() || cancel.load(Ordering::Relaxed) {
                    break;
                }
                let (h, p) = jobs[i];
                if dial(SocketAddr::new(IpAddr::V4(h), p)) {
                    open.lock().unwrap().push((h, p));
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                progress(d, jobs.len());
            });
        }
    });
    let mut by: Vec<(Ipv4Addr, Vec<u16>)> = vec![];
    for (h, p) in open.into_inner().unwrap() {
        match by.iter_mut().find(|(x, _)| *x == h) {
            Some((_, v)) => v.push(p),
            None => by.push((h, vec![p])),
        }
    }
    by.sort_by_key(|(h, _)| *h);
    by.into_iter()
        .map(|(h, mut ps)| {
            ps.sort_unstable();
            let mut d = Device { ip: h.to_string(), ports: ps.clone(), ..Default::default() };
            if ps.contains(&ports.ipp) && !cancel.load(Ordering::Relaxed) {
                if let Ok(a) = get_printer_attributes(transport, &d.ip, ports.ipp, &["printer-make-and-model", "printer-name", "printer-info", "printer-location", "printer-state"]) {
                    if !a.is_empty() {
                        let first = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
                        d.ipp = true;
                        d.model = first("printer-make-and-model");
                        d.name = first("printer-name");
                        d.info = first("printer-info");
                        d.location = first("printer-location");
                        d.state = match first("printer-state").as_str() { "3" => "lista", "4" => "imprimiendo", "5" => "detenida", _ => "" }.into();
                    }
                }
            }
            d
        })
        .collect()
}

/// Una cola del sistema con dispositivo de red.
#[derive(Debug, Clone)]
pub struct QueueRef {
    pub name: String,
    /// IP o nombre del dispositivo al que apunta la cola.
    pub host: String,
    /// Control activo (regla propia o global).
    pub controlled: bool,
}

/// Dispositivo encontrado y su relación con las colas.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeviceView {
    #[serde(flatten)]
    pub device: Device,
    /// Colas del sistema que apuntan a esta IP (vacío = ninguna).
    pub queue: String,
    /// `controlada` | `cola_sin_control` | `sin_cola`.
    pub managed: String,
    pub explanation: String,
}

fn port_list(ps: &[u16]) -> String {
    ps.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
}

/// Compara lo encontrado con las colas. `resolve` convierte un nombre de host en IPv4 (se inyecta en las pruebas).
pub fn classify(devs: Vec<Device>, queues: &[QueueRef], resolve: &dyn Fn(&str) -> Vec<String>) -> Vec<DeviceView> {
    let ips: Vec<(Vec<String>, &QueueRef)> = queues
        .iter()
        .map(|q| (if q.host.parse::<Ipv4Addr>().is_ok() { vec![q.host.clone()] } else { resolve(&q.host) }, q))
        .collect();
    devs.into_iter()
        .map(|d| {
            let mut all: Vec<&QueueRef> = ips.iter().filter(|(i, _)| i.contains(&d.ip)).map(|(_, q)| *q).collect();
            all.sort_by(|a, b| a.name.cmp(&b.name));
            let ctl: Vec<&str> = all.iter().filter(|q| q.controlled).map(|q| q.name.as_str()).collect();
            let any: Vec<&str> = all.iter().map(|q| q.name.as_str()).collect();
            let ports = port_list(&d.ports);
            let (queue, managed, explanation) = if !ctl.is_empty() {
                let q = ctl.join(", ");
                (q.clone(), "controlada", format!("Gestionada por la cola controlada «{q}». Aun así acepta conexiones directas por {ports} de cualquier equipo que la alcance: restringe en la impresora o en el firewall que solo este equipo pueda conectarse."))
            } else if !any.is_empty() {
                let q = any.join(", ");
                (q.clone(), "cola_sin_control", format!("Existe la cola «{q}» hacia este dispositivo, pero el control está apagado para ella: los trabajos no se evalúan. Activa el control de la cola y restringe la impresión directa."))
            } else {
                let mut e = format!("Ninguna cola de este equipo apunta a esta IP. Responde en {ports}: cualquier equipo de la red puede imprimir en ella directamente, sin pasar por el control (riesgo de impresión directa).");
                if !d.ipp {
                    e.push_str(" No se pudo confirmar que sea una impresora (no respondió IPP): verifícalo.");
                }
                (String::new(), "sin_cola", e)
            };
            DeviceView { device: d, queue, managed: managed.into(), explanation }
        })
        .collect()
}

// --- IPP mínimo (RFC 8010) ---

fn put(b: &mut Vec<u8>, tag: u8, name: &str, val: &[u8]) {
    b.push(tag);
    b.extend((name.len() as u16).to_be_bytes());
    b.extend(name.as_bytes());
    b.extend((val.len() as u16).to_be_bytes());
    b.extend(val);
}

/// Petición IPP 1.1 Get-Printer-Attributes (0x000B) con los atributos pedidos.
pub fn encode_get_printer_attributes(uri: &str, names: &[&str]) -> Vec<u8> {
    let mut b = vec![1, 1, 0x00, 0x0B, 0, 0, 0, 1, 0x01];
    put(&mut b, 0x47, "attributes-charset", b"utf-8");
    put(&mut b, 0x48, "attributes-natural-language", b"en");
    put(&mut b, 0x45, "printer-uri", uri.as_bytes());
    for (i, n) in names.iter().enumerate() {
        put(&mut b, 0x44, if i == 0 { "requested-attributes" } else { "" }, n.as_bytes());
    }
    b.push(0x03);
    b
}

/// Atributos de impresora (grupo 0x04) como pares (nombre, valor); los multivalor repiten el nombre.
/// Ignora colecciones y tipos que no se usan; con límites estrictos de longitud.
pub fn parse_response(b: &[u8]) -> Result<(u16, Vec<(String, String)>), String> {
    if b.len() < 9 {
        return Err("respuesta IPP demasiado corta".into());
    }
    let status = u16::from_be_bytes([b[2], b[3]]);
    let (mut i, mut depth) = (8usize, 0u32);
    let mut group = 0u8;
    let (mut out, mut last) = (vec![], String::new());
    while i < b.len() {
        let tag = b[i];
        i += 1;
        if tag == 0x03 {
            break;
        }
        if tag <= 0x0F {
            group = tag;
            last.clear();
            continue;
        }
        let rd = |i: &mut usize| -> Result<&[u8], String> {
            if *i + 2 > b.len() {
                return Err("IPP truncado".into());
            }
            let n = u16::from_be_bytes([b[*i], b[*i + 1]]) as usize;
            *i += 2;
            if *i + n > b.len() {
                return Err("IPP truncado".into());
            }
            let s = &b[*i..*i + n];
            *i += n;
            Ok(s)
        };
        let name = String::from_utf8_lossy(rd(&mut i)?).into_owned();
        let val = rd(&mut i)?;
        match tag {
            0x34 => {
                depth += 1;
                continue;
            }
            0x37 => {
                depth = depth.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        if depth > 0 || group != 0x04 {
            continue;
        }
        let s = match tag {
            0x21 | 0x23 if val.len() == 4 => i32::from_be_bytes([val[0], val[1], val[2], val[3]]).to_string(),
            0x22 if val.len() == 1 => (val[0] != 0).to_string(),
            0x41 | 0x42 | 0x44..=0x49 => String::from_utf8_lossy(val).into_owned(),
            _ => continue,
        };
        let name = if name.is_empty() { last.clone() } else { name };
        if name.is_empty() {
            continue;
        }
        last = name.clone();
        out.push((name, s));
    }
    Ok((status, out))
}

pub fn get_printer_attributes(tr: Transport, host: &str, port: u16, names: &[&str]) -> Result<Vec<(String, String)>, String> {
    let uri = format!("ipp://{}:{port}/ipp/print", if host.contains(':') { format!("[{host}]") } else { host.to_string() });
    let raw = tr(host, port, &encode_get_printer_attributes(&uri, names))?;
    let (status, attrs) = parse_response(&raw)?;
    if status >= 0x0100 {
        return Err(format!("IPP status 0x{status:04x}"));
    }
    Ok(attrs)
}

/// Transporte real: POST application/ipp a /ipp/print y, si no existe, a /.
pub fn http_transport(host: &str, port: u16, req: &[u8]) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    for path in ["/ipp/print", "/"] {
        match post_ipp(host, port, path, req) {
            Ok(b) => return Ok(b),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn post_ipp(host: &str, port: u16, path: &str, req: &[u8]) -> Result<Vec<u8>, String> {
    let ip: IpAddr = host.parse().map_err(|_| "host inválido".to_string())?;
    let mut s = TcpStream::connect_timeout(&SocketAddr::new(ip, port), DIAL_TIMEOUT).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(IPP_TIMEOUT));
    let _ = s.set_write_timeout(Some(IPP_TIMEOUT));
    let head = format!("POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", req.len());
    let mut msg = head.into_bytes();
    msg.extend_from_slice(req);
    s.write_all(&msg).map_err(|e| e.to_string())?;
    // Tope de tiempo TOTAL: el timeout de lectura es por llamada y una impresora hostil podría gotear bytes
    // para retener el hilo muchísimo tiempo. Un timeout de lectura deja lo recibido hasta ese momento.
    let mut buf = vec![];
    let start = std::time::Instant::now();
    let mut rd = (&s).take(MAX_IPP_REPLY as u64);
    let mut chunk = [0u8; 8192];
    while start.elapsed() < IPP_TIMEOUT * 2 {
        match rd.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("respuesta HTTP inválida")?;
    let head = String::from_utf8_lossy(&buf[..split]).to_lowercase();
    if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") {
        return Err(format!("HTTP {}", head.lines().next().unwrap_or("")));
    }
    let body = &buf[split + 4..];
    Ok(if head.contains("transfer-encoding: chunked") { dechunk(body)? } else { body.to_vec() })
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    loop {
        let nl = b.windows(2).position(|w| w == b"\r\n").ok_or("chunk inválido")?;
        let n = usize::from_str_radix(String::from_utf8_lossy(&b[..nl]).split(';').next().unwrap_or("").trim(), 16).map_err(|_| "chunk inválido")?;
        b = &b[nl + 2..];
        if n == 0 {
            return Ok(out);
        }
        if b.len() < n || out.len() + n > MAX_IPP_REPLY {
            return Err("chunk inválido".into());
        }
        out.extend(&b[..n]);
        b = b.get(n + 2..).ok_or("chunk inválido")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn valida_rangos() {
        assert_eq!(parse_cidrs(&s(&["192.168.1.0/24"]), false).unwrap().len(), 254);
        assert_eq!(parse_cidrs(&s(&["192.168.1.77/24"]), false).unwrap().len(), 254);
        assert_eq!(parse_cidrs(&s(&["10.0.0.5"]), false).unwrap().len(), 1);
        assert_eq!(parse_cidrs(&s(&["127.0.0.1/32"]), false).unwrap().len(), 1);
        assert_eq!(parse_cidrs(&s(&["192.168.1.0/31"]), false).unwrap().len(), 2);
        assert_eq!(parse_cidrs(&s(&["192.168.1.0/24", "192.168.1.0/25"]), false).unwrap().len(), 254);
        assert_eq!(parse_cidrs(&s(&["192.168.1.0/24", "192.168.2.0/24"]), false).unwrap().len(), 508);
        assert_eq!(parse_cidrs(&s(&["8.8.8.0/24"]), true).unwrap().len(), 254);
        for (c, frag) in [
            (vec![], "no hay subredes"), (vec![" "], "no hay subredes"), (vec!["10.0.0.0/8"], "prefijo"), (vec!["192.168.0.0/16"], "prefijo"),
            (vec!["192.168.0.0/23"], "prefijo"), (vec!["0.0.0.0/0"], "prefijo"), (vec!["::1/128"], "IPv4"), (vec!["abc"], "inválida"),
            (vec!["10.0.0.0/33"], "inválido"), (vec!["224.0.0.0/24"], "reservado"), (vec!["0.1.2.0/24"], "reservado"),
            (vec!["8.8.8.0/24"], "público"), (vec!["8.8.8.8"], "público"), (vec!["100.64.0.0/24"], "público"),
            (vec!["192.168.1.0/24", "192.168.2.0/24", "192.168.3.0/24"], "demasiados hosts"),
        ] {
            let e = parse_cidrs(&c.iter().map(|x| x.to_string()).collect::<Vec<_>>(), false).unwrap_err();
            assert!(e.contains(frag), "{c:?}: {e}");
        }
        let many: Vec<String> = (0..17).map(|i| format!("10.0.0.{i}")).collect();
        assert!(parse_cidrs(&many, false).unwrap_err().contains("demasiados rangos"));
    }

    fn attr(tag: u8, name: &str, v: &[u8]) -> Vec<u8> {
        let mut b = vec![];
        put(&mut b, tag, name, v);
        b
    }
    fn resp(status: u16, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![1, 1, (status >> 8) as u8, status as u8, 0, 0, 0, 1, 0x04];
        for a in attrs {
            b.extend(a);
        }
        b.push(3);
        b
    }

    #[test]
    fn parsea_ipp_con_colecciones_y_multivalor() {
        let b = resp(0, &[
            attr(0x44, "printer-uri-supported", b"ipp://x/ipp/print"), attr(0x45, "", b"ipps://x/ipp/print"),
            attr(0x34, "media-col", b""), attr(0x4A, "", b"media-size"), attr(0x34, "", b""), attr(0x21, "", &[0, 0, 0, 5]), attr(0x37, "", b""), attr(0x37, "", b""),
            attr(0x23, "printer-state", &[0, 0, 0, 3]), attr(0x22, "ok", &[1]), attr(0x41, "printer-info", b"Oficina"),
        ]);
        let (st, a) = parse_response(&b).unwrap();
        assert_eq!(st, 0);
        let names: Vec<&str> = a.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["printer-uri-supported", "printer-uri-supported", "printer-state", "ok", "printer-info"]);
        assert_eq!(a[2].1, "3");
        // Truncada: nunca entra en pánico.
        for n in 0..b.len() - 1 {
            let _ = parse_response(&b[..n]);
        }
        assert!(parse_response(&b[..14]).is_err());
    }

    #[test]
    fn codifica_la_peticion() {
        let b = encode_get_printer_attributes("ipp://h:631/ipp/print", &["a", "b"]);
        assert_eq!(&b[2..4], &[0, 0x0B]);
        let (_, a) = parse_response(&[&b[..8], &[0x04], &b[9..]].concat()).unwrap();
        let get = |k: &str| a.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
        assert_eq!(get("printer-uri"), ["ipp://h:631/ipp/print"]);
        assert_eq!(get("requested-attributes"), ["a", "b"]);
    }

    #[test]
    fn clasifica_contra_las_colas() {
        let dev = |ip: &str| Device { ip: ip.into(), ports: vec![9100], ..Default::default() };
        let q = |n: &str, h: &str, c: bool| QueueRef { name: n.into(), host: h.into(), controlled: c };
        let r = |h: &str| if h == "imp.local" { vec!["10.0.0.9".to_string()] } else { vec![] };
        let v = classify(vec![dev("10.0.0.5"), dev("10.0.0.6"), dev("10.0.0.7"), dev("10.0.0.9")], &[q("A", "10.0.0.5", true), q("B", "10.0.0.6", false), q("C", "imp.local", true)], &r);
        assert_eq!((v[0].managed.as_str(), v[0].queue.as_str()), ("controlada", "A"));
        assert_eq!((v[1].managed.as_str(), v[1].queue.as_str()), ("cola_sin_control", "B"));
        assert_eq!((v[2].managed.as_str(), v[2].queue.as_str()), ("sin_cola", ""));
        assert!(v[2].explanation.contains("impresión directa") && v[2].explanation.contains("no respondió IPP"));
        assert_eq!(v[3].managed, "controlada"); // resuelto por nombre
    }

    #[test]
    fn dechunk_ok() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
        assert!(dechunk(b"zz\r\n").is_err());
    }

    #[test]
    fn escanea_listeners_falsos_en_loopback() {
        let raw = TcpListener::bind("127.0.0.1:0").unwrap();
        let ipp = TcpListener::bind("127.0.0.1:0").unwrap();
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // se cierra: puerto cerrado
        let ports = Ports { raw: raw.local_addr().unwrap().port(), lpd: free, ipp: ipp.local_addr().unwrap().port() };
        let hosts = parse_cidrs(&s(&["127.0.0.1"]), false).unwrap();
        let tr = |_: &str, _: u16, _: &[u8]| -> Result<Vec<u8>, String> {
            Ok(resp(0, &[attr(0x41, "printer-make-and-model", b"Fake Laser 9000"), attr(0x42, "printer-name", b"falsa"), attr(0x23, "printer-state", &[0, 0, 0, 3])]))
        };
        let last = AtomicUsize::new(0);
        let d = scan(&hosts, ports, &tcp_open, &tr, &AtomicBool::new(false), &|d, t| {
            assert_eq!(t, 3);
            last.store(d, Ordering::SeqCst);
        });
        assert_eq!(last.load(Ordering::SeqCst), 3);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].ip, "127.0.0.1");
        assert_eq!(d[0].ports.len(), 2);
        assert!(!d[0].ports.contains(&free));
        assert!(d[0].ipp && d[0].model == "Fake Laser 9000" && d[0].name == "falsa" && d[0].state == "lista");
    }

    #[test]
    fn no_pasa_de_ocho_conexiones_y_se_cancela() {
        let hosts = parse_cidrs(&s(&["10.1.2.0/24"]), false).unwrap();
        let (cur, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let dial = |_: SocketAddr| {
            let c = cur.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(c, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            cur.fetch_sub(1, Ordering::SeqCst);
            false
        };
        let tr = |_: &str, _: u16, _: &[u8]| -> Result<Vec<u8>, String> { Err("no".into()) };
        assert!(scan(&hosts[..20], DEFAULT_PORTS, &dial, &tr, &AtomicBool::new(false), &|_, _| {}).is_empty());
        assert!(peak.load(Ordering::SeqCst) <= PARALLEL && peak.load(Ordering::SeqCst) > 1);
        // Cancelado de entrada: no se hace ninguna conexión.
        let calls = AtomicUsize::new(0);
        let dial2 = |_: SocketAddr| {
            calls.fetch_add(1, Ordering::SeqCst);
            false
        };
        scan(&hosts, DEFAULT_PORTS, &dial2, &tr, &AtomicBool::new(true), &|_, _| {});
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn transporte_http_real_contra_servidor_falso() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let body = resp(0, &[attr(0x44, "x", b"y")]);
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
            assert!(req.starts_with("post /ipp/print http/1.1") && req.contains("content-type: application/ipp"));
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            c.write_all(head.as_bytes()).unwrap();
            c.write_all(&body).unwrap();
        });
        let a = get_printer_attributes(&http_transport, "127.0.0.1", port, &["x"]).unwrap();
        assert_eq!(a, [("x".to_string(), "y".to_string())]);
        t.join().unwrap();
    }
}
