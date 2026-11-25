use control_core::backup;
use control_core::cups::Cancelled;
use control_core::discover;
use control_core::inventory::InventoryRow;
use control_core::events::{Event, EventDb, Filter, HttpTransport, SummaryRow, SyncStatus, Syncer, Transport};
use control_core::rules::{Decision, PrinterRules};
use serde::{Deserialize, Serialize};
use std::{
    time::Instant,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{
    image::Image,
    menu::{AboutMetadata, Menu, MenuBuilder, MenuItem, SubmenuBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Wry, WindowEvent,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_notification::NotificationExt;

mod browser;
mod printers;
mod pw;

#[cfg(unix)]
mod queue {
    pub use control_core::cups::*;
}
#[cfg(windows)]
mod queue {
    pub use control_core::spooler::*;
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Settings {
    /// Qué hacer con trabajos sin datos suficientes: "allow" | "block".
    unknown_policy: String,
    interval_secs: u64,
    notify_block: bool,
    notify_allow: bool,
    theme: String,
    retention_days: u64,
    /// SHA-256 de sal + contraseña; vacío = sin contraseña.
    password_hash: String,
    password_salt: String,
    /// La pausa sobrevive a reinicios.
    paused: bool,
    /// Enviar los eventos al servidor (apagado por defecto: sin esto no se abre ninguna conexión de red).
    send_events: bool,
    /// Dirección del servidor (http:// o https://).
    server_url: String,
    /// Token de equipo (`sgm_…`) creado en el servidor. get_settings solo devuelve "set" si existe.
    server_token: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            unknown_policy: "allow".into(),
            interval_secs: 1,
            notify_block: true,
            notify_allow: false,
            theme: "system".into(),
            retention_days: 90,
            password_hash: String::new(),
            password_salt: String::new(),
            paused: false,
            send_events: false,
            server_url: String::new(),
            server_token: String::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize, Clone)]
#[serde(default)]
struct Config {
    printers: HashMap<String, PrinterRules>,
    /// Regla que aplica a todas las impresoras (se suma a la propia de cada una).
    global: PrinterRules,
    settings: Settings,
}

/// Fila del registro para la interfaz (los primeros campos son los de siempre).
#[derive(Serialize, Clone)]
struct Activity {
    id: String,
    time: u64,
    printer: String,
    document: String,
    user: Option<String>,
    origin: Option<String>,
    allowed: bool,
    reason: String,
    host: String,
    site: String,
    app: String,
    decision: String,
    status: String,
    port: String,
    conn_type: String,
    pages: Option<u64>,
    bytes: Option<u64>,
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

impl From<Event> for Activity {
    fn from(e: Event) -> Activity {
        Activity {
            id: e.id,
            time: e.ts,
            printer: e.printer,
            document: e.document,
            user: non_empty(e.user),
            origin: non_empty(e.ip),
            allowed: e.decision == "allow",
            reason: e.reason,
            host: e.host,
            site: e.site,
            app: e.app,
            decision: e.decision,
            status: e.status,
            port: e.port,
            conn_type: e.conn_type,
            pages: e.pages,
            bytes: e.bytes,
        }
    }
}

struct State {
    dir: PathBuf,
    config: Mutex<Config>,
    events: EventDb,
    /// Nombre de este equipo (columna `host` de los eventos).
    host: String,
    sync: Mutex<Syncer>,
    paused: AtomicBool,
    /// Con contraseña definida, los comandos que modifican estado exigen haberla verificado.
    unlocked: AtomicBool,
    /// Descubrimiento de red en curso o el último terminado.
    disc: Mutex<DiscJob>,
    disc_cancel: AtomicBool,
}

fn guard(s: &State) -> Result<(), String> {
    if s.unlocked.load(Ordering::Relaxed) { Ok(()) } else { Err("La aplicación está bloqueada".into()) }
}

type Shared = Arc<State>;

/// Elementos del menú de la bandeja que el servicio actualiza.
struct TrayItems {
    status: MenuItem<Wry>,
    pause: MenuItem<Wry>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Cola retenida por esta app y su estado previo.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
struct Managed {
    /// La cola ya estaba pausada (por el usuario) antes de retenerla: al liberar no se reanuda.
    #[serde(default)]
    pausada_antes: bool,
}

/// Lee managed.json; acepta el formato anterior (lista de nombres) y el actual (nombre → Managed).
fn parse_managed(b: &[u8]) -> BTreeMap<String, Managed> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum File {
        Map(BTreeMap<String, Managed>),
        Old(BTreeSet<String>),
    }
    match serde_json::from_slice(b) {
        Ok(File::Map(m)) => m,
        Ok(File::Old(l)) => l.into_iter().map(|p| (p, Managed::default())).collect(),
        Err(_) => BTreeMap::new(),
    }
}

/// Colas que esta app retiene (managed.json). Permite liberar las que queden huérfanas
/// (regla borrada, config restablecida, cierre inesperado) en vez de dejar trabajos en pausa.
fn managed(dir: &std::path::Path) -> BTreeMap<String, Managed> {
    fs::read(dir.join("managed.json")).map(|b| parse_managed(&b)).unwrap_or_default()
}

/// Serializa todo cambio de retención (managed.json + pausa de la cola) y las decisiones «esta cola ya no
/// debe retenerse». Sin él, set_rules (que retiene y DESPUÉS guarda la regla) podía cruzarse con el worker,
/// que veía la cola retenida sin regla, la daba por huérfana y la liberaba, y dos apply_hold simultáneos
/// perdían entradas de managed.json (lectura-modificación-escritura).
static HOLD_LOCK: Mutex<()> = Mutex::new(());

fn hold_guard() -> std::sync::MutexGuard<'static, ()> {
    HOLD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Colas cuyo fallo de retención ya se avisó: el arranque y el worker intentan retener la misma cola y el aviso
/// salía dos veces. Se olvida cuando la retención se logra.
static HOLD_NOTICED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn notice_hold_failed(s: &State, printer: &str) {
    let first = HOLD_NOTICED.lock().unwrap_or_else(|e| e.into_inner()).insert(printer.to_string());
    if first {
        log_notice(s, format!("No se pudo retener la cola de {printer} (¿no admite pausa?)"));
    }
}

fn apply_hold(dir: &std::path::Path, printer: &str, on: bool) -> Result<(), String> {
    let _g = hold_guard();
    apply_hold_locked(dir, printer, on)
}

/// Como `apply_hold`, con `hold_guard()` ya tomado por quien llama.
fn apply_hold_locked(dir: &std::path::Path, printer: &str, on: bool) -> Result<(), String> {
    let mut m = managed(dir);
    if on {
        // El estado previo se registra solo la primera vez: después la cola ya está pausada por nosotros.
        let before = m.get(printer).cloned().unwrap_or_else(|| Managed { pausada_antes: queue::is_paused(printer) });
        queue::set_hold(printer, true, false)?;
        HOLD_NOTICED.lock().unwrap_or_else(|e| e.into_inner()).remove(printer);
        m.insert(printer.to_string(), before);
    } else {
        queue::set_hold(printer, false, m.get(printer).is_some_and(|e| e.pausada_antes))?;
        m.remove(printer);
    }
    let tmp = dir.join("managed.json.tmp");
    write_private(&tmp, &serde_json::to_vec(&m).unwrap()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, dir.join("managed.json")).map_err(|e| e.to_string())
}

/// Quita la retención y libera los trabajos que hayan quedado pendientes.
fn free_queue(dir: &std::path::Path, printer: &str) {
    let _g = hold_guard();
    free_queue_locked(dir, printer);
}

fn free_queue_locked(dir: &std::path::Path, printer: &str) {
    let _ = apply_hold_locked(dir, printer, false);
    for id in queue::held_job_ids(printer).unwrap_or_default() {
        let _ = queue::release(id);
    }
}

/// Libera la cola solo si, con la configuración ACTUAL (leída bajo `hold_guard`), ya no debe retenerse.
fn free_if_unwanted(s: &State, printer: &str) {
    let _g = hold_guard();
    let cfg = s.config.lock().unwrap().clone();
    if !wants_hold(&cfg, printer) {
        free_queue_locked(&s.dir, printer);
    }
}

/// Escribe un archivo que solo el dueño puede leer (0600 desde su creación, sin ventana con permisos más amplios).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(path)?;
    #[cfg(unix)]
    f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?; // si ya existía con otros permisos
    f.write_all(bytes)
}

/// Cola cuya retención falló; `denied`: Windows negó administrarla (falta permiso), no que no admita pausa.
#[derive(Debug, PartialEq)]
struct HoldFailure {
    printer: String,
    denied: bool,
}

impl HoldFailure {
    fn new(printer: &str, err: &str) -> Self {
        HoldFailure { printer: printer.to_string(), denied: err.starts_with(control_core::ACCESS_DENIED) }
    }
}

/// Aviso para la UI con las colas cuya retención falló; None si no hubo fallos. Distingue la falta de
/// permiso (se puede conceder) de las colas que no admiten pausa (Fax).
fn hold_warning(failed: &[HoldFailure]) -> Option<String> {
    let names = |denied: bool| failed.iter().filter(|f| f.denied == denied).map(|f| f.printer.as_str()).collect::<Vec<_>>().join(", ");
    let (denied, other) = (names(true), names(false));
    let mut parts = vec![];
    if !denied.is_empty() {
        parts.push(format!("Sin permiso para administrar la cola de: {denied}. Concede el permiso desde la impresora."));
    }
    if !other.is_empty() {
        parts.push(format!("No se pudo cambiar la retención de: {other}. Es posible que no admitan pausa."));
    }
    (!parts.is_empty()).then(|| format!("{} El resto quedó configurado.", parts.join(" ")))
}

/// Reajusta la retención de `printers` según `cfg`. Un fallo en una cola no detiene las demás:
/// devuelve las que fallaron.
/// `hold_guard()` debe estar tomado por quien llama, y `cfg` leída después de tomarlo.
fn sync_holds_locked(dir: &std::path::Path, cfg: &Config, printers: &[String]) -> Vec<HoldFailure> {
    let mut failed = vec![];
    for p in printers {
        if wants_hold(cfg, p) {
            if let Err(e) = apply_hold_locked(dir, p, true) {
                failed.push(HoldFailure::new(p, &e));
            }
        } else if managed(dir).contains_key(p) {
            free_queue_locked(dir, p);
        }
    }
    failed
}

/// ¿Debe retenerse la cola? Con control propio o con la regla global activa.
fn wants_hold(cfg: &Config, printer: &str) -> bool {
    cfg.global.enabled || cfg.printers.get(printer).map(|r| r.enabled).unwrap_or(false)
}

/// ¿Hay que (re)registrar el inicio con el sistema? `prev`: contenido de la marca autostart.init
/// (None = primera ejecución, "1" = marca antigua sin ruta, otro = exe que lo registró); `enabled`: ya registrado.
fn autostart_needs_register(prev: Option<&str>, exe: &str, enabled: bool) -> bool {
    match prev.map(str::trim) {
        None | Some("1") => true,
        Some(p) if p == exe => false,
        Some(_) => enabled,
    }
}

/// Colas que esta app retiene (managed.json) pero que según `cfg` ya no deben retenerse.
fn orphan_queues(cfg: &Config, managed: &BTreeMap<String, Managed>) -> Vec<String> {
    managed.keys().filter(|p| !wants_hold(cfg, p)).cloned().collect()
}

/// Escritura atómica (archivo temporal + renombrar): un corte de luz no deja config.json truncado.
fn save(s: &State) -> Result<(), String> {
    let c = s.config.lock().unwrap();
    let tmp = s.dir.join("config.json.tmp");
    // config.json contiene el hash de la contraseña y el token de equipo: 0600 desde su creación.
    write_private(&tmp, &serde_json::to_vec_pretty(&*c).unwrap()).map_err(|e| e.to_string())?;
    let cur = s.dir.join("config.json");
    // Respaldo de la última configuración válida (si el archivo actual parsea), también 0600.
    if let Some(b) = fs::read(&cur).ok().filter(|b| serde_json::from_slice::<Config>(b).is_ok()) {
        let _ = write_private(&s.dir.join("config.json.bak"), &b);
    }
    fs::rename(&tmp, cur).map_err(|e| e.to_string())
}

/// Carga la configuración; si está corrupta usa el respaldo y conserva el archivo dañado.
/// Devuelve también un aviso para el registro.
fn load_config(dir: &std::path::Path) -> (Config, Option<String>) {
    let read = |f: &str| fs::read(dir.join(f)).ok().and_then(|b| serde_json::from_slice::<Config>(&b).ok());
    let exists = dir.join("config.json").exists();
    if let Some(c) = read("config.json") {
        return (c, None);
    }
    if !exists {
        return (Config::default(), None);
    }
    let _ = fs::rename(dir.join("config.json"), dir.join(format!("config.json.corrupto-{}", now())));
    match read("config.json.bak") {
        Some(c) => {
            let _ = write_private(&dir.join("config.json"), &serde_json::to_vec_pretty(&c).unwrap());
            (c, Some("Configuración dañada: se restauró el último respaldo válido".into()))
        }
        None => (Config::default(), Some("Configuración dañada sin respaldo: se inició con valores por defecto".into())),
    }
}

#[derive(Serialize)]
struct PrinterView {
    name: String,
    rules: PrinterRules,
    info: printers::PrinterInfo,
}

#[tauri::command]
async fn list_printers(s: tauri::State<'_, Shared>) -> Result<Vec<PrinterView>, String> {
    guard(&s)?;
    let names = queue::list_printers()?;
    let rules: Vec<PrinterRules> = {
        let c = s.config.lock().unwrap();
        names.iter().map(|n| c.printers.get(n).cloned().unwrap_or_default()).collect()
    };
    // Sondeo en paralelo: cada impresora de red puede tardar hasta ~1 s en responder.
    let dir = s.dir.clone();
    let usb = printers::usb_devices();
    let infos: Vec<printers::PrinterInfo> = thread::scope(|sc| {
        let hs: Vec<_> = names.iter().map(|n| { let (d, u) = (dir.clone(), &usb); sc.spawn(move || printers::info(&d, n, u)) }).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    Ok(names.into_iter().zip(rules).zip(infos).map(|((name, rules), info)| PrinterView { name, rules, info }).collect())
}

#[tauri::command]
fn set_rules(s: tauri::State<Shared>, printer: String, rules: PrinterRules) -> Result<Option<String>, String> {
    guard(&s)?;
    // Solo impresoras reales del sistema (un nombre con '-' inicial se leería como opción).
    if printer.starts_with('-') || !queue::list_printers()?.contains(&printer) {
        return Err("Impresora desconocida".into());
    }
    // Retener y guardar la regla es una sola operación para el worker (ver HOLD_LOCK).
    let failed: Vec<HoldFailure> = {
        let _g = hold_guard();
        let hold = rules.enabled || s.config.lock().unwrap().global.enabled;
        // Si la cola no admite pausa (Fax) la regla se guarda igual y se avisa.
        let failed = match apply_hold_locked(&s.dir, &printer, hold) {
            // Sin permiso de administración: no se guarda nada y la interfaz ofrece concederlo (grant_printer_access).
            Err(e) if e.starts_with(control_core::ACCESS_DENIED) => return Err(e),
            r => r.err().map(|e| vec![HoldFailure::new(&printer, &e)]).unwrap_or_default(),
        };
        s.config.lock().unwrap().printers.insert(printer, rules);
        failed
    };
    save(&s)?;
    Ok(hold_warning(&failed))
}

/// Windows: añade a la cola un ACE para Usuarios autenticados (AU) con los mismos derechos que el ACE de
/// Administradores (BA) del descriptor por defecto: administrar impresora y documentos. Pide UAC una vez.
#[tauri::command]
fn grant_printer_access(s: tauri::State<Shared>, printer: String) -> Result<(), String> {
    guard(&s)?;
    #[cfg(not(windows))]
    {
        let _ = printer;
        Err("Solo disponible en Windows".into())
    }
    #[cfg(windows)]
    {
        use base64::Engine;
        if !queue::list_printers()?.contains(&printer) {
            return Err("Impresora desconocida".into());
        }
        let script = format!(
            "$ErrorActionPreference='Stop'; $n='{}'; $s=(Get-Printer -Full -Name $n).PermissionSDDL; \
             if ($s -notmatch '\\(A;[A-Z]*;[A-Z]*;;;AU\\)') {{ Set-Printer -Name $n -PermissionSDDL ($s + '(A;;LCSWSDRCWDWO;;;AU)(A;OIIO;RPWPSDRCWDWO;;;AU)') }}",
            printer.replace('\'', "''")
        );
        let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let enc = base64::engine::general_purpose::STANDARD.encode(utf16);
        // El PowerShell elevado hereda el código de salida mediante -PassThru.
        let outer = format!(
            "$p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile','-EncodedCommand','{enc}'; exit $p.ExitCode"
        );
        let out = printers::no_window(std::process::Command::new("powershell").args(["-NoProfile", "-Command", &outer]))
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err("No se concedió el permiso (¿se canceló la confirmación de administrador?)".into()) }
    }
}

#[tauri::command]
fn get_global_rules(s: tauri::State<Shared>) -> PrinterRules {
    if guard(&s).is_err() {
        return PrinterRules::default(); // bloqueada: no se revelan las reglas
    }
    s.config.lock().unwrap().global.clone()
}

/// Resultado de `set_global_rules`: aviso y colas sin permiso de administración (la UI ofrece concederlo).
#[derive(Serialize)]
struct GlobalResult {
    warning: Option<String>,
    denied: Vec<String>,
}

#[tauri::command]
fn set_global_rules(s: tauri::State<Shared>, rules: PrinterRules) -> Result<GlobalResult, String> {
    guard(&s)?;
    let names = queue::list_printers()?;
    // Reajusta la retención de todas las colas según la nueva regla global (ver HOLD_LOCK).
    let failed = {
        let _g = hold_guard();
        s.config.lock().unwrap().global = rules;
        let cfg = s.config.lock().unwrap().clone();
        sync_holds_locked(&s.dir, &cfg, &names)
    };
    save(&s)?;
    Ok(GlobalResult {
        warning: hold_warning(&failed),
        denied: failed.iter().filter(|f| f.denied).map(|f| f.printer.clone()).collect(),
    })
}

/// Últimos eventos (más antiguo primero), para compatibilidad; la vista Registro usa query_events.
#[tauri::command]
fn get_activity(s: tauri::State<Shared>) -> Vec<Activity> {
    if guard(&s).is_err() {
        return vec![];
    }
    let mut v: Vec<Activity> = s.events.query(&Filter::default(), 2000, 0).map(|p| p.rows).unwrap_or_default().into_iter().map(Activity::from).collect();
    v.reverse();
    v
}

#[derive(Serialize)]
struct ActivityPage {
    rows: Vec<Activity>,
    total: u64,
    printers: Vec<String>,
}

/// Registro con filtros y paginación (página desde 1, máx. 500 filas).
#[tauri::command]
fn query_events(s: tauri::State<Shared>, filter: Option<Filter>, page: Option<u32>, size: Option<u32>) -> Result<ActivityPage, String> {
    guard(&s)?;
    let size = size.unwrap_or(25).clamp(1, 500);
    let page = page.unwrap_or(1).max(1);
    let p = s.events.query(&filter.unwrap_or_default(), size, (page - 1).saturating_mul(size))?;
    Ok(ActivityPage { rows: p.rows.into_iter().map(Activity::from).collect(), total: p.total, printers: s.events.printers().unwrap_or_default() })
}

#[tauri::command]
fn clear_log(s: tauri::State<Shared>) -> Result<(), String> {
    guard(&s)?;
    s.events.clear()
}

#[tauri::command]
fn export_log_csv(s: tauri::State<Shared>, filter: Option<Filter>) -> Result<String, String> {
    guard(&s)?;
    s.events.export_csv(&filter.unwrap_or_default())
}

/// Totales por usuario, equipo, impresora o decisión ("Quién imprimió qué").
#[tauri::command]
fn summarize_events(s: tauri::State<Shared>, filter: Option<Filter>, by: String) -> Result<Vec<SummaryRow>, String> {
    guard(&s)?;
    s.events.summarize(&filter.unwrap_or_default(), &by)
}

// --- Descubrimiento de red (opcional; solo si el administrador lo inicia) ---

const NET_FILE: &str = "network.json";

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct NetSettings {
    /// Subredes que el administrador autoriza a probar (por defecto vacío).
    cidrs: Vec<String>,
}

fn load_net(dir: &std::path::Path) -> NetSettings {
    fs::read(dir.join(NET_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

#[tauri::command]
fn get_network_settings(s: tauri::State<Shared>) -> serde_json::Value {
    if guard(&s).is_err() {
        return serde_json::json!({"cidrs": [], "direct": {"enabled": false, "interval_secs": 300}, "direct_available": false, "direct_status": [],
            "limits": {"max_hosts": discover::MAX_HOSTS, "min_prefix": discover::MIN_PREFIX}});
    }
    // La detección de impresión directa es solo del servidor: aquí no está disponible.
    serde_json::json!({"cidrs": load_net(&s.dir).cidrs, "direct": {"enabled": false, "interval_secs": 300}, "direct_available": false, "direct_status": [],
        "limits": {"max_hosts": discover::MAX_HOSTS, "min_prefix": discover::MIN_PREFIX}})
}

#[tauri::command(rename_all = "snake_case")]
fn set_network_settings(s: tauri::State<Shared>, cidrs: Vec<String>) -> Result<(), String> {
    guard(&s)?;
    let clean: Vec<String> = cidrs.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    if !clean.is_empty() {
        // Se valida la forma; el permiso para rangos públicos se pide en cada consulta.
        discover::parse_cidrs(&clean, true)?;
    }
    let b = serde_json::to_vec_pretty(&NetSettings { cidrs: clean }).map_err(|e| e.to_string())?;
    write_private(&s.dir.join(NET_FILE), &b).map_err(|e| e.to_string())
}

#[derive(Serialize, Default, Clone)]
struct DiscJob {
    running: bool,
    started: u64,
    finished: u64,
    by: String,
    ranges: Vec<String>,
    hosts: usize,
    done: usize,
    total: usize,
    devices: Vec<discover::DeviceView>,
    error: String,
}

/// Quién ejecuta una acción de administración en el escritorio: el usuario del sistema.
fn local_user() -> String {
    ["USER", "USERNAME"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty())).unwrap_or_else(|| "usuario local".into())
}

/// Colas con dispositivo de red y si tienen control activo.
fn queue_refs(s: &State) -> Vec<discover::QueueRef> {
    let cfg = s.config.lock().unwrap().clone();
    queue::list_printers().unwrap_or_default().into_iter().filter_map(|n| {
        let host = printers::queue_host(&s.dir, &n);
        (!host.is_empty()).then(|| discover::QueueRef { controlled: wants_hold(&cfg, &n), name: n, host })
    }).collect()
}

/// Resuelve el nombre del equipo de origen de un trabajo a una IP (la primera IPv4) para las reglas por IP.
/// Solo se llama desde el hilo de evaluación, nunca desde el vigilante. La consulta tiene límite de tiempo
/// (un nombre que no resuelve no detiene la evaluación más de ~1 s) y se recuerda: 10 min si resolvió, 2 min si no.
/// Los equipos de origen los informa el cliente; un nombre que no resuelve (NetBIOS sin DNS) solo sirve para reglas por nombre.
fn resolve_origin(name: &str) -> Option<String> {
    static CACHE: Mutex<BTreeMap<String, (Option<String>, Instant)>> = Mutex::new(BTreeMap::new());
    let key = name.trim().to_lowercase();
    if let Some((ip, at)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < Duration::from_secs(if ip.is_some() { 600 } else { 120 }) {
            return ip.clone();
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let n = key.clone();
    thread::spawn(move || {
        let _ = tx.send(resolve_v4(&n).into_iter().next());
    });
    let ip = rx.recv_timeout(Duration::from_millis(1000)).ok().flatten();
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if c.len() > 256 {
        c.clear();
    }
    c.insert(key, (ip.clone(), Instant::now()));
    ip
}

fn resolve_v4(host: &str) -> Vec<String> {
    use std::net::ToSocketAddrs;
    (host, 0u16).to_socket_addrs().map(|it| it.filter_map(|a| match a.ip() { std::net::IpAddr::V4(v) => Some(v.to_string()), _ => None }).collect()).unwrap_or_default()
}

/// Inicia el descubrimiento (en segundo plano; el avance se consulta con `discover_status`).
/// `ports` solo cambia en las pruebas (en la aplicación son 9100/515/631).
fn start_discovery(s: &Shared, cidrs: Vec<String>, confirm_public: bool, ports: discover::Ports, who: String) -> Result<(), String> {
    let cidrs = if cidrs.iter().any(|c| !c.trim().is_empty()) { cidrs } else { load_net(&s.dir).cidrs };
    let hosts = discover::parse_cidrs(&cidrs, confirm_public)?;
    let ranges: Vec<String> = cidrs.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    {
        let mut j = s.disc.lock().unwrap();
        if j.running {
            return Err("Ya hay un descubrimiento en curso".into());
        }
        *j = DiscJob { running: true, started: now(), by: who.clone(), ranges: ranges.clone(), hosts: hosts.len(), total: hosts.len() * 3, ..Default::default() };
    }
    s.disc_cancel.store(false, Ordering::Relaxed);
    log_notice(s, format!("Descubrimiento de red iniciado por {who}. Rangos: {} ({} hosts; solo TCP a {}/{}/{})", ranges.join(", "), hosts.len(), ports.raw, ports.lpd, ports.ipp));
    let s = s.clone();
    thread::spawn(move || {
        let devs = discover::scan(&hosts, ports, &discover::tcp_open, &discover::http_transport, &s.disc_cancel, &|d, t| {
            let mut j = s.disc.lock().unwrap();
            j.done = d;
            j.total = t;
        });
        let views = discover::classify(devs, &queue_refs(&s), &resolve_v4);
        let risk = views.iter().filter(|v| v.managed == "sin_cola").count();
        let cancelled = s.disc_cancel.load(Ordering::Relaxed);
        {
            let mut j = s.disc.lock().unwrap();
            j.devices = views;
            j.running = false;
            j.finished = now();
            if cancelled {
                j.error = "Descubrimiento interrumpido: el resultado puede estar incompleto".into();
            }
        }
        let n = s.disc.lock().unwrap().devices.len();
        log_notice(&s, format!("Descubrimiento de red terminado (iniciado por {who}): {n} dispositivos con puertos de impresión abiertos, {risk} sin cola en este equipo"));
    });
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
fn discover_network(s: tauri::State<Shared>, cidrs: Option<Vec<String>>, confirm_public: Option<bool>) -> Result<DiscJob, String> {
    guard(&s)?;
    start_discovery(&s, cidrs.unwrap_or_default(), confirm_public.unwrap_or(false), discover::DEFAULT_PORTS, format!("{} (escritorio)", local_user()))?;
    Ok(s.disc.lock().unwrap().clone())
}

#[tauri::command]
fn discover_status(s: tauri::State<Shared>) -> DiscJob {
    if guard(&s).is_err() {
        return DiscJob::default();
    }
    s.disc.lock().unwrap().clone()
}

#[tauri::command]
fn discover_cancel(s: tauri::State<Shared>) {
    if guard(&s).is_err() {
        return;
    }
    s.disc_cancel.store(true, Ordering::Relaxed);
}

/// Estado del envío al servidor (cola pendiente, último envío correcto y último error).
#[tauri::command]
fn get_sync_status(s: tauri::State<Shared>) -> SyncStatus {
    s.sync.lock().unwrap().status(&s.events)
}

#[tauri::command]
fn get_settings(s: tauri::State<Shared>) -> Settings {
    let mut st = s.config.lock().unwrap().settings.clone();
    st.password_hash = if st.password_hash.is_empty() { String::new() } else { "set".into() };
    st.password_salt.clear();
    st.server_token = if st.server_token.is_empty() { String::new() } else { "set".into() };
    st
}

#[tauri::command]
fn set_settings(s: tauri::State<Shared>, mut settings: Settings) -> Result<(), String> {
    guard(&s)?;
    {
        let mut c = s.config.lock().unwrap();
        // La contraseña solo cambia con set_password.
        settings.password_hash = c.settings.password_hash.clone();
        settings.password_salt = c.settings.password_salt.clone();
        settings.interval_secs = settings.interval_secs.clamp(1, 60);
        // El token solo se reemplaza con uno nuevo ("set" = sin cambios, vacío = quitarlo).
        if settings.server_token == "set" {
            settings.server_token = c.settings.server_token.clone();
        }
        settings.server_url = settings.server_url.trim().trim_end_matches('/').to_string();
        settings.server_token = settings.server_token.trim().to_string();
        if settings.send_events {
            // Valida antes de guardar: así nunca se queda activado un envío que no puede funcionar.
            HttpTransport::new(&settings.server_url, &settings.server_token).map(|_| ())?;
        }
        let changed = settings.server_url != c.settings.server_url || settings.server_token != c.settings.server_token || settings.send_events != c.settings.send_events;
        c.settings = settings;
        if changed {
            s.sync.lock().unwrap().reset();
        }
    }
    save(&s)
}

#[tauri::command]
fn has_password(s: tauri::State<Shared>) -> bool {
    !s.config.lock().unwrap().settings.password_hash.is_empty()
}

#[tauri::command]
fn verify_password(s: tauri::State<Shared>, password: String) -> bool {
    // El hash (lento a propósito) se calcula sin tener tomado el candado de la configuración.
    let (salt, stored) = {
        let c = s.config.lock().unwrap();
        (c.settings.password_salt.clone(), c.settings.password_hash.clone())
    };
    let ok = match pw::check(&password, &salt, &stored) {
        _ if stored.is_empty() => true,
        pw::Check::No => false,
        pw::Check::Ok => true,
        pw::Check::OkMigrate => {
            // Migración transparente al formato PBKDF2 (si nadie cambió la contraseña entretanto).
            let nuevo = pw::hash(&password);
            let mut c = s.config.lock().unwrap();
            if c.settings.password_hash == stored {
                c.settings.password_hash = nuevo;
                c.settings.password_salt.clear();
                drop(c);
                let _ = save(&s);
            }
            true
        }
    };
    if ok {
        s.unlocked.store(true, Ordering::Relaxed);
    }
    ok
}

/// Cambia la contraseña; si ya existe exige la actual. Vacía = quitarla.
#[tauri::command]
fn set_password(s: tauri::State<Shared>, current: String, new: String) -> Result<(), String> {
    if !verify_password(s.clone(), current) {
        return Err("Contraseña actual incorrecta".into());
    }
    let nuevo = if new.is_empty() { None } else { Some(pw::hash(&new)) };
    {
        let mut c = s.config.lock().unwrap();
        c.settings.password_salt.clear();
        c.settings.password_hash = nuevo.unwrap_or_default();
    }
    save(&s)
}

/// Rechaza rutas dentro del directorio de datos de la app (config, manifiestos, iconos), incluso vía symlinks.
fn outside_data_dir(s: &State, path: &str, new_file: bool) -> Result<(), String> {
    let p = PathBuf::from(path);
    let target = if new_file { p.parent().map(|d| d.to_path_buf()).unwrap_or_default() } else { p };
    let real = fs::canonicalize(&target).map_err(|e| e.to_string())?;
    let data = fs::canonicalize(&s.dir).unwrap_or_else(|_| s.dir.clone());
    if real.starts_with(&data) {
        return Err("Ruta no permitida".into());
    }
    Ok(())
}

/// Escribe un archivo elegido por el usuario en el diálogo nativo (solo .json y .csv).
#[tauri::command]
fn write_text_file(s: tauri::State<Shared>, path: String, content: String) -> Result<(), String> {
    guard(&s)?;
    outside_data_dir(&s, &path, true)?;
    let ok = path.ends_with(".json") || path.ends_with(".csv");
    if !ok || content.len() > 20_000_000 {
        return Err("Solo se pueden guardar archivos .json o .csv".into());
    }
    fs::write(&path, content).map_err(|e| e.to_string())
}

/// Lee un .json elegido por el usuario en el diálogo nativo (máx. 1 MB).
#[tauri::command]
fn read_text_file(s: tauri::State<Shared>, path: String) -> Result<String, String> {
    guard(&s)?;
    outside_data_dir(&s, &path, false)?;
    if !path.ends_with(".json") {
        return Err("Solo se pueden importar archivos .json".into());
    }
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 1_000_000 {
        return Err("El archivo es demasiado grande".into());
    }
    fs::read_to_string(&path).map_err(|e| e.to_string())
}


/// Lee una copia `.json` elegida por el usuario en el diálogo nativo (máx. 10 MiB).
#[tauri::command]
fn read_backup_file(s: tauri::State<Shared>, path: String) -> Result<String, String> {
    guard(&s)?;
    outside_data_dir(&s, &path, false)?;
    if !path.ends_with(".json") {
        return Err("Solo se pueden importar archivos .json".into());
    }
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > backup::MAX_BYTES as u64 {
        return Err("El archivo es demasiado grande (máx. 10 MiB)".into());
    }
    String::from_utf8(fs::read(&path).map_err(|e| e.to_string())?).map_err(|_| "El archivo no es texto UTF-8".to_string())
}

#[tauri::command]
fn extension_status(app: AppHandle, s: tauri::State<Shared>) -> Result<browser::ExtensionStatus, String> {
    guard(&s)?;
    browser::ensure(&s.dir, app.path().resource_dir().ok())
}

#[tauri::command]
fn open_extension(s: tauri::State<Shared>, what: String) -> Result<(), String> {
    guard(&s)?;
    browser::open(&s.dir, &what)
}

#[tauri::command]
fn list_browsers() -> Vec<browser::BrowserInfo> {
    browser::list()
}

#[tauri::command]
fn install_extension(app: AppHandle, s: tauri::State<Shared>, browser: String, profile: Option<String>) -> Result<browser::InstallResult, String> {
    guard(&s)?;
    browser::install(&s.dir, app.path().resource_dir().ok(), &browser, profile.as_deref())
}

#[tauri::command]
fn export_extension_zip(app: AppHandle, s: tauri::State<Shared>, path: String) -> Result<(), String> {
    guard(&s)?;
    if !path.to_lowercase().ends_with(".zip") {
        return Err("Solo se puede guardar un archivo .zip".into());
    }
    outside_data_dir(&s, &path, true)?;
    browser::export_zip(&s.dir, app.path().resource_dir().ok(), std::path::Path::new(&path))
}

/// Protección previa a la impresión (filtro de CUPS): (colas protegidas, colas con filtros, plataforma compatible).
#[tauri::command]
fn guard_status() -> (usize, usize, bool) {
    #[cfg(unix)]
    {
        let (on, total) = queue::guard_status();
        (on, total, true)
    }
    #[cfg(not(unix))]
    (0, 0, false)
}

/// Instala el filtro en las colas de CUPS con permisos de administrador (diálogo nativo de contraseña).
#[tauri::command]
fn guard_install(s: tauri::State<Shared>) -> Result<String, String> {
    guard(&s)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let user = std::env::var("USER").map_err(|_| "No se conoce el usuario".to_string())?;
        if !user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
            return Err("Nombre de usuario no admitido".into());
        }
        // Script en una carpeta privada del usuario: solo se escribe aquí y se ejecuta como administrador.
        let script = s.dir.join("guard-cups.sh");
        std::fs::write(&script, queue::GUARD_SCRIPT).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let sp = script.to_string_lossy().to_string();
        if sp.contains(['\'', '"', '\\']) {
            return Err("Ruta de datos no admitida".into());
        }
        #[cfg(target_os = "macos")]
        let out = std::process::Command::new("osascript")
            .args(["-e", &format!("do shell script \"SG_OWNER={user} /bin/sh '{sp}' install\" with administrator privileges")])
            .output();
        #[cfg(not(target_os = "macos"))]
        let out = std::process::Command::new("pkexec").args(["env", &format!("SG_OWNER={user}"), "/bin/sh", &sp, "install"]).output();
        let out = out.map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
    #[cfg(not(unix))]
    Err("En Windows el spooler ya retiene los trabajos; no hace falta".into())
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, s: tauri::State<Shared>, enabled: bool) -> Result<(), String> {
    guard(&s)?;
    use tauri_plugin_autostart::ManagerExt;
    let m = app.autolaunch();
    let r = (if enabled { m.enable() } else { m.disable() }).map_err(|e| e.to_string());
    #[cfg(windows)]
    if enabled {
        quote_run_value(&app.package_info().name);
    }
    r
}

/// `auto-launch` (bajo tauri-plugin-autostart) escribe el valor `Run` de Windows como `<ruta> <argumentos>` sin
/// comillas: con espacios en la ruta (`C:\Program Files\...`) es ambiguo y abre la puerta a que se ejecute otro
/// programa. Se reescribe entre comillas. Se mira HKCU y HKLM (el plugin usa HKLM si la app corre elevada).
#[cfg(windows)]
fn quote_run_value(name: &str) {
    for root in ["HKCU", "HKLM"] {
        let key = format!(r"{root}\SOFTWARE\Microsoft\Windows\CurrentVersion\Run");
        let Ok(o) = printers::no_window(std::process::Command::new("reg").args(["query", &key, "/v", name])).output() else { continue };
        let text = String::from_utf8_lossy(&o.stdout);
        let Some(val) = text
            .lines()
            .find_map(|l| l.trim().strip_prefix(name))
            .and_then(|r| r.trim().strip_prefix("REG_SZ"))
            .map(|v| v.trim().to_string())
        else {
            continue;
        };
        let Some(i) = val.to_ascii_lowercase().find(".exe") else { continue };
        if val.starts_with('"') {
            continue;
        }
        let (path, rest) = val.split_at(i + 4);
        let quoted = format!("\"{path}\"{rest}");
        let _ = printers::no_window(std::process::Command::new("reg").args(["add", &key, "/v", name, "/t", "REG_SZ", "/d", &quoted, "/f"])).output();
    }
}

fn app_dirs() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/Applications"), PathBuf::from("/System/Applications")];
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h).join("Applications"));
    }
    v
}

/// Nombres de aplicaciones bajo `dir` (recursivo, hasta 4 niveles) cuyo archivo tiene la extensión
/// `ext`; `name` convierte el archivo en el nombre mostrado (None lo descarta).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn scan_apps(dir: &std::path::Path, ext: &str, depth: u8, name: &dyn Fn(&std::path::Path) -> Option<String>, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                scan_apps(&p, ext, depth - 1, name, out);
            }
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case(ext)) {
            if let Some(n) = name(&p) {
                out.push(n);
            }
        }
    }
}

/// Nombre mostrado de un `.desktop` (None si está oculto o no tiene `Name=`).
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn desktop_name(p: &std::path::Path) -> Option<String> {
    let t = fs::read_to_string(p).ok()?;
    if t.lines().any(|l| l.trim() == "NoDisplay=true") {
        return None;
    }
    t.lines().find_map(|l| l.strip_prefix("Name=")).map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}

/// Aplicaciones instaladas para elegirlas como regla: .app (macOS), accesos del menú Inicio (Windows)
/// o entradas .desktop (Linux).
#[tauri::command]
fn list_apps() -> Vec<String> {
    #[cfg(target_os = "macos")]
    let mut v: Vec<String> = app_dirs()
        .into_iter()
        .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".app").map(String::from))
        .collect();
    #[cfg(windows)]
    let mut v: Vec<String> = {
        let mut v = Vec::new();
        let skip = |n: &str| {
            let l = n.to_lowercase();
            ["uninstall", "desinstalar", "readme", "help", "ayuda", "documentation", "documentación", "license", "licencia"].iter().any(|k| l.contains(k))
        };
        let name = |p: &std::path::Path| p.file_stem()?.to_str().map(String::from).filter(|n| !skip(n));
        for var in ["ProgramData", "APPDATA"] {
            if let Some(b) = std::env::var_os(var) {
                scan_apps(&PathBuf::from(b).join(r"Microsoft\Windows\Start Menu\Programs"), "lnk", 3, &name, &mut v);
            }
        }
        v
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut v: Vec<String> = {
        let mut v = Vec::new();
        let name = desktop_name;
        let mut dirs = vec![PathBuf::from("/usr/share/applications"), PathBuf::from("/var/lib/flatpak/exports/share/applications")];
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(h).join(".local/share/applications"));
        }
        for d in dirs {
            scan_apps(&d, "desktop", 1, &name, &mut v);
        }
        v
    };
    v.sort_by_key(|a| a.to_lowercase());
    v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    v
}

/// Nombre de archivo sin componentes de ruta (ni `..`, separadores, controles o NUL).
fn safe_file_stem(n: &str) -> bool {
    !n.is_empty() && n.len() <= 128 && !n.contains("..") && !n.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// Ícono de la app como data URL PNG (32 px), cacheado en el directorio de datos.
#[tauri::command]
fn app_icon(s: tauri::State<Shared>, name: String) -> Option<String> {
    use base64::Engine;
    if !safe_file_stem(&name) {
        return None;
    }
    let cache = s.dir.join("icons");
    let png = cache.join(format!("{name}.png"));
    if !png.exists() {
        let app = app_dirs().into_iter().map(|d| d.join(format!("{name}.app"))).find(|p| p.exists())?;
        let plist = app.join("Contents/Info.plist");
        let out = std::process::Command::new("plutil")
            .args(["-extract", "CFBundleIconFile", "raw", "-o", "-"])
            .arg(&plist)
            .output()
            .ok()?;
        let mut icon = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // El nombre del ícono lo escribe el .app: no puede salirse de Contents/Resources.
        if icon.is_empty() || !safe_file_stem(&icon) {
            return None;
        }
        if !icon.ends_with(".icns") {
            icon.push_str(".icns");
        }
        fs::create_dir_all(&cache).ok()?;
        let ok = std::process::Command::new("sips")
            .args(["-s", "format", "png", "-Z", "32"])
            .arg(app.join("Contents/Resources").join(&icon))
            .arg("--out")
            .arg(&png)
            .output()
            .ok()?
            .status
            .success();
        if !ok {
            return None;
        }
    }
    let b = fs::read(&png).ok()?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(b)))
}

// --- Copia de seguridad (.spoolguard.json): formato y validación compartidos con el servidor en core/src/backup.rs ---

/// Lista blanca de campos de la configuración que viajan en la copia (nunca contraseña ni token).
fn snapshot_of(c: &Config) -> backup::Snapshot {
    let st = &c.settings;
    backup::Snapshot {
        printers: c.printers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        global: c.global.clone(),
        params: backup::Params {
            unknown_policy: st.unknown_policy.clone(),
            interval_secs: st.interval_secs,
            notify_block: st.notify_block,
            notify_allow: st.notify_allow,
            theme: st.theme.clone(),
            retention_days: st.retention_days,
        },
        quick: backup::Quick::default(),
    }
}

/// Vuelca en la configuración solo las categorías aplicadas; contraseña, tokens y envío al servidor no se tocan.
fn apply_snapshot(c: &mut Config, snap: &backup::Snapshot, applied: &[&str]) {
    if applied.contains(&"reglas") {
        c.printers = snap.printers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        c.global = snap.global.clone();
    }
    if applied.contains(&"parametros") {
        let p = &snap.params;
        c.settings.unknown_policy = p.unknown_policy.clone();
        c.settings.interval_secs = p.interval_secs;
        c.settings.notify_block = p.notify_block;
        c.settings.notify_allow = p.notify_allow;
        c.settings.theme = p.theme.clone();
        c.settings.retention_days = p.retention_days;
    }
}

fn parse_modo(modo: &Option<serde_json::Value>) -> Result<backup::Modes, String> {
    backup::Modes::parse(modo.as_ref().unwrap_or(&serde_json::Value::Null))
}

/// Inventario de impresoras y puertos con estructura estable (también alimenta la categoría "inventario" de la copia).
fn inventory_rows(s: &State) -> Result<Vec<InventoryRow>, String> {
    let names = queue::list_printers()?;
    let usb = printers::usb_devices();
    let cfg = s.config.lock().unwrap().clone();
    Ok(thread::scope(|sc| {
        let hs: Vec<_> = names.iter().map(|n| { let u = &usb; sc.spawn(move || printers::info(&s.dir, n, u)) }).collect();
        hs.into_iter().zip(&names).map(|(h, n)| {
            let i = h.join().unwrap_or_default();
            InventoryRow {
                name: n.clone(), driver: i.driver, uri: backup::sanitize_uri(&i.uri), kind: i.kind, conn_type: i.conn_type, host: i.host,
                shared: i.shared, status: i.status, hold_supported: i.hold_supported, controlled: wants_hold(&cfg, n),
            }
        }).collect()
    }))
}

#[tauri::command]
async fn get_inventory(s: tauri::State<'_, Shared>) -> Result<Vec<InventoryRow>, String> {
    guard(&s)?;
    inventory_rows(&s)
}

fn inventory(s: &State) -> Result<Vec<backup::InvItem>, String> {
    Ok(inventory_rows(s)?.into_iter().map(|r| backup::InvItem {
        name: r.name, kind: r.kind, uri: r.uri, driver: r.driver, conn_type: r.conn_type, host: r.host, shared: r.shared,
        status: r.status, hold_supported: Some(r.hold_supported), controlled: Some(r.controlled),
    }).collect())
}

fn build_backup(s: &State, cats: &[String], from: Option<u64>, to: Option<u64>, max_events: Option<usize>) -> Result<String, String> {
    if cats.is_empty() {
        return Err("Elige al menos una categoría".into());
    }
    if let Some(c) = cats.iter().find(|c| !backup::CATS.contains(&c.as_str())) {
        return Err(format!("Categoría desconocida: {}", c.chars().take(40).collect::<String>()));
    }
    if cats.iter().any(|c| c == "bloqueo_rapido") {
        return Err("El bloqueo rápido solo existe en el servidor".into());
    }
    let events = if cats.iter().any(|c| c == "registro") {
        let n = max_events.filter(|n| *n > 0).unwrap_or(backup::DEFAULT_EVENTS).min(backup::MAX_EVENTS) as u32;
        let mut rows = s.events.query(&Filter { from, to, ..Default::default() }, n, 0)?.rows;
        rows.reverse(); // más antiguo primero
        rows
    } else {
        vec![]
    };
    let inv = if cats.iter().any(|c| c == "inventario") { inventory(s)? } else { vec![] };
    let snap = snapshot_of(&s.config.lock().unwrap());
    let meta = backup::Meta { created: now() as i64, host: s.host.clone(), app_version: env!("CARGO_PKG_VERSION").into() };
    serde_json::to_string_pretty(&backup::export(&snap, cats, &events, &inv, &meta)).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
async fn export_backup(s: tauri::State<'_, Shared>, categories: Vec<String>, from: Option<u64>, to: Option<u64>, max_events: Option<usize>) -> Result<String, String> {
    guard(&s)?;
    let out = build_backup(&s, &categories, from, to, max_events)?;
    log_notice(&s, format!("Exportación de copia de seguridad. Categorías: {}", categories.join(", ")));
    Ok(out)
}

fn plan_import(s: &State, json: &str, modo: &Option<serde_json::Value>) -> Result<backup::Plan, String> {
    let m = parse_modo(modo)?;
    let p = backup::parse(json.as_bytes(), now() as i64)?;
    let ids: Vec<String> = p.events.iter().flatten().map(|e| e.id.clone()).collect();
    let ctx = backup::Ctx { local: queue::list_printers().unwrap_or_default(), existing_ids: s.events.existing_ids(&ids)?, event_count: s.events.count(), server: false };
    let cur = snapshot_of(&s.config.lock().unwrap());
    Ok(backup::plan(&cur, &p, &m, &ctx))
}

/// Valida el archivo y resume qué cambiaría, SIN aplicar nada.
#[tauri::command]
fn preview_import(s: tauri::State<Shared>, json: String, modo: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    guard(&s)?;
    Ok(plan_import(&s, &json, &modo)?.preview)
}

fn strings_of(v: &serde_json::Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Aplica la importación de forma atómica: guarda `config.json.bak` y, si algo falla, deja todo como estaba.
fn do_import(s: &State, json: &str, modo: &Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let pl = plan_import(s, json, modo)?;
    if !pl.ok {
        return Err(format!("El archivo tiene errores y no se aplicó nada: {}", strings_of(&pl.preview["errores"]).into_iter().take(3).collect::<Vec<_>>().join("; ")));
    }
    if pl.applied.is_empty() {
        return Err("No hay nada que importar con las opciones elegidas".into());
    }
    let cur = s.dir.join("config.json");
    let prev_file = fs::read(&cur).ok();
    if prev_file.is_some() {
        fs::copy(&cur, s.dir.join("config.json.bak")).map_err(|e| format!("No se pudo guardar la copia previa: {e}"))?;
    }
    let prev = s.config.lock().unwrap().clone();
    // Deshacer: restaura memoria y archivo sin pasar por save() (que sobrescribiría el .bak con lo importado).
    let rollback = || {
        *s.config.lock().unwrap() = prev.clone();
        if let Some(b) = &prev_file {
            let tmp = s.dir.join("config.json.tmp");
            if write_private(&tmp, b).is_ok() {
                let _ = fs::rename(&tmp, &cur);
            }
        }
    };
    if pl.applied.contains(&"reglas") || pl.applied.contains(&"parametros") {
        apply_snapshot(&mut s.config.lock().unwrap(), &pl.next, &pl.applied);
        if let Err(e) = save(s) {
            rollback();
            return Err(e);
        }
    }
    let mut imported = 0;
    if pl.applied.contains(&"registro") {
        match s.events.import_events(&pl.events, pl.replace_events) {
            Ok(n) => imported = n,
            Err(e) => {
                rollback();
                return Err(format!("No se pudo importar el registro (la configuración quedó como estaba): {e}"));
            }
        }
    }
    // Retención: reaplica según las reglas nuevas, solo en las colas que existen aquí.
    let names = queue::list_printers().unwrap_or_default();
    let failed = {
        let _g = hold_guard();
        let cfg = s.config.lock().unwrap().clone();
        sync_holds_locked(&s.dir, &cfg, &names)
    };
    let m = &pl.preview["modos"];
    log_notice(s, format!(
        "Importación de copia de seguridad. Categorías: {}. Modo: reglas {}, parámetros {}, registro {} ({imported} eventos)",
        pl.applied.join(", "), m["reglas"].as_str().unwrap_or(""), m["parametros"].as_str().unwrap_or(""), m["registro"].as_str().unwrap_or("")
    ));
    Ok(serde_json::json!({
        "warning": hold_warning(&failed),
        "denied": failed.iter().filter(|f| f.denied).map(|f| f.printer.clone()).collect::<Vec<_>>(),
        "avisos": pl.preview["avisos"],
        "aplicado": pl.applied,
        "eventos_importados": imported,
        "preview": pl.preview,
    }))
}

#[tauri::command]
fn apply_import(s: tauri::State<Shared>, json: String, modo: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    guard(&s)?;
    do_import(&s, &json, &modo)
}

/// Compatibilidad con la interfaz anterior: exporta solo reglas y parámetros (con el formato nuevo).
#[tauri::command]
fn export_rules(s: tauri::State<Shared>) -> Result<String, String> {
    guard(&s)?;
    build_backup(&s, &["reglas".into(), "parametros".into()], None, None, None)
}

/// Compatibilidad: acepta archivos de reglas antiguos y nuevos; reemplaza reglas y parámetros.
#[tauri::command]
fn import_rules(s: tauri::State<Shared>, json: String) -> Result<Option<String>, String> {
    guard(&s)?;
    let r = do_import(&s, &json, &Some(serde_json::json!({"reglas": "reemplazar", "parametros": "reemplazar"})))?;
    Ok(r["warning"].as_str().map(String::from))
}

#[tauri::command]
fn get_paused(s: tauri::State<Shared>) -> bool {
    s.paused.load(Ordering::Relaxed)
}

#[tauri::command]
fn set_paused(s: tauri::State<Shared>, paused: bool) -> Result<(), String> {
    guard(&s)?;
    s.paused.store(paused, Ordering::Relaxed);
    s.config.lock().unwrap().settings.paused = paused;
    save(&s)
}

/// Guarda un evento en la base local; entra en la cola de envío solo si el envío está activado.
fn record(s: &State, ev: Event) {
    let queue = {
        let c = s.config.lock().unwrap();
        c.settings.send_events && !c.settings.server_url.is_empty() && !c.settings.server_token.is_empty()
    };
    let _ = s.events.insert(&ev, queue);
}

/// Aviso del sistema en el registro de actividad.
fn log_notice(s: &State, text: String) {
    let mut e = Event::new("desktop", &s.host);
    e.printer = "—".into();
    e.document = text;
    e.decision = "notice".into();
    e.reason = "Aviso del sistema".into();
    e.status = "aviso".into();
    record(s, e);
}

/// Nombre de este equipo.
fn host_name() -> String {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    #[cfg(windows)]
    {
        return env("COMPUTERNAME").unwrap_or_default();
    }
    #[cfg(not(windows))]
    {
        let out = std::process::Command::new("hostname").output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        out.or_else(|| env("HOSTNAME")).unwrap_or_default()
    }
}

/// Hilo de envío al servidor: cada 2 s mira si hay algo pendiente y, solo con el envío activado,
/// manda lotes con reintentos de espera creciente.
fn sync_loop(s: Shared) {
    loop {
        thread::sleep(Duration::from_secs(2));
        let (on, url, token) = {
            let c = s.config.lock().unwrap();
            (c.settings.send_events, c.settings.server_url.clone(), c.settings.server_token.clone())
        };
        let make = move || -> Result<Box<dyn Transport>, String> { Ok(Box::new(HttpTransport::new(&url, &token)?)) };
        s.sync.lock().unwrap().tick(now(), on, &s.events, &make);
    }
}

/// Refleja en el menú de la bandeja cuántas impresoras se controlan y los bloqueos de las últimas 24 h.
fn update_tray(app: &AppHandle, s: &State, last: &mut String) {
    let Some(t) = app.try_state::<TrayItems>() else { return };
    let ctl = s.config.lock().unwrap().printers.values().filter(|r| r.enabled).count();
    let day = now().saturating_sub(86400);
    let blocked = s.events.count_blocked_since(day);
    let paused = s.paused.load(Ordering::Relaxed);
    let text = format!(
        "{} · {} controladas · {} bloqueos (24 h)",
        if paused { "⏸ En pausa" } else { "✓ Activo" },
        ctl,
        blocked
    );
    if text != *last {
        let _ = t.status.set_text(&text);
        let _ = t.pause.set_text(if paused { "Reanudar control" } else { "Pausar control" });
        *last = text;
    }
}

fn worker(app: AppHandle, s: Shared) {
    let mut last = String::new();
    let mut busy_until = Instant::now();
    // Colas que no admiten pausa: se intenta una vez y se avisa, no cada ciclo.
    let mut no_hold: HashSet<String> = HashSet::new();
    // macOS/Linux: trabajos que el cliente mandó sin retención (`no-hold`) ya vistos, para no evaluarlos en cada ciclo.
    #[cfg(unix)]
    let mut unheld: HashSet<(String, u32)> = HashSet::new();
    let mut last_purge = Instant::now();
    // Despertar inmediato en macOS/Linux cuando CUPS crea un trabajo (antes de que se imprima).
    let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
    #[cfg(unix)]
    queue::watch_new_jobs(s.dir.clone(), move || {
        let _ = wake_tx.send(());
    });
    // En Windows no hay vigía: el emisor se conserva para que recv_timeout espere de verdad (sin él devolvería error al instante).
    #[cfg(not(unix))]
    let _wake_tx = wake_tx;
    loop {
        update_tray(&app, &s, &mut last);
        // Retención del registro: limpieza una vez por hora.
        if last_purge.elapsed() >= Duration::from_secs(3600) {
            last_purge = Instant::now();
            let days = s.config.lock().unwrap().settings.retention_days;
            let _ = s.events.purge(days);
        }
        // Sondeo adaptativo: 100 ms con trabajos liberados en curso (la cola está activa y un trabajo
        // nuevo podría imprimirse sin evaluar), 200 ms con actividad reciente; en reposo, el intervalo configurado.
        let idle = Duration::from_secs(s.config.lock().unwrap().settings.interval_secs.max(1));
        let pausa = if queue::busy() {
            Duration::from_millis(100)
        } else if Instant::now() < busy_until {
            Duration::from_millis(200)
        } else {
            idle
        };
        // Dormir hasta el próximo ciclo, o hasta que el vigía de CUPS avise de un trabajo nuevo.
        if wake_rx.recv_timeout(pausa).is_ok() {
            while wake_rx.try_recv().is_ok() {}
        }
        // La configuración se lee DESPUÉS de dormir: una copia anterior (p. ej. con el global aún activo)
        // retendría colas que set_global_rules acaba de liberar.
        let cfg = s.config.lock().unwrap().clone();
        // Colas que retenemos y ya no deben estarlo (global apagado, regla quitada): liberar.
        for p in orphan_queues(&cfg, &managed(&s.dir)) {
            free_if_unwanted(&s, &p);
        }
        if !cfg.global.enabled {
            no_hold.clear();
        }
        // Colas controladas: con regla propia o todas si la global está activa.
        let mut targets: Vec<String> = cfg.printers.iter().filter(|(_, r)| r.enabled).map(|(p, _)| p.clone()).collect();
        if cfg.global.enabled {
            let known = managed(&s.dir);
            for p in queue::list_printers().unwrap_or_default() {
                if !known.contains_key(&p) && !no_hold.contains(&p) {
                    // Impresora nueva: se retiene antes de evaluar (si el global sigue activo en este instante).
                    let g = hold_guard();
                    if !wants_hold(&s.config.lock().unwrap(), &p) {
                        continue;
                    }
                    let r = apply_hold_locked(&s.dir, &p, true);
                    drop(g);
                    if r.is_err() {
                        no_hold.insert(p.clone());
                        notice_hold_failed(&s, &p);
                    }
                }
                if !targets.contains(&p) {
                    targets.push(p);
                }
            }
        }
        let none = PrinterRules::default();
        for printer in &targets {
            let rules = cfg.printers.get(printer).unwrap_or(&none);
            let Ok(ids) = queue::held_job_ids(printer) else { continue };
            #[cfg(unix)]
            unheld.retain(|(p, i)| p != printer || ids.contains(i));
            if !ids.is_empty() {
                busy_until = Instant::now() + Duration::from_secs(5);
            }
            for id in ids {
                let Ok(mut h) = queue::job_info(&s.dir, id) else { continue };
                // Un cliente (p. ej. Word por el diálogo de macOS) puede pedir `job-hold-until=no-hold` y saltarse la
                // retención de la cola: se retiene aquí al verlo; si ya empezó a salir, `late` permite cancelarlo igual.
                #[allow(unused_mut)]
                let mut late = false;
                if !h.held {
                    #[cfg(unix)]
                    {
                        if !unheld.insert((printer.clone(), id)) {
                            continue;
                        }
                        let _ = queue::hold(id);
                        match queue::job_info(&s.dir, id) {
                            Ok(n) => {
                                late = !n.held;
                                h = n;
                            }
                            Err(_) => continue,
                        }
                    }
                    #[cfg(not(unix))]
                    continue;
                }
                if h.job.site.is_none() {
                    h.job.site = control_core::intents::find_site(&s.dir, &h.job.document, now());
                }
                // Windows informa el nombre del equipo de origen, no su IP: se resuelve aquí (hilo de evaluación) para las reglas por IP.
                if h.job.origin_ip.is_none() {
                    h.job.origin_ip = h.job.origin_host.as_deref().and_then(resolve_origin);
                }
                // Con el control pausado todo se libera sin evaluar.
                let (d, reason) = if s.paused.load(Ordering::Relaxed) {
                    (Decision::Allow, "Control pausado".to_string())
                } else if h.job.document.is_empty() {
                    let a = cfg.settings.unknown_policy != "block";
                    (if a { Decision::Allow } else { Decision::Deny }, "Datos insuficientes".to_string())
                } else {
                    control_core::rules::explain_both(&cfg.global, rules, &h.job)
                };
                let allowed = d == Decision::Allow;
                // El filtro de CUPS (scripts/guard-macos.sh) espera este veredicto antes de enviar datos a la impresora.
                #[cfg(unix)]
                queue::write_verdict(printer, id, allowed);
                let r = if late {
                    // Ya salió hacia la impresora: permitido no hay nada que liberar; bloqueado se cancela lo que quede.
                    if allowed { continue } else { queue::cancel(id).map(|_| Cancelled::AlreadyPrinted) }
                } else if allowed {
                    // Liberado por la app: al verlo sin retención no hay que volver a retenerlo.
                    #[cfg(unix)]
                    unheld.insert((printer.clone(), id));
                    queue::release(id).map(|_| Cancelled::Held)
                } else {
                    queue::cancel(id)
                };
                let Ok(outcome) = r else { continue };
                // El registro dice lo que pasó, no lo que se pretendía: un bloqueado que ya había salido no se anota «cancelado».
                let status = match (allowed, outcome) {
                    (true, _) => "liberado",
                    (false, Cancelled::Held) => "cancelado",
                    (false, Cancelled::AlreadyPrinted) => "impreso antes de cancelar",
                    (false, Cancelled::Gone) => "ya no estaba en la cola",
                };
                let leaked = !allowed && outcome == Cancelled::AlreadyPrinted;
                let mut entry = Event::new("desktop", &s.host);
                entry.printer = printer.clone();
                entry.document = h.job.document.clone();
                entry.user = h.job.user.clone().unwrap_or_default();
                entry.ip = h.job.origin_ip.clone().or_else(|| h.job.origin_host.clone()).unwrap_or_default();
                entry.site = h.job.site.clone().unwrap_or_default();
                entry.app = h.job.app.clone().unwrap_or_default();
                entry.decision = if allowed { "allow" } else { "block" }.into();
                entry.status = status.into();
                entry.reason = if leaked { format!("{reason} (el trabajo ya se estaba imprimiendo al cancelarlo)") } else { reason };
                entry.pages = h.pages;
                entry.bytes = h.bytes;
                (entry.port, entry.conn_type) = printers::route(&s.dir, printer);
                record(&s, entry);
                if leaked || (!allowed && cfg.settings.notify_block) || (allowed && cfg.settings.notify_allow) {
                    let _ = app
                        .notification()
                        .builder()
                        .title(if allowed { "Impresión permitida" } else if leaked { "Impresión bloqueada que ya había salido" } else { "Impresión bloqueada" })
                        .body(format!("{} → {}", h.job.document, printer))
                        .show();
                }
            }
        }
    }
}

/// En macOS la app vive solo en la barra de menús cuando no hay ventana (sin ícono en el Dock).
fn dock(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(if visible { tauri::ActivationPolicy::Regular } else { tauri::ActivationPolicy::Accessory });
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

fn show_main(app: &AppHandle) {
    dock(app, true);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Proceso guardián: se lanza desde la app (mismo ejecutable, `--guardian <pid> <dir>`), sobrevive a
/// un cierre forzoso de la app y deja las colas normales cuando el proceso observado desaparece.
pub fn guardian(args: &[String]) {
    let (Some(pid), Some(dir)) = (args.first().and_then(|p| p.parse::<u32>().ok()), args.get(1)) else { return };
    let dir = PathBuf::from(dir);
    let alive = |pid: u32| -> bool {
        #[cfg(unix)]
        {
            std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
        }
        #[cfg(windows)]
        {
            let o = printers::no_window(std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"])).output();
            o.map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())).unwrap_or(false)
        }
    };
    while alive(pid) {
        thread::sleep(Duration::from_secs(1));
    }
    // Si otra instancia tomó el relevo (reinicio), ella gestiona las colas.
    thread::sleep(Duration::from_secs(3));
    let current: Option<u32> = fs::read_to_string(dir.join("app.pid")).ok().and_then(|t| t.trim().parse().ok());
    if let Some(c) = current {
        if c != pid && alive(c) {
            return;
        }
    }
    for p in managed(&dir).into_keys() {
        free_queue(&dir, &p);
    }
}

/// Lanza el guardián desacoplado de la app.
fn spawn_guardian(dir: &std::path::Path) {
    let _ = fs::write(dir.join("app.pid"), std::process::id().to_string());
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    c.arg("--guardian").arg(std::process::id().to_string()).arg(dir)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0008 | 0x0800_0000); // DETACHED_PROCESS | CREATE_NO_WINDOW
    }
    let _ = c.spawn();
}

pub fn run() {
    tauri::Builder::default()
        // Una sola instancia: abrir la app otra vez trae la ventana existente.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .setup(|app| {
            // CONTROL_IMPRESION_DIR permite probar sin tocar la configuración real del usuario.
            let dir = match std::env::var_os("CONTROL_IMPRESION_DIR") {
                Some(d) => PathBuf::from(d),
                None => app.path().app_data_dir()?,
            };
            fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                // El directorio de datos (config, registro, token) es solo del dueño.
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            }
            let (config, warn) = load_config(&dir);
            // Base de eventos local; importa log.jsonl la primera vez (y lo conserva renombrado).
            let events = EventDb::open(&dir).map_err(|e| format!("No se pudo abrir el registro de eventos: {e}"))?;
            let _ = events.purge(config.settings.retention_days);
            let paused0 = config.settings.paused;
            let unlocked0 = config.settings.password_hash.is_empty();
            let state: Shared = Arc::new(State {
                dir,
                config: Mutex::new(config),
                events,
                host: host_name(),
                sync: Mutex::new(Syncer::default()),
                paused: AtomicBool::new(paused0),
                unlocked: AtomicBool::new(unlocked0),
                disc: Mutex::new(DiscJob::default()),
                disc_cancel: AtomicBool::new(false),
            });
            app.manage(state.clone());

            // Primera ejecución: activa el inicio con el sistema (el usuario puede desactivarlo después
            // en Ajustes). La marca guarda la ruta del exe que lo registró: si cambió (actualización desde
            // otro instalador, otra carpeta) o es la marca antigua ("1"), se vuelve a registrar el actual.
            // No en pruebas con directorio propio.
            {
                use tauri_plugin_autostart::ManagerExt;
                let marker = state.dir.join("autostart.init");
                if std::env::var_os("CONTROL_IMPRESION_DIR").is_none() {
                    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                    let prev = fs::read_to_string(&marker).ok();
                    let enabled = app.autolaunch().is_enabled().unwrap_or(false);
                    if autostart_needs_register(prev.as_deref(), &exe, enabled) {
                        if app.autolaunch().enable().is_ok() {
                            let _ = fs::write(&marker, exe.as_bytes());
                        }
                    } else if prev.as_deref().map(str::trim) != Some(exe.as_str()) {
                        let _ = fs::write(&marker, exe.as_bytes());
                    }
                    #[cfg(windows)]
                    {
                        let name = app.package_info().name.clone();
                        thread::spawn(move || quote_run_value(&name));
                    }
                }
            }
            // Reaplica la retención de las colas controladas (el estado del SO puede haber cambiado
            // durante el reinicio) y el estado de pausa guardado.
            {
                let cfg = state.config.lock().unwrap().clone();
                for p in queue::list_printers().unwrap_or_default() {
                    if wants_hold(&cfg, &p) && apply_hold(&state.dir, &p, true).is_err() {
                        notice_hold_failed(&state, &p);
                    }
                }
                // Colas retenidas por la app que ya no tienen control: liberar.
                for p in managed(&state.dir).into_keys() {
                    if !wants_hold(&cfg, &p) {
                        free_queue(&state.dir, &p);
                    }
                }
            }
            if let Some(w) = warn {
                log_notice(&state, w);
            }
            spawn_guardian(&state.dir);
            {
                let st = state.clone();
                thread::spawn(move || sync_loop(st));
            }
            // Registra el host de la extensión sin intervención del usuario.
            let _ = browser::ensure(&state.dir, app.path().resource_dir().ok());

            // Menú de aplicación nativo (Edición es necesario para Cmd+C/V en los campos).
            let nav = |id: &str, label: &str, key: &str| MenuItem::with_id(app, id, label, true, Some(key));
            let app_menu = SubmenuBuilder::new(app, "SpoolGuard")
                .about(Some(AboutMetadata {
                    name: Some("SpoolGuard".into()),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                    copyright: Some("© 2026 Cohasa".into()),
                    ..Default::default()
                }))
                .separator()
                .item(&nav("nav-preferences", "Ajustes…", "CmdOrCtrl+,")?)
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .quit()
                .build()?;
            let edit_menu = SubmenuBuilder::new(app, "Edición")
                .undo().redo().separator().cut().copy().paste().select_all()
                .build()?;
            let view_menu = SubmenuBuilder::new(app, "Vista")
                .item(&nav("nav-printers", "Impresoras", "CmdOrCtrl+1")?)
                .item(&nav("nav-rules", "Reglas", "CmdOrCtrl+2")?)
                .item(&nav("nav-log", "Registro", "CmdOrCtrl+3")?)
                .item(&nav("nav-settings", "Ajustes", "CmdOrCtrl+4")?)
                .separator()
                .fullscreen()
                .build()?;
            let window_menu = SubmenuBuilder::new(app, "Ventana").minimize().close_window().build()?;
            app.set_menu(MenuBuilder::new(app).items(&[&app_menu, &edit_menu, &view_menu, &window_menu]).build()?)?;
            // Solo macOS muestra el menú en la barra superior; en Windows/Linux se oculta (los atajos siguen activos).
            #[cfg(not(target_os = "macos"))]
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.hide_menu();
            }

            // window-state puede restaurar un tamaño guardado menor al mínimo (visto: 626×340): se sube al mínimo
            // de tauri.conf.json (minWidth/minHeight) para que todo el contenido quede visible.
            // El plugin restaura después de `setup`, por eso se repite unas veces en vez de una sola.
            {
                let handle = app.handle().clone();
                thread::spawn(move || {
                    for _ in 0..10 {
                        thread::sleep(Duration::from_millis(500));
                        let Some(w) = handle.get_webview_window("main") else { continue };
                        if let (Ok(size), Ok(scale)) = (w.inner_size(), w.scale_factor()) {
                            let l = size.to_logical::<f64>(scale);
                            // También se acota al monitor: un tamaño/posición guardados de otra pantalla dejaban la ventana fuera.
                            let (mw, mh) = match w.current_monitor() {
                                Ok(Some(m)) => {
                                    let ml = m.size().to_logical::<f64>(m.scale_factor());
                                    (ml.width.max(760.0), ml.height.max(500.0))
                                }
                                _ => (f64::MAX, f64::MAX),
                            };
                            let (nw, nh) = (l.width.clamp(760.0, mw), l.height.clamp(500.0, mh));
                            if (nw, nh) != (l.width, l.height) {
                                let _ = w.set_size(tauri::LogicalSize::new(nw, nh));
                                let _ = w.center();
                            }
                        }
                    }
                });
            }

            // Bandeja: ícono monocromo adaptable al tema del sistema y estado del control.
            let status = MenuItem::with_id(app, "status", "Iniciando…", false, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Abrir SpoolGuard", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, "pause", "Pausar control", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&status, &open, &pause, &quit])?;
            app.manage(TrayItems { status: status.clone(), pause: pause.clone() });
            let st = state.clone();
            // macOS: plantilla monocromática que el sistema tiñe; Windows/Linux no tiñen, así que
            // un PNG negro se pierde en la bandeja oscura: se usa uno a color.
            #[cfg(target_os = "macos")]
            let icon = Image::from_bytes(include_bytes!("../icons/tray-template.png"))?;
            #[cfg(not(target_os = "macos"))]
            let icon = Image::from_bytes(include_bytes!("../icons/tray-color.png"))?;
            let tray = TrayIconBuilder::new().icon(icon);
            #[cfg(target_os = "macos")]
            let tray = tray.icon_as_template(true);
            tray
                .tooltip("SpoolGuard")
                .menu(&tray_menu)
                .on_menu_event(move |app, ev| match ev.id().as_ref() {
                    "open" => show_main(app),
                    "pause" => {
                        if guard(&st).is_err() {
                            return;
                        }
                        let p = !st.paused.load(Ordering::Relaxed);
                        st.paused.store(p, Ordering::Relaxed);
                        st.config.lock().unwrap().settings.paused = p;
                        let _ = save(&st);
                        let _ = pause.set_text(if p { "Reanudar control" } else { "Pausar control" });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // Con --minimized (inicio automático) arranca oculta en la bandeja.
            if std::env::args().any(|a| a == "--minimized") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
                dock(app.handle(), false);
            }
            let handle = app.handle().clone();
            thread::spawn(move || worker(handle, state));
            Ok(())
        })
        .on_window_event(|w, ev| {
            // La X minimiza a la bandeja y el control sigue activo. Las colas se liberan solo al
            // salir de verdad (Salir de la bandeja, Cmd+Q o cierre forzoso, que cubre el guardián).
            if let WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = w.hide();
                dock(w.app_handle(), false);
            }
        })
        .on_menu_event(|app, ev| {
            if let Some(view) = ev.id().as_ref().strip_prefix("nav-") {
                show_main(app);
                let _ = app.emit("navigate", view);
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_printers, set_rules, grant_printer_access, get_global_rules, set_global_rules, get_activity, export_rules, import_rules, set_paused, get_paused,
            clear_log, export_log_csv, query_events, get_inventory, summarize_events, get_network_settings, set_network_settings, discover_network, discover_status, discover_cancel, get_sync_status, get_settings, set_settings, has_password, verify_password,
            set_password, write_text_file, read_text_file, read_backup_file, export_backup, preview_import, apply_import, app_icon, extension_status, open_extension, list_browsers, install_extension, export_extension_zip, get_autostart, set_autostart, list_apps, guard_status, guard_install
        ])
        .build(tauri::generate_context!())
        .expect("error al iniciar la aplicación")
        .run(|app, ev| {
            // Clic en el Dock o reabrir la app: mostrar la ventana oculta en la bandeja.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = ev {
                show_main(app);
            }
            // Al salir no deben quedar trabajos en pausa sin nadie que los libere.
            if matches!(ev, tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }) {
                if let Some(st) = app.try_state::<Shared>() {
                    for p in managed(&st.dir).into_keys() {
                        free_queue(&st.dir, &p);
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = app;
        });
}

#[cfg(test)]
mod apps_tests {
    use super::*;

    #[test]
    fn escanea_desktop_y_oculta_los_ocultos() {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("prueba-apps");
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.desktop"), "[Desktop Entry]\nName=Writer\nExec=x\n").unwrap();
        fs::write(d.join("sub/b.desktop"), "[Desktop Entry]\nName=Calc\nNoDisplay=true\n").unwrap();
        fs::write(d.join("sub/c.desktop"), "[Desktop Entry]\nName=Impress\n").unwrap();
        fs::write(d.join("x.txt"), "Name=Nada").unwrap();
        let mut v = Vec::new();
        scan_apps(&d, "desktop", 1, &desktop_name, &mut v);
        v.sort();
        assert_eq!(v, vec!["Impress".to_string(), "Writer".to_string()]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn lista_apps_no_vacia_en_este_equipo() {
        // macOS (/Applications) y Windows (menú Inicio) siempre tienen algo; en Linux de CI puede estar vacío.
        let v = list_apps();
        if cfg!(any(target_os = "macos", windows)) {
            assert!(!v.is_empty(), "list_apps devolvió una lista vacía");
        }
    }
}

#[cfg(test)]
mod managed_tests {
    use super::*;

    #[test]
    fn managed_acepta_formato_viejo_y_nuevo() {
        let v = parse_managed(br#"["Fax","HP"]"#);
        assert_eq!(v.len(), 2);
        assert!(!v["Fax"].pausada_antes);
        let n = parse_managed(br#"{"Fax":{"pausada_antes":true},"HP":{}}"#);
        assert!(n["Fax"].pausada_antes);
        assert!(!n["HP"].pausada_antes);
        assert!(parse_managed(b"[]").is_empty());
        assert!(parse_managed(b"{}").is_empty());
        assert!(parse_managed(b"basura").is_empty());
        // Ida y vuelta.
        assert_eq!(parse_managed(&serde_json::to_vec(&n).unwrap()), n);
    }

    #[test]
    fn aviso_de_retencion() {
        assert_eq!(hold_warning(&[]), None);
        let w = hold_warning(&[HoldFailure::new("Fax", "x"), HoldFailure::new("PDF", "x")]).unwrap();
        assert!(w.contains("Fax, PDF"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_de_archivo_sin_rutas() {
        for ok in ["Safari", "Google Chrome", "Pages 2"] {
            assert!(safe_file_stem(ok), "{ok}");
        }
        for bad in ["", "..", "../x", "a/b", "a\\b", "x\0", "a\nb", "..hidden", "C:x"] {
            assert!(!safe_file_stem(bad), "{bad:?}");
        }
    }

    fn cfg(global: bool, own: &[&str]) -> Config {
        let mut c = Config::default();
        c.global.enabled = global;
        for p in own {
            c.printers.insert(p.to_string(), PrinterRules { enabled: true, ..Default::default() });
        }
        c
    }

    fn managed_of(names: &[&str]) -> BTreeMap<String, Managed> {
        names.iter().map(|n| (n.to_string(), Managed::default())).collect()
    }

    #[test]
    fn autostart_se_reregistra_si_cambia_el_exe() {
        let exe = r"C:\Program Files\SpoolGuard\spoolguard.exe";
        assert!(autostart_needs_register(None, exe, false));
        assert!(autostart_needs_register(Some("1"), exe, false));
        assert!(!autostart_needs_register(Some(exe), exe, false));
        assert!(!autostart_needs_register(Some(exe), exe, true));
        // Exe distinto: solo si estaba activo (si el usuario lo desactivó se respeta).
        assert!(autostart_needs_register(Some(r"C:\viejo.exe"), exe, true));
        assert!(!autostart_needs_register(Some(r"C:\viejo.exe"), exe, false));
    }

    #[test]
    fn global_apagado_libera_colas_gestionadas() {
        let m = managed_of(&["A", "B", "C"]);
        // Global activo: nada huérfano.
        assert!(orphan_queues(&cfg(true, &[]), &m).is_empty());
        // Global apagado: solo se conservan las de regla propia.
        assert_eq!(orphan_queues(&cfg(false, &["B"]), &m), vec!["A".to_string(), "C".to_string()]);
        assert_eq!(orphan_queues(&cfg(false, &[]), &m).len(), 3);
    }

    #[test]
    fn aviso_distingue_permiso_de_sin_pausa() {
        assert_eq!(hold_warning(&[]), None);
        let denied = HoldFailure::new("Ctrl", &format!("{}: OpenPrinterW(Ctrl): x", control_core::ACCESS_DENIED));
        let nopause = HoldFailure::new("Fax", "SetPrinterW: x");
        assert!(denied.denied && !nopause.denied);
        let w = hold_warning(&[denied, nopause]).unwrap();
        assert!(w.contains("Sin permiso para administrar la cola de: Ctrl"), "{w}");
        assert!(w.contains("retención de: Fax"), "{w}");
        let solo = hold_warning(&[HoldFailure::new("Ctrl", control_core::ACCESS_DENIED)]).unwrap();
        assert!(!solo.contains("admitan pausa"), "{solo}");
    }
}

#[cfg(test)]
mod backup_tests {
    use super::*;
    use serde_json::json;

    pub(super) fn state(name: &str) -> State {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("work").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.settings.password_hash = "HASH-SECRETO".into();
        cfg.settings.password_salt = "SAL-SECRETA".into();
        cfg.settings.server_token = "sgm_TOKEN-SECRETO".into();
        cfg.settings.server_url = "https://servidor.local".into();
        let s = State {
            events: EventDb::open(&dir).unwrap(),
            dir,
            config: Mutex::new(cfg),
            host: "pc-prueba".into(),
            sync: Mutex::new(Syncer::default()),
            paused: AtomicBool::new(false),
            unlocked: AtomicBool::new(true),
            disc: Mutex::new(DiscJob::default()),
            disc_cancel: AtomicBool::new(false),
        };
        save(&s).unwrap();
        s
    }

    // Solo reglas de impresoras que no existen aquí y global apagada: la prueba nunca retiene colas reales.
    fn file(extra: serde_json::Value) -> String {
        let mut f = json!({"format": "spoolguard-backup", "version": 1, "reglas": {"printers": {"ImpresoraDePrueba-xyz": {"enabled": true, "mode": "block", "apps": ["Word"], "sites": [], "ips": []}}}});
        f.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        f.to_string()
    }

    #[test]
    fn importar_hostil_no_toca_contrasena_ni_tokens_y_guarda_copia() {
        let s = state("bk-rust-hostil");
        let hostil = file(json!({
            "password_hash": "x", "token": "t",
            "parametros": {"theme": "dark", "password_hash": "deadbeef", "password_salt": "s", "server_token": "sgm_robado", "server_url": "http://evil", "send_events": true, "__proto__": {"x": 1}}
        }));
        let r = do_import(&s, &hostil, &Some(json!("reemplazar"))).unwrap();
        assert!(r["avisos"].to_string().contains("campo sensible ignorado por seguridad: server_token"));
        let c = s.config.lock().unwrap().clone();
        assert_eq!(c.settings.password_hash, "HASH-SECRETO");
        assert_eq!(c.settings.password_salt, "SAL-SECRETA");
        assert_eq!(c.settings.server_token, "sgm_TOKEN-SECRETO");
        assert_eq!(c.settings.server_url, "https://servidor.local");
        assert!(!c.settings.send_events);
        assert_eq!(c.settings.theme, "dark");
        assert!(c.printers.contains_key("ImpresoraDePrueba-xyz"));
        let disk = fs::read_to_string(s.dir.join("config.json")).unwrap();
        assert!(!disk.contains("deadbeef") && !disk.contains("sgm_robado") && !disk.contains("evil"));
        assert!(s.dir.join("config.json.bak").exists(), "falta la copia previa");
        // Errores: no se aplica nada.
        let antes = serde_json::to_string(&*s.config.lock().unwrap()).unwrap();
        let mal = json!({"format": "spoolguard-backup", "version": 1, "parametros": {"interval_secs": "x"}}).to_string();
        let e = do_import(&s, &mal, &Some(json!("reemplazar"))).unwrap_err();
        assert!(e.contains("no se aplicó nada"), "{e}");
        assert_eq!(antes, serde_json::to_string(&*s.config.lock().unwrap()).unwrap());
        // Versión futura y basura: rechazadas.
        assert!(do_import(&s, r#"{"format":"spoolguard-backup","version":99}"#, &None).unwrap_err().contains("no soportada"));
        assert!(do_import(&s, "no es json", &None).is_err());
    }

    #[test]
    fn exportar_no_trae_secretos_y_la_ida_y_vuelta_coincide() {
        let s = state("bk-rust-ida");
        s.config.lock().unwrap().printers.insert("ImpresoraDePrueba-xyz".into(), PrinterRules { enabled: true, apps: vec!["Word".into()], ..Default::default() });
        s.config.lock().unwrap().settings.theme = "dark".into();
        let out = build_backup(&s, &["reglas".into(), "parametros".into(), "registro".into()], None, None, None).unwrap();
        for w in ["HASH-SECRETO", "SAL-SECRETA", "TOKEN-SECRETO", "servidor.local"] {
            assert!(!out.contains(w), "el archivo contiene {w}");
        }
        let antes = snapshot_of(&s.config.lock().unwrap());
        s.config.lock().unwrap().printers.clear();
        s.config.lock().unwrap().settings.theme = "light".into();
        do_import(&s, &out, &Some(json!("reemplazar"))).unwrap();
        assert_eq!(snapshot_of(&s.config.lock().unwrap()), antes);
        assert!(build_backup(&s, &["bloqueo_rapido".into()], None, None, None).is_err());
        assert!(build_backup(&s, &[], None, None, None).is_err());
        assert!(build_backup(&s, &["contraseñas".into()], None, None, None).is_err());
    }

    #[test]
    fn registro_importado_es_idempotente_y_no_entra_en_la_cola_de_envio() {
        let s = state("bk-rust-registro");
        let f = file(json!({"registro": [
            {"id": "dddddddd-0001", "ts": 1700000000, "printer": "P", "decision": "block", "document": "a"},
            {"id": "dddddddd-0002", "ts": 1700000001, "printer": "P", "decision": "allow"}]}));
        let m = Some(json!({"registro": "combinar"}));
        let r = do_import(&s, &f, &m).unwrap();
        assert_eq!(r["eventos_importados"], 2);
        let n = s.events.count();
        let r2 = do_import(&s, &f, &m).unwrap();
        assert_eq!(r2["eventos_importados"], 0);
        assert_eq!(s.events.count(), n + 1, "solo el aviso de la segunda importación");
        assert_eq!(s.events.pending_count(), 0, "los importados no se envían al servidor");
        // Reemplazar borra lo anterior e importa lo del archivo.
        do_import(&s, &f, &Some(json!({"registro": "reemplazar"}))).unwrap();
        assert_eq!(s.events.count(), 3); // 2 importados + aviso
    }

    #[test]
    fn formato_antiguo_de_reglas_sigue_importandose() {
        let s = state("bk-rust-antiguo");
        let old = json!({"printers": {"ImpresoraDePrueba-xyz": {"enabled": false, "mode": "allow", "apps": ["*.pdf"]}},
            "global": {"enabled": false}, "settings": {"theme": "light", "server_token": "sgm_viejo", "password_hash": ""}}).to_string();
        do_import(&s, &old, &Some(json!({"reglas": "reemplazar", "parametros": "reemplazar"}))).unwrap();
        let c = s.config.lock().unwrap().clone();
        assert_eq!(c.printers["ImpresoraDePrueba-xyz"].apps, vec!["*.pdf".to_string()]);
        assert_eq!(c.settings.server_token, "sgm_TOKEN-SECRETO");
        assert_eq!(c.settings.password_hash, "HASH-SECRETO");
    }
}

#[cfg(test)]
mod net_tests {
    use super::backup_tests::state;
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn wait(s: &Shared) -> DiscJob {
        for _ in 0..200 {
            let j = s.disc.lock().unwrap().clone();
            if !j.running && j.started != 0 {
                return j;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("el descubrimiento no terminó");
    }

    #[test]
    fn descubrimiento_con_listeners_falsos_y_registro() {
        let s: Shared = Arc::new(state("net-prueba"));
        let raw = TcpListener::bind("127.0.0.1:0").unwrap();
        let ipp = TcpListener::bind("127.0.0.1:0").unwrap();
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let ports = discover::Ports { raw: raw.local_addr().unwrap().port(), lpd: free, ipp: ipp.local_addr().unwrap().port() };
        // IPP falso: responde un Get-Printer-Attributes mínimo por HTTP.
        let srv = thread::spawn(move || {
            // La primera conexión es el sondeo TCP del escaneo (se cierra sin enviar nada): se espera la petición IPP.
            let mut c = loop {
                let (mut c, _) = ipp.accept().unwrap();
                let mut buf = [0u8; 4096];
                if c.read(&mut buf).unwrap_or(0) > 0 {
                    break c;
                }
            };
            let mut body = vec![1, 1, 0, 0, 0, 0, 0, 1, 0x04];
            for (tag, name, val) in [(0x41u8, "printer-make-and-model", "Fake Laser 9000"), (0x42, "printer-name", "falsa")] {
                body.push(tag);
                body.extend((name.len() as u16).to_be_bytes());
                body.extend(name.as_bytes());
                body.extend((val.len() as u16).to_be_bytes());
                body.extend(val.as_bytes());
            }
            body.push(3);
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = c.write_all(&[head.as_bytes(), &body].concat());
        });
        // Sin subredes configuradas (por defecto) o con rangos no permitidos: no se escanea nada.
        for bad in [vec![], vec!["10.0.0.0/8".to_string()], vec!["8.8.8.0/24".to_string()]] {
            assert!(start_discovery(&s, bad, false, ports, "t".into()).is_err());
        }
        assert_eq!(s.disc.lock().unwrap().started, 0);
        set_net(&s, vec!["127.0.0.1".into()]);
        start_discovery(&s, vec![], false, ports, "ana (escritorio)".into()).unwrap();
        let j = wait(&s);
        srv.join().unwrap();
        assert_eq!(j.devices.len(), 1);
        let d = &j.devices[0];
        assert_eq!(d.device.ip, "127.0.0.1");
        assert!(d.device.ipp && d.device.model == "Fake Laser 9000" && d.device.ports.len() == 2);
        assert_eq!(d.managed, "sin_cola"); // ninguna cola apunta a 127.0.0.1
        assert!(d.explanation.contains("impresión directa"));
        // Quedó registrado quién y qué rango.
        let p = s.events.query(&Filter { text: "Descubrimiento de red".into(), ..Default::default() }, 10, 0).unwrap();
        assert!(p.rows.iter().any(|e| e.document.contains("iniciado por ana (escritorio)") && e.document.contains("127.0.0.1")), "{:?}", p.rows);
    }

    fn set_net(s: &State, cidrs: Vec<String>) {
        fs::write(s.dir.join(NET_FILE), serde_json::to_vec(&NetSettings { cidrs }).unwrap()).unwrap();
    }

    #[test]
    fn por_defecto_no_hay_subredes() {
        let s = state("net-vacio");
        assert!(load_net(&s.dir).cidrs.is_empty());
    }
}
