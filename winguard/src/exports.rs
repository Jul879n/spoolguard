//! Exportaciones del print processor (winsplp.h). Cada una atrapa los panics y falla abierta: ante un error propio se
//! delega en `winprint.dll`. Nunca se bloquea más de `WAIT_MS` tras terminar de encolarse el trabajo.
use crate::sys::*;
use crate::verdict::{self, Verdict};
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const JOB_CONTROL_CANCEL: u32 = 3;
const JOB_CONTROL_DELETE: u32 = 5;
const JOB_CONTROL_RETAIN: u32 = 8;
const JOB_CONTROL_RELEASE: u32 = 9;
/// Sondeo del archivo de veredicto.
const POLL: Duration = Duration::from_millis(20);
/// Cada cuánto se consulta el estado del trabajo al spooler (solo para saber si sigue encolándose o lo borraron).
const STATUS_EVERY: Duration = Duration::from_millis(250);
/// Tope absoluto de espera (un trabajo enorme que tarda en encolarse).
const HARD_CAP: Duration = Duration::from_secs(300);

/// Tras un fallo abierto (la app no respondió) los trabajos siguientes esperan solo esto hasta que vuelva a llegar un veredicto.
/// El spooler llama al procesador de a un trabajo por cola: sin esto, 90 trabajos con la app cerrada tardarían 90 × 10 s (medido).
const DEGRADED_WAIT: Duration = Duration::from_millis(300);
/// Cuánto dura ese modo degradado antes de volver a esperar los 10 s completos (para detectar que la app volvió).
const DEGRADED_FOR: Duration = Duration::from_secs(60);

/// Un latido de la app más viejo que esto se considera «app no disponible».
const HEARTBEAT_FRESH: Duration = Duration::from_secs(5);

/// Lo que dice el latido de la app (`verdicts\.alive`: una cola por línea, las que la app controla ahora mismo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum App {
    /// La app vive y controla esta cola: se espera su veredicto con el plazo completo.
    Managed,
    /// La app vive y no controla esta cola (sin reglas): nadie va a escribir un veredicto, se imprime sin esperar.
    Unmanaged,
    /// Sin latido reciente (app cerrada o colgada): se espera, con falla abierta.
    Unknown,
}

fn app_state(dir: &std::path::Path, queue: &str) -> App {
    let p = dir.join(".alive");
    let fresh = std::fs::symlink_metadata(&p)
        .ok()
        .filter(|m| m.is_file() && m.len() <= 16_384)
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age <= HEARTBEAT_FRESH);
    if !fresh {
        return App::Unknown;
    }
    let Ok(text) = std::fs::read_to_string(&p) else { return App::Unknown };
    let want = verdict::sanitize(queue);
    if text.lines().any(|l| verdict::sanitize(l.trim()) == want) {
        App::Managed
    } else {
        App::Unmanaged
    }
}

static REAL: OnceLock<Option<Real>> = OnceLock::new();
static BASE: OnceLock<Instant> = OnceLock::new();
/// Milisegundos desde `BASE` (+1, 0 = nunca) del último fallo abierto y del último veredicto leído.
static LAST_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static LAST_VERDICT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

fn real() -> Option<&'static Real> {
    REAL.get_or_init(load_real).as_ref()
}

/// Contexto de un trabajo: es el `HANDLE` que el spooler nos devuelve en cada llamada.
struct Ctx {
    inner: Handle,
    queue: String,
    job_id: u32,
    open_doc: String,
    datatype: String,
    cancelled: AtomicBool,
    /// El acuse anterior de este trabajo decía «hold» (lo dejó retenido el modo estricto).
    was_held: AtomicBool,
}

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

fn trace(line: &str) {
    use std::io::Write;
    let base = program_data().join("SpoolGuard");
    if !base.join("trace.on").exists() {
        return;
    }
    let log = base.join("winguard.log");
    if std::fs::metadata(&log).map(|m| m.len() > 5_000_000).unwrap_or(false) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = writeln!(f, "{ms} {line}");
    }
}

/// Lee el veredicto si el archivo es regular (no enlace), pequeño y no es anterior al envío del trabajo
/// (un archivo viejo de otro trabajo con el mismo id no vale).
fn read_verdict(path: &std::path::Path, min_unix: Option<u64>) -> Option<Verdict> {
    let m = std::fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.len() > 16 {
        return None;
    }
    if let Some(min) = min_unix {
        let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
        if t + 2 < min {
            return None;
        }
    }
    verdict::parse(&std::fs::read(path).ok()?)
}

/// Qué hacer con el trabajo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Allow,
    Deny,
    /// Modo estricto sin veredicto a tiempo: no se imprime y el trabajo queda recuperable.
    Hold,
}

/// Última configuración leída bien (timeout_ms << 1 | estricto; 0 = ninguna). Si el archivo existe pero no se puede
/// leer (bloqueo momentáneo), se sigue con la última conocida en vez de volver a los valores por defecto.
static LAST_CFG: AtomicU64 = AtomicU64::new(0);

fn config(dir: &std::path::Path) -> verdict::Config {
    let p = dir.join("config");
    let known = || match LAST_CFG.load(Ordering::Relaxed) {
        0 => verdict::Config::default(),
        v => verdict::Config { timeout_ms: v >> 1, strict: v & 1 == 1 },
    };
    let meta = match std::fs::symlink_metadata(&p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            LAST_CFG.store(0, Ordering::Relaxed);
            return verdict::Config::default(); // sin archivo: valores por defecto (10 s, falla abierta)
        }
        Err(_) => return known(),
    };
    if !meta.is_file() || meta.len() > 256 {
        return verdict::Config::default(); // enlace o archivo enorme: no es nuestro, valores seguros
    }
    match std::fs::read(&p) {
        Ok(b) => {
            let c = verdict::parse_config(&b);
            LAST_CFG.store((c.timeout_ms << 1) | u64::from(c.strict), Ordering::Relaxed);
            c
        }
        Err(_) => known(),
    }
}

fn wait(ctx: &Ctx, doc: &str) -> Decision {
    let start = Instant::now();
    let dir = program_data().join("SpoolGuard").join("verdicts");
    // Sin carpeta de veredictos la protección no está instalada: no hay nada que esperar.
    if !dir.is_dir() {
        return Decision::Allow;
    }
    let cfg = config(&dir);
    let ack_path = dir.join(format!("{}.ack", verdict::file_name(&ctx.queue, ctx.job_id)));
    if std::fs::read(&ack_path).is_ok_and(|b| b.starts_with(b"hold")) {
        ctx.was_held.store(true, Ordering::Relaxed);
    }
    let app = app_state(&dir, &ctx.queue);
    if app == App::Unmanaged {
        trace(&format!("cola={:?} job={} tipo={:?} doc_open={:?} -> Allow (la app no controla esta cola)", ctx.queue, ctx.job_id, ctx.datatype, ctx.open_doc));
        return Decision::Allow;
    }
    let path = dir.join(verdict::file_name(&ctx.queue, ctx.job_id));
    let q = Queue::open(&ctx.queue);
    let (lt, lv) = (LAST_TIMEOUT.load(Ordering::Relaxed), LAST_VERDICT.load(Ordering::Relaxed));
    let degraded = app == App::Unknown && lt > lv && now_ms().saturating_sub(lt) < DEGRADED_FOR.as_millis() as u64;
    let patience = if degraded { DEGRADED_WAIT.min(Duration::from_millis(cfg.timeout_ms)) } else { Duration::from_millis(cfg.timeout_ms) };
    let mut deadline = start + patience;
    let (mut submitted, mut last_status) = (None, None::<Instant>);
    let mut first_status = String::new();
    let mut timed_out = false;
    let result = loop {
        let now = Instant::now();
        if ctx.cancelled.load(Ordering::Relaxed) {
            break Decision::Deny;
        }
        if let Some(q) = &q {
            if last_status.is_none_or(|t| now.duration_since(t) >= STATUS_EVERY) {
                last_status = Some(now);
                if let Some(st) = q.job(ctx.job_id) {
                    if first_status.is_empty() {
                        first_status = format!("status=0x{:x} doc_actual={:?}", st.status, st.document);
                    }
                    submitted = st.submitted_unix;
                    if st.status & (JOB_STATUS_DELETING | JOB_STATUS_DELETED) != 0 {
                        break Decision::Deny; // borrado (por la app o el usuario): no se imprime
                    }
                    // Mientras el spooler aún recibe los datos la app no puede evaluar el trabajo: el plazo cuenta desde que termina.
                    if st.status & JOB_STATUS_SPOOLING != 0 {
                        deadline = deadline.max(now + patience);
                    }
                }
            }
        }
        if let Some(v) = read_verdict(&path, submitted) {
            LAST_VERDICT.store(now_ms(), Ordering::Relaxed);
            break if v == Verdict::Deny { Decision::Deny } else { Decision::Allow };
        }
        if now >= deadline || now.duration_since(start) >= HARD_CAP + patience {
            LAST_TIMEOUT.store(now_ms(), Ordering::Relaxed);
            timed_out = true;
            // Falla abierta (por defecto) o, en modo estricto, el trabajo no se imprime.
            break if cfg.strict { Decision::Hold } else { Decision::Allow };
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(now)));
    };
    // Acuse para la app: qué se decidió con este trabajo (le sirve para registrar «bloqueado» solo si de verdad no salió).
    let ack = match (result, timed_out) {
        (Decision::Deny, _) => "deny",
        (Decision::Hold, _) => "hold",
        (Decision::Allow, true) => "timeout",
        (Decision::Allow, false) => "allow",
    };
    let acked = std::fs::write(dir.join(format!("{}.ack", verdict::file_name(&ctx.queue, ctx.job_id))), format!("{ack} {}", user_name())).is_ok();
    trace(&format!(
        "cola={:?} job={} tipo={:?} doc_open={:?} doc_print={:?} {} -> {:?} espero_ms={} degradado={} estricto={} plazo_ms={} usuario={:?} ack_escrito={}",
        ctx.queue,
        ctx.job_id,
        ctx.datatype,
        ctx.open_doc,
        doc,
        first_status,
        result,
        start.elapsed().as_millis(),
        degraded,
        cfg.strict,
        cfg.timeout_ms,
        user_name(),
        acked
    ));
    result
}

fn fail_not_found() {
    // SAFETY: SetLastError no tiene precondiciones.
    unsafe { SetLastError(ERROR_MOD_NOT_FOUND) };
}

/// # Safety
/// Llamada por el spooler con punteros válidos (winsplp.h).
#[no_mangle]
pub unsafe extern "system" fn OpenPrintProcessor(printer_name: *mut u16, data: *mut OpenData) -> Handle {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(r) = real() else {
            fail_not_found();
            return std::ptr::null_mut();
        };
        let inner = (r.open)(printer_name, data);
        if inner.is_null() {
            return inner; // winprint dejó su error
        }
        let (mut queue, mut job_id, mut open_doc, mut datatype) = (text(printer_name), 0, String::new(), String::new());
        if !data.is_null() {
            let d = &*data;
            job_id = d.job_id;
            open_doc = text(d.document_name);
            datatype = text(d.datatype);
            if !d.printer_name.is_null() {
                queue = text(d.printer_name);
            }
        }
        trace(&format!("OPEN cola={queue:?} job={job_id} tipo={datatype:?} doc={open_doc:?}"));
        Box::into_raw(Box::new(Ctx { inner, queue, job_id, open_doc, datatype, cancelled: AtomicBool::new(false), was_held: AtomicBool::new(false) })) as Handle
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `h` es el valor devuelto por `OpenPrintProcessor`.
#[no_mangle]
pub unsafe extern "system" fn PrintDocumentOnPrintProcessor(h: Handle, doc: *mut u16) -> Bool {
    let Some(r) = real() else {
        fail_not_found();
        return 0;
    };
    if h.is_null() {
        return 0;
    }
    let ctx = &*(h as *const Ctx);
    // Un panic en nuestra espera no debe tumbar spoolsv ni parar la impresión: se delega.
    let v = catch_unwind(AssertUnwindSafe(|| as_system(Decision::Allow, || wait(ctx, &text(doc))))).unwrap_or(Decision::Allow);
    if v == Decision::Deny {
        return 1; // el spooler da el trabajo por terminado sin que se haya enviado nada a la impresora
    }
    if v == Decision::Hold {
        // Estricto sin veredicto: no se delega en winprint, así que nada llega a la impresora. Medido en Windows 10: si se
        // devuelve error (FALSE) el spooler borra el trabajo (se perdería); en cambio, pedirle con SetJob que lo RETENGA
        // (JOB_CONTROL_RETAIN) y devolver éxito deja el trabajo en la cola como «Complete, Retained» sin haber enviado nada,
        // y reiniciarlo (JOB_CONTROL_RESTART) lo vuelve a procesar con el veredicto ya escrito. La app lo evalúa al volver.
        let r = as_system(None, || Queue::open_admin(&ctx.queue).map(|q| q.control(ctx.job_id, JOB_CONTROL_RETAIN)));
        as_system((), || trace(&format!("HOLD cola={:?} job={} retain={r:?}", ctx.queue, ctx.job_id)));
        if matches!(r, Some(Ok(()))) {
            return 1;
        }
        // No se pudo retener: el trabajo se pierde (sin imprimir) antes que dejarlo salir sin veredicto en modo estricto.
        SetLastError(ERROR_PRINT_CANCELLED_HOLD);
        return 0;
    }
    if ctx.was_held.load(Ordering::Relaxed) {
        // Venía retenido por el modo estricto y ya tiene veredicto «allow»: se suelta la retención para que, al imprimirse, el
        // trabajo salga de la cola como cualquier otro.
        let _ = as_system(None, || Queue::open_admin(&ctx.queue).map(|q| q.control(ctx.job_id, JOB_CONTROL_RELEASE)));
    }
    (r.print)(ctx.inner, doc)
}

/// # Safety
/// `h` es el valor devuelto por `OpenPrintProcessor`.
#[no_mangle]
pub unsafe extern "system" fn ClosePrintProcessor(h: Handle) -> Bool {
    let Some(r) = real() else {
        return 0;
    };
    if h.is_null() {
        return 0;
    }
    let ctx = Box::from_raw(h as *mut Ctx);
    (r.close)(ctx.inner)
}

/// # Safety
/// `h` es el valor devuelto por `OpenPrintProcessor`.
#[no_mangle]
pub unsafe extern "system" fn ControlPrintProcessor(h: Handle, command: u32) -> Bool {
    let Some(r) = real() else {
        return 0;
    };
    if h.is_null() {
        return 0;
    }
    let ctx = &*(h as *const Ctx);
    if matches!(command, JOB_CONTROL_CANCEL | JOB_CONTROL_DELETE) {
        ctx.cancelled.store(true, Ordering::Relaxed); // despierta la espera del veredicto
    }
    (r.control)(ctx.inner, command)
}

/// # Safety
/// Punteros del spooler (winsplp.h).
#[no_mangle]
pub unsafe extern "system" fn GetPrintProcessorCapabilities(value: *mut u16, attrs: u32, data: *mut u8, size: u32, needed: *mut u32) -> u32 {
    match real().and_then(|r| r.caps) {
        Some(f) => f(value, attrs, data, size, needed),
        None => {
            fail_not_found();
            0
        }
    }
}

/// # Safety
/// Punteros del spooler (winspool.h).
#[no_mangle]
pub unsafe extern "system" fn EnumPrintProcessorDatatypesW(
    name: *mut u16,
    processor: *mut u16,
    level: u32,
    datatypes: *mut u8,
    size: u32,
    needed: *mut u32,
    returned: *mut u32,
) -> Bool {
    match real() {
        Some(r) => (r.enum_datatypes)(name, processor, level, datatypes, size, needed, returned),
        None => {
            fail_not_found();
            0
        }
    }
}
