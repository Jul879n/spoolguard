//! Intercepción en CUPS (macOS/Linux): retiene trabajos y los libera o cancela.
//! Usa `lpstat`, `lpadmin`, `lp`, `cancel` e `ipptool` (incluidos en CUPS).
//! Verificado en macOS: CUPS no expone la aplicación de origen; solo
//! `job-name`, `document-name-supplied`, usuario y host de origen.

use crate::rules::Job;
use std::{collections::HashMap, fs, net::ToSocketAddrs, path::Path, process::Command};

fn parse_ipv4(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}

const GET_JOB: &str = "{
	NAME \"Get-Job-Attributes\"
	OPERATION Get-Job-Attributes
	GROUP operation-attributes-tag
	ATTR charset attributes-charset utf-8
	ATTR language attributes-natural-language en
	ATTR uri job-uri $uri
	ATTR name requesting-user-name root
	ATTR keyword requested-attributes job-name,job-state,job-hold-until,job-originating-user-name,job-originating-host-name,document-name-supplied,job-k-octets,job-impressions
}
";

/// ¿Es `name` un nombre de cola seguro para pasarlo como argumento o dentro de una URI IPP? Sin `-` inicial
/// (se leería como opción), sin controles, espacios, `/`, `\`, `#` ni `?`. Los nombres reales de CUPS ya
/// cumplen esto: es una defensa en profundidad (mismo criterio que `cups.ValidQueue` del servidor Go).
pub fn valid_queue(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.starts_with('-') && !name.chars().any(|c| c.is_control() || matches!(c, ' ' | '/' | '\\' | '#' | '?'))
}

const INVALID_QUEUE: &str = "nombre de cola inválido";

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let o = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(format!("{cmd}: {}", String::from_utf8_lossy(&o.stderr).trim()))
    }
}

/// Impresoras del sistema (`lpstat -e` lista los destinos).
pub fn list_printers() -> Result<Vec<String>, String> {
    Ok(run("lpstat", &["-e"])?.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Activa o desactiva la retención por defecto de la cola.
/// La retención de CUPS no usa pausa de cola, así que no hay pausa previa que respetar.
pub fn is_paused(_printer: &str) -> bool {
    false
}

/// Sin trabajos "liberados vivos" que vigilar: el sondeo normal basta.
pub fn busy() -> bool {
    false
}

pub fn set_hold(printer: &str, hold: bool, _leave_paused: bool) -> Result<(), String> {
    if !valid_queue(printer) {
        return Err(INVALID_QUEUE.into());
    }
    let v = if hold { "job-hold-until-default=indefinite" } else { "job-hold-until-default=no-hold" };
    run("lpadmin", &["-p", printer, "-o", v]).map(|_| ())
}

/// IDs de trabajos pendientes de la cola.
pub fn held_job_ids(printer: &str) -> Result<Vec<u32>, String> {
    if !valid_queue(printer) {
        return Err(INVALID_QUEUE.into());
    }
    let out = run("lpstat", &["-o", printer])?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_whitespace().next()?.rsplit('-').next()?.parse().ok())
        .collect())
}

pub struct HeldJob {
    pub id: u32,
    pub held: bool,
    pub job: Job,
    /// Solo lo que el SO informa (`None` = no se sabe): `job-impressions` suele ser 0 hasta que CUPS filtra el trabajo.
    pub pages: Option<u64>,
    /// `job-k-octets` está en KiB (× 1024): aproximado.
    pub bytes: Option<u64>,
}

/// Entero positivo de un atributo de ipptool; 0, ausente o no numérico = desconocido.
fn pos_int(a: &HashMap<String, String>, k: &str) -> Option<u64> {
    a.get(k).and_then(|v| v.trim().parse::<u64>().ok()).filter(|n| *n > 0)
}

/// Páginas y bytes que informa CUPS para un trabajo, a partir de sus atributos IPP.
pub fn sizes(a: &HashMap<String, String>) -> (Option<u64>, Option<u64>) {
    (pos_int(a, "job-impressions"), pos_int(a, "job-k-octets").filter(|n| *n < 1 << 40).map(|n| n * 1024))
}

/// device-uri de una cola (sin comprobar conectividad). Vacío si no se sabe.
pub fn device_uri(work_dir: &Path, name: &str) -> String {
    if !valid_queue(name) {
        return String::new();
    }
    let _ = fs::create_dir_all(work_dir);
    let test = work_dir.join(format!("get-uri-{}.test", name.replace(['/', '\\', '.'], "_")));
    let body = "{\n\tNAME \"Get-Printer-Attributes\"\n\tOPERATION Get-Printer-Attributes\n\tGROUP operation-attributes-tag\n\tATTR charset attributes-charset utf-8\n\tATTR language attributes-natural-language en\n\tATTR uri printer-uri $uri\n\tATTR keyword requested-attributes device-uri\n}\n";
    if fs::write(&test, body).is_err() {
        return String::new();
    }
    let out = Command::new("ipptool").args(["-tv", &format!("ipp://localhost/printers/{name}"), test.to_str().unwrap_or("")]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    out.lines().filter_map(|l| l.trim().split_once(" = ").filter(|(k, _)| k.split(" (").next() == Some("device-uri")).map(|(_, v)| v.to_string())).next_back().unwrap_or_default()
}

/// Lee los atributos IPP del trabajo. `work_dir` guarda el archivo de prueba de ipptool.
pub fn job_info(work_dir: &Path, id: u32) -> Result<HeldJob, String> {
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let test = work_dir.join("get-job.test");
    fs::write(&test, GET_JOB).map_err(|e| e.to_string())?;
    let uri = format!("ipp://localhost/jobs/{id}");
    let out = run("ipptool", &["-tv", &uri, test.to_str().unwrap()])?;
    let mut a: HashMap<String, String> = HashMap::new();
    let mut in_response = false;
    for l in out.lines() {
        let l = l.trim();
        // Los atributos de la respuesta vienen después de los de la petición.
        if l.starts_with("job-uri") {
            in_response = true;
        }
        if !in_response {
            continue;
        }
        if let Some((k, v)) = l.split_once(" = ") {
            if let Some(name) = k.split(" (").next() {
                a.entry(name.to_string()).or_insert_with(|| v.to_string());
            }
        }
    }
    // CUPS entrega a veces un nombre de equipo: se resuelve a IPv4 para que las reglas por IP apliquen.
    let host = a.get("job-originating-host-name").map(|h| {
        if parse_ipv4(h) || h == "localhost" {
            return h.clone();
        }
        (h.as_str(), 0u16)
            .to_socket_addrs()
            .ok()
            .and_then(|it| it.filter_map(|s| match s.ip() { std::net::IpAddr::V4(v) => Some(v.to_string()), _ => None }).next())
            .unwrap_or_else(|| h.clone())
    });
    let (pages, bytes) = sizes(&a);
    Ok(HeldJob {
        pages,
        bytes,
        id,
        held: a.get("job-state").map(|s| s == "pending-held").unwrap_or(false),
        job: Job {
            document: a
                .get("job-name")
                .or_else(|| a.get("document-name-supplied"))
                .cloned()
                .unwrap_or_default(),
            // CUPS no expone la aplicación de origen; `document-name-supplied` es el archivo enviado, no la app.
            app: None,
            user: a.get("job-originating-user-name").cloned(),
            origin_ip: host.filter(|h| h != "localhost" && !h.starts_with("127.")),
            origin_host: None,
            site: None,
        },
    })
}

// --- Vigía de trabajos nuevos: notificaciones IPP (`job-created`) por sondeo corto contra cupsd local ---

const SUB_TEST: &str = "{
	NAME \"Create-Printer-Subscription\"
	OPERATION Create-Printer-Subscription
	GROUP operation-attributes-tag
	ATTR charset attributes-charset utf-8
	ATTR language attributes-natural-language en
	ATTR uri printer-uri $uri
	ATTR name requesting-user-name $user
	GROUP subscription-attributes-tag
	ATTR keyword notify-pull-method ippget
	ATTR keyword notify-events job-created
	ATTR integer notify-lease-duration 3600
}
";

fn ipp_attr(out: &mut Vec<u8>, tag: u8, name: &str, val: &[u8]) {
    out.push(tag);
    out.extend((name.len() as u16).to_be_bytes());
    out.extend(name.as_bytes());
    out.extend((val.len() as u16).to_be_bytes());
    out.extend(val);
}

/// Petición IPP Get-Notifications (sin esperar: cupsd responde al instante con los eventos pendientes).
pub fn get_notifications_request(sub: u32, seq: u32) -> Vec<u8> {
    let mut b = vec![2, 0, 0x00, 0x1C, 0, 0, 0, 1, 0x01];
    ipp_attr(&mut b, 0x47, "attributes-charset", b"utf-8");
    ipp_attr(&mut b, 0x48, "attributes-natural-language", b"en");
    ipp_attr(&mut b, 0x45, "printer-uri", b"ipp://localhost/");
    // La suscripción es del usuario que la creó (ipptool usa $user = USER): se pide con el mismo nombre.
    let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
    ipp_attr(&mut b, 0x42, "requesting-user-name", user.as_bytes());
    ipp_attr(&mut b, 0x21, "notify-subscription-ids", &sub.to_be_bytes());
    ipp_attr(&mut b, 0x21, "notify-sequence-numbers", &seq.to_be_bytes());
    b.push(0x03);
    b
}

/// Mayor `notify-sequence-number` de una respuesta IPP (`None` = sin eventos). Recorre las etiquetas sin asumir longitudes.
pub fn max_sequence(resp: &[u8]) -> Option<u32> {
    // Cabecera IPP: versión (2) + estado (2) + id (4); luego grupos de atributos.
    let mut i = 8;
    let mut name = String::new();
    let mut best = None;
    while i < resp.len() {
        let tag = resp[i];
        if tag == 0x03 {
            break;
        }
        if tag <= 0x0f {
            i += 1; // delimitador de grupo
            continue;
        }
        let nl = u16::from_be_bytes([*resp.get(i + 1)?, *resp.get(i + 2)?]) as usize;
        let ns = i + 3;
        if nl > 0 {
            name = String::from_utf8_lossy(resp.get(ns..ns + nl)?).into_owned();
        }
        let vi = ns + nl;
        let vl = u16::from_be_bytes([*resp.get(vi)?, *resp.get(vi + 1)?]) as usize;
        let val = resp.get(vi + 2..vi + 2 + vl)?;
        if name == "notify-sequence-number" && tag == 0x21 && val.len() == 4 {
            let n = u32::from_be_bytes([val[0], val[1], val[2], val[3]]);
            best = Some(best.map_or(n, |b: u32| b.max(n)));
        }
        i = vi + 2 + vl;
    }
    best
}

/// Id de la suscripción ippget (se crea con `ipptool`; caduca a la hora).
fn create_subscription(work_dir: &Path) -> Result<u32, String> {
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let test = work_dir.join("subscription.test");
    fs::write(&test, SUB_TEST).map_err(|e| e.to_string())?;
    let out = run("ipptool", &["-tv", "ipp://localhost/", test.to_str().unwrap_or("")])?;
    out.lines()
        .filter_map(|l| l.trim().split_once(" = ").filter(|(k, _)| k.starts_with("notify-subscription-id ")).and_then(|(_, v)| v.trim().parse().ok()))
        .next_back()
        .ok_or_else(|| "sin notify-subscription-id".to_string())
}

fn poll_notifications(sub: u32, seq: u32) -> Result<Option<u32>, String> {
    use std::io::{Read, Write};
    let body = get_notifications_request(sub, seq);
    let mut c = std::net::TcpStream::connect_timeout(&"127.0.0.1:631".parse().unwrap(), std::time::Duration::from_secs(2)).map_err(|e| e.to_string())?;
    c.set_read_timeout(Some(std::time::Duration::from_secs(3))).ok();
    let head = format!("POST / HTTP/1.1\r\nHost: localhost:631\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    c.write_all(head.as_bytes()).and_then(|_| c.write_all(&body)).map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    c.read_to_end(&mut resp).map_err(|e| e.to_string())?;
    let at = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or("respuesta HTTP inválida")? + 4;
    let ipp = &resp[at..];
    if ipp.len() < 8 || ipp[2] != 0 || ipp[3] > 1 {
        return Err("Get-Notifications rechazado".into()); // 0x0000/0x0001 = successful-ok
    }
    Ok(max_sequence(ipp))
}

/// Hilo que avisa (`wake`) en cuanto CUPS crea un trabajo, para retenerlo antes de que se imprima.
/// Sin suscripción posible (cupsd sin notificaciones) no hace nada: el sondeo normal sigue de respaldo.
pub fn watch_new_jobs(work_dir: std::path::PathBuf, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || loop {
        let Ok(sub) = create_subscription(&work_dir) else {
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        };
        let started = std::time::Instant::now();
        let mut seq = 1;
        while started.elapsed() < std::time::Duration::from_secs(3500) {
            match poll_notifications(sub, seq) {
                Ok(Some(n)) => {
                    seq = n + 1;
                    wake();
                }
                Ok(None) => {}
                Err(_) => break,
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    });
}

/// Carpeta donde la app deja el veredicto de cada trabajo para el filtro de CUPS (`scripts/guard-cups.sh`).
#[cfg(target_os = "macos")]
const VERDICTS_DIR: &str = "/Library/Application Support/SpoolGuard/verdicts";
#[cfg(not(target_os = "macos"))]
const VERDICTS_DIR: &str = "/var/lib/spoolguard/verdicts";

/// Instalador del filtro (se ejecuta como administrador: `sh guard-cups.sh install`).
pub const GUARD_SCRIPT: &str = include_str!("../../scripts/guard-cups.sh");

/// Colas con filtro de SpoolGuard y colas con PPD (las demás son RAW y no pasan por filtros).
pub fn guard_status() -> (usize, usize) {
    guard_status_of(None)
}

/// Igual que `guard_status`, pero limitado a `only` (p. ej. las colas con control activo); `None` = todas.
pub fn guard_status_of(only: Option<&[String]>) -> (usize, usize) {
    let (mut on, mut total) = (0, 0);
    for q in list_printers().unwrap_or_default() {
        if !valid_queue(&q) || only.is_some_and(|o| !o.contains(&q)) {
            continue;
        }
        if let Ok(ppd) = fs::read_to_string(format!("/etc/cups/ppd/{q}.ppd")) {
            total += 1;
            on += ppd.contains("spoolguard-") as usize;
        }
    }
    (on, total)
}

/// Deja el veredicto (`allow`/`deny`) que el filtro previo a la impresión espera. Sin la carpeta (filtro no instalado) no hace nada.
pub fn write_verdict(printer: &str, id: u32, allowed: bool) {
    let dir = Path::new(VERDICTS_DIR);
    if !dir.is_dir() || !valid_queue(printer) {
        return;
    }
    let tmp = dir.join(format!(".{printer}-{id}.tmp"));
    let dst = dir.join(format!("{printer}-{id}"));
    if fs::write(&tmp, if allowed { "allow" } else { "deny" }).is_ok() && fs::rename(&tmp, &dst).is_err() {
        let _ = fs::remove_file(&tmp);
    }
    // Veredictos de más de un día: se limpian de paso (sin hilo aparte).
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name() == "config" || e.file_name().to_string_lossy().starts_with("controlled-") {
                continue; // la configuración y las marcas de control no caducan
            }
            if e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() > 86_400) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// Marca las colas con control (`verdicts/controlled-<cola>`) y borra las marcas de las demás: el filtro solo espera el
/// veredicto en las marcadas; una cola protegida sin control deja pasar los trabajos (también en modo estricto).
/// Sin la carpeta de veredictos (protección no instalada) no hace nada.
pub fn write_controlled(targets: &[String]) {
    write_controlled_in(Path::new(VERDICTS_DIR), targets);
}

fn write_controlled_in(dir: &Path, targets: &[String]) {
    if !dir.is_dir() {
        return;
    }
    for t in targets.iter().filter(|t| valid_queue(t)) {
        let m = dir.join(format!("controlled-{t}"));
        if !m.exists() {
            let _ = fs::write(&m, b"");
        }
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(q) = n.strip_prefix("controlled-") {
                if !targets.iter().any(|t| t == q) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
}

/// Deja la espera del veredicto (1-600 s) y el modo estricto para el filtro (`verdicts/config`, líneas `timeout=` y `strict=`),
/// de forma atómica y solo si cambió. Fuera de rango = 10 s. Sin la carpeta no hace nada.
pub fn write_config(timeout_secs: u64, strict: bool) {
    static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let dir = Path::new(VERDICTS_DIR);
    if !dir.is_dir() {
        return;
    }
    let t = if (1..=600).contains(&timeout_secs) { timeout_secs } else { 10 };
    let body = format!("timeout={t}\nstrict={}\n", u8::from(strict));
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let dst = dir.join("config");
    if last.as_deref() == Some(body.as_str()) && dst.is_file() {
        return;
    }
    let tmp = dir.join(".config.tmp");
    if fs::write(&tmp, &body).is_ok() {
        if fs::rename(&tmp, &dst).is_ok() {
            *last = Some(body);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
}

/// Retiene un trabajo que el cliente mandó sin retención (`job-hold-until=no-hold`): CUPS también retiene uno en proceso.
pub fn hold(id: u32) -> Result<(), String> {
    run("lp", &["-i", &id.to_string(), "-H", "hold"]).map(|_| ())
}

pub fn release(id: u32) -> Result<(), String> {
    run("lp", &["-i", &id.to_string(), "-H", "resume"]).map(|_| ())
}

/// Resultado de cancelar un trabajo retenido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancelled {
    /// Se canceló antes de que saliera.
    Held,
    /// Ya estaba imprimiéndose o impreso al cancelarlo: la retención no llegó a tiempo.
    AlreadyPrinted,
    /// Ya no estaba en la cola (impreso o borrado por otro antes de poder cancelarlo).
    Gone,
}

pub fn cancel(id: u32) -> Result<Cancelled, String> {
    run("cancel", &[&id.to_string()]).map(|_| Cancelled::Held)
}

#[cfg(test)]
mod tests {
    #[test]
    fn marcas_de_cola_controlada() {
        let d = std::env::temp_dir().join(format!("sg-marcas-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("config"), "timeout=10\nstrict=0\n").unwrap();
        write_controlled_in(&d, &["A".to_string(), "B".to_string()]);
        assert!(d.join("controlled-A").is_file() && d.join("controlled-B").is_file());
        write_controlled_in(&d, &["B".to_string(), "../x".to_string()]);
        assert!(!d.join("controlled-A").exists(), "se borra la marca de la cola sin control");
        assert!(d.join("controlled-B").is_file());
        assert!(d.join("config").is_file(), "no toca otros archivos");
        write_controlled_in(&d.join("no-existe"), &["A".to_string()]);
        let _ = fs::remove_dir_all(&d);
    }

    use super::*;

    fn m(v: &[(&str, &str)]) -> HashMap<String, String> {
        v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn secuencia_de_notificacion_ipp() {
        let mut r = vec![2, 0, 0, 0, 0, 0, 0, 1, 0x01];
        ipp_attr(&mut r, 0x47, "attributes-charset", b"utf-8");
        r.push(0x07);
        ipp_attr(&mut r, 0x21, "notify-sequence-number", &5u32.to_be_bytes());
        ipp_attr(&mut r, 0x44, "notify-subscribed-event", b"job-created");
        r.push(0x07);
        ipp_attr(&mut r, 0x21, "notify-sequence-number", &7u32.to_be_bytes());
        r.push(0x03);
        assert_eq!(max_sequence(&r), Some(7));
        assert_eq!(max_sequence(&[2, 0, 0, 0, 0, 0, 0, 1, 0x01, 0x03]), None);
        let q = get_notifications_request(672, 2);
        assert_eq!(&q[2..4], &[0, 0x1C]);
        assert!(q.windows(4).any(|w| w == 672u32.to_be_bytes()));
    }

    #[test]
    fn nombres_de_cola_seguros() {
        for ok in ["Oficina", "HP_LaserJet-400", "Taller.2", "impresora_ñ"] {
            assert!(valid_queue(ok), "{ok}");
        }
        for bad in ["", "-p", "--help", "a b", "a\nb", "a\rb", "a\0b", "a/b", "../x", "a#b", "a?b=1", "a\\b", "a\tb"] {
            assert!(!valid_queue(bad), "{bad:?}");
        }
        assert!(set_hold("-x", true, false).is_err());
        assert!(held_job_ids("a b").is_err());
        assert_eq!(device_uri(std::path::Path::new("."), "a/b"), "");
    }

    #[test]
    fn paginas_y_bytes_solo_si_cups_los_informa() {
        assert_eq!(sizes(&m(&[("job-impressions", "3"), ("job-k-octets", "12")])), (Some(3), Some(12288)));
        assert_eq!(sizes(&m(&[("job-impressions", "0"), ("job-k-octets", "0")])), (None, None));
        assert_eq!(sizes(&m(&[])), (None, None));
        assert_eq!(sizes(&m(&[("job-impressions", "x"), ("job-k-octets", "-4")])), (None, None));
        assert_eq!(sizes(&m(&[("job-k-octets", "99999999999999")])), (None, None));
    }
}
