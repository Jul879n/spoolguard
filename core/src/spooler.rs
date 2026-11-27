//! Intercepción en el spooler de Windows (winspool): misma API que `cups`.
//!
//! Diseño de la retención
//! - `set_hold(true)` pone la cola en pausa (`PRINTER_CONTROL_PAUSE`): los trabajos se encolan pero no se
//!   imprimen mientras se evalúan. Con la cola en pausa no puede salir nada, por mucho que se retrase la evaluación.
//! - **No se evalúa un trabajo mientras el spooler aún lo recibe** (`JOB_STATUS_SPOOLING`): hasta entonces su nombre es
//!   provisional («Local Downlevel Document») y no tiene páginas; evaluarlo así lo daba por permitido y se imprimía un
//!   bloqueado. Era la causa de todas las fugas en ráfaga medidas (ver work/win-fix/INFORME.md). Un trabajo ya en
//!   impresión (cola activa) no admite espera y se evalúa igualmente.
//! - Liberar un solo trabajo no es posible con la cola pausada: `JOB_CONTROL_RESUME` sobre el trabajo no lo imprime
//!   (medido) y reanudar la cola planifica todo lo que no esté pausado. Por eso `release` sube el trabajo a la prioridad
//!   máxima, pausa uno a uno los demás pendientes, lo reanuda a él y reanuda la cola, y vuelve a pausar la cola en cuanto
//!   el trabajo pasa a imprimir (`settle`): un trabajo en impresión termina aunque la cola se pause (medido con puerto
//!   de archivo y TCP), así la cola solo está activa los milisegundos que tarda el spooler en tomar el trabajo, y lo que
//!   llegue en ese intervalo no se puede adelantar por tener menos prioridad. Los trabajos liberados esperando puerto
//!   (impresora ocupada) mantienen la cola activa hasta que empiezan a imprimir.
//! - Vigilante por cola retenida (`watch`): `FindFirstPrinterChangeNotification` avisa en ms de cada trabajo nuevo o
//!   cambiado y `sweep` pausa al instante los que no están liberados y vuelve a pausar la cola cuando corresponde. Solo
//!   toma `state()` un instante: nunca E/S de la aplicación (SQLite, red, CSV) ni el candado de retención de la app.
//!   El sondeo de `held_job_ids` (hilo de evaluación) sigue como respaldo.
//! - Los puertos `PORTPROMPT:`/`XPSPort:`/`nul:` no retienen (el spooler imprime aunque la
//!   cola esté en pausa); no hay forma de evitarlo desde aquí.
//! - Se requieren derechos de administrador sobre la impresora.
//!
//! IDs: el API compartido usa un `u32` y los JobId de Windows son por impresora, así que
//! se entrega un token propio (válido durante el proceso) que mapea a (impresora, JobId).

pub use crate::cups::{Cancelled, HeldJob};
use crate::rules::Job;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Graphics::Printing::*,
        System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects},
    },
};

#[derive(Default)]
struct State {
    next: u32,
    // La clave incluye la hora de envío: Windows puede reutilizar un JobId y el trabajo nuevo no debe
    // heredar el token (ni el estado «liberado») del anterior.
    by_token: HashMap<u32, (String, u32, u64)>,
    by_job: HashMap<(String, u32, u64), u32>,
    holding: HashSet<String>,
    released: HashSet<u32>,
    /// Colas con algún trabajo que aún se está encolando: no se evalúa todavía, pero conviene sondear rápido.
    spool_wait: HashSet<String>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Vigilante de una cola: evento de parada (HANDLE como entero para poder moverlo de hilo) e hilo.
struct Watcher {
    stop: isize,
    thread: JoinHandle<()>,
}

/// Serializa los cambios de pausa de la cola entre `release` (reanuda) y `settle` (vuelve a pausar): sin él, un `settle`
/// con una enumeración vieja pausaba la cola justo después de que `release` la reanudara y el liberado no imprimía.
/// Se mantiene solo durante unas pocas llamadas al spooler, nunca durante E/S ni esperas.
static QUEUE_OPS: Mutex<()> = Mutex::new(());

fn queue_ops() -> MutexGuard<'static, ()> {
    QUEUE_OPS.lock().unwrap_or_else(|e| e.into_inner())
}

static WATCHERS: LazyLock<Mutex<HashMap<String, Watcher>>> = LazyLock::new(Default::default);

fn watchers() -> MutexGuard<'static, HashMap<String, Watcher>> {
    WATCHERS.lock().unwrap_or_else(|e| e.into_inner())
}

fn token_in(s: &mut State, printer: &str, job: &Spooled) -> u32 {
    let key = (printer.to_string(), job.id, job.submitted);
    if let Some(t) = s.by_job.get(&key) {
        return *t;
    }
    s.next += 1;
    let t = s.next;
    s.by_token.insert(t, key.clone());
    s.by_job.insert(key, t);
    t
}

fn token(printer: &str, job: &Spooled) -> u32 {
    token_in(&mut state(), printer, job)
}

fn resolve(token: u32) -> Result<(String, u32), String> {
    state().by_token.get(&token).map(|(p, id, _)| (p.clone(), *id)).ok_or_else(|| format!("trabajo {token} desconocido"))
}

fn forget(token: u32) {
    let mut s = state();
    if let Some(k) = s.by_token.remove(&token) {
        s.by_job.remove(&k);
    }
    s.released.remove(&token);
}


fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn text(p: PWSTR) -> Option<String> {
    if p.is_null() {
        None
    } else {
        unsafe { p.to_string().ok() }
    }
}

fn last_error(ctx: &str) -> String {
    format!("{ctx}: {}", windows::core::Error::from(unsafe { GetLastError() }.to_hresult()))
}

struct Handle(PRINTER_HANDLE);

impl Handle {
    fn open(name: &str) -> Result<Self, String> {
        let w = wide(name);
        let mut h = PRINTER_HANDLE::default();
        let def = PRINTER_DEFAULTSW { DesiredAccess: PRINTER_ACCESS_ADMINISTER | PRINTER_ACCESS_USE, ..Default::default() };
        unsafe { OpenPrinterW(PCWSTR(w.as_ptr()), &mut h, Some(&def)) }
            .map_err(|e| {
                let denied = if e.code() == windows::core::HRESULT(0x8007_0005_u32 as i32) { format!("{}: ", crate::ACCESS_DENIED) } else { String::new() };
                format!("{denied}OpenPrinterW({name}): {e}")
            })?;
        Ok(Handle(h))
    }

    fn printer_paused(&self) -> Result<bool, String> {
        // Nivel 2: el nivel 6 responde "acceso denegado" con el handle de administración.
        let mut needed = 0u32;
        let _ = unsafe { GetPrinterW(self.0, 2, None, &mut needed) };
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        unsafe { GetPrinterW(self.0, 2, Some(bytes), &mut needed) }.map_err(|e| format!("GetPrinterW: {e}"))?;
        let info = unsafe { &*(buf.as_ptr() as *const PRINTER_INFO_2W) };
        Ok(info.Status & PRINTER_STATUS_PAUSED != 0)
    }

    /// Procesador de impresión de la cola (nivel 2, igual que `printer_paused`).
    fn print_processor(&self) -> Result<String, String> {
        let mut needed = 0u32;
        let _ = unsafe { GetPrinterW(self.0, 2, None, &mut needed) };
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        unsafe { GetPrinterW(self.0, 2, Some(bytes), &mut needed) }.map_err(|e| format!("GetPrinterW: {e}"))?;
        let info = unsafe { &*(buf.as_ptr() as *const PRINTER_INFO_2W) };
        Ok(text(info.pPrintProcessor).unwrap_or_default())
    }

    /// ¿La cola usa el print processor de SpoolGuard? Sus trabajos esperan el veredicto de la app antes de enviar datos.
    fn guarded(&self) -> bool {
        self.print_processor().is_ok_and(|p| p.eq_ignore_ascii_case(PROCESSOR))
    }

    fn printer_control(&self, cmd: u32) -> Result<(), String> {
        unsafe { SetPrinterW(self.0, 0, None, cmd) }.map_err(|e| format!("SetPrinterW: {e}"))
    }

    fn job_control(&self, id: u32, cmd: u32) -> Result<(), String> {
        if unsafe { SetJobW(self.0, id, 0, None, cmd) }.as_bool() {
            Ok(())
        } else {
            Err(last_error(&format!("SetJobW({id})")))
        }
    }

    fn jobs(&self) -> Result<Vec<Spooled>, String> {
        let (mut needed, mut count) = (0u32, 0u32);
        let _ = unsafe { EnumJobsW(self.0, 0, u32::MAX, 1, None, &mut needed, &mut count) };
        if needed == 0 {
            return Ok(vec![]);
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        unsafe { EnumJobsW(self.0, 0, u32::MAX, 1, Some(bytes), &mut needed, &mut count) }
            .map_err(|e| format!("EnumJobsW: {e}"))?;
        let infos = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const JOB_INFO_1W, count as usize) };
        Ok(infos.iter().map(Spooled::from).collect())
    }

    fn job(&self, id: u32) -> Result<Spooled, String> {
        let mut needed = 0u32;
        let _ = unsafe { GetJobW(self.0, id, 1, None, &mut needed) };
        if needed == 0 {
            return Err(last_error(&format!("GetJobW({id})")));
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        if !unsafe { GetJobW(self.0, id, 1, Some(bytes), &mut needed) }.as_bool() {
            return Err(last_error(&format!("GetJobW({id})")));
        }
        Ok(Spooled::from(unsafe { &*(buf.as_ptr() as *const JOB_INFO_1W) }))
    }

    /// Cambia la prioridad del trabajo (JOB_INFO_1: lee el registro entero, modifica `Priority` y lo devuelve).
    fn set_priority(&self, id: u32, priority: u32) -> Result<(), String> {
        let mut needed = 0u32;
        let _ = unsafe { GetJobW(self.0, id, 1, None, &mut needed) };
        if needed == 0 {
            return Err(last_error(&format!("GetJobW({id})")));
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        if !unsafe { GetJobW(self.0, id, 1, Some(bytes), &mut needed) }.as_bool() {
            return Err(last_error(&format!("GetJobW({id})")));
        }
        unsafe { (*(buf.as_mut_ptr() as *mut JOB_INFO_1W)).Priority = priority };
        if unsafe { SetJobW(self.0, id, 1, Some(buf.as_ptr() as *const u8), 0) }.as_bool() {
            Ok(())
        } else {
            Err(last_error(&format!("SetJobW({id}, prioridad)")))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { ClosePrinter(self.0) };
    }
}

struct Spooled {
    id: u32,
    status: u32,
    document: String,
    user: Option<String>,
    machine: Option<String>,
    total_pages: u32,
    /// Hora de envío (SYSTEMTIME aplanado): distingue un JobId reutilizado.
    submitted: u64,
}

impl From<&JOB_INFO_1W> for Spooled {
    fn from(j: &JOB_INFO_1W) -> Self {
        Spooled {
            id: j.JobId,
            status: j.Status,
            document: text(j.pDocument).unwrap_or_default(),
            user: text(j.pUserName),
            machine: text(j.pMachineName),
            total_pages: j.TotalPages,
            submitted: {
                let t = &j.Submitted;
                [t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds].iter().fold(0u64, |a, v| a.wrapping_mul(1_000_003).wrapping_add(u64::from(*v)))
            },
        }
    }
}

const DONE: u32 = JOB_STATUS_PRINTED
    | JOB_STATUS_COMPLETE
    | JOB_STATUS_DELETING
    | JOB_STATUS_DELETED;

impl Spooled {
    fn done(&self) -> bool {
        self.status & DONE != 0
    }
    /// El spooler aún recibe los datos del trabajo: nombre de documento y páginas no son definitivos.
    fn spooling(&self) -> bool {
        self.status & JOB_STATUS_SPOOLING != 0
    }
    /// Ya salió o está saliendo hacia la impresora.
    fn sent(&self) -> bool {
        self.status & (JOB_STATUS_PRINTING | JOB_STATUS_PRINTED | JOB_STATUS_COMPLETE) != 0
    }
    fn printing(&self) -> bool {
        self.status & JOB_STATUS_PRINTING != 0
    }
    fn paused(&self) -> bool {
        self.status & JOB_STATUS_PAUSED != 0
    }
}

/// Prioridad máxima de un trabajo en el spooler (MAXPRIORITY): el liberado se planifica antes que cualquier
/// otro que no esté pausado, por si llega uno nuevo mientras la cola está reanudada.
const MAX_PRIORITY: u32 = 99;

/// Vuelve a pausar la cola cuando ningún trabajo liberado espera turno. Un liberado que ya está imprimiendo
/// termina aunque la cola se pause (medido con un puerto TCP lento) y mientras tanto no se planifica nada nuevo,
/// así la cola solo queda activa lo que tarda el spooler en tomar el trabajo liberado.
fn settle(h: &Handle, printer: &str) {
    let _g = queue_ops();
    let Ok(jobs) = h.jobs() else { return };
    let waiting = {
        let mut s = state();
        jobs.iter().any(|j| {
            let t = token_in(&mut s, printer, j);
            s.released.contains(&t) && !j.done() && !j.printing()
        })
    };
    if !waiting && h.printer_paused() == Ok(false) {
        let _ = h.printer_control(PRINTER_CONTROL_PAUSE);
    }
}

/// Pausa de inmediato los trabajos nuevos no liberados de una cola retenida y vuelve a pausar la
/// cola si ya no quedan liberados esperando turno. Solo toma `state()` un instante (sin E/S bajo el candado).
fn sweep(h: &Handle, printer: &str) {
    if !state().holding.contains(printer) {
        return;
    }
    let Ok(jobs) = h.jobs() else { return };
    let fresh: Vec<(u32, u32)> = {
        let mut s = state();
        jobs.iter()
            .filter_map(|j| {
                let t = token_in(&mut s, printer, j);
                (!s.released.contains(&t) && !j.done() && !j.paused()).then_some((j.id, t))
            })
            .collect()
    };
    for (id, t) in fresh {
        // Se vuelve a mirar justo antes: `release` puede haber liberado el trabajo después de la enumeración.
        if !state().released.contains(&t) {
            // También los que ya empezaron a imprimir: pausar puede detener un trabajo grande a medias.
            let _ = h.job_control(id, JOB_CONTROL_PAUSE);
        }
    }
    settle(h, printer);
}

/// Hilo vigilante: espera avisos del spooler (trabajo añadido/cambiado) o el evento de parada.
fn watch(printer: &str, stop: isize) {
    let stop = HANDLE(stop as *mut core::ffi::c_void);
    let Ok(h) = Handle::open(printer) else { return };
    let n = unsafe { FindFirstPrinterChangeNotification(h.0, PRINTER_CHANGE_ADD_JOB | PRINTER_CHANGE_SET_JOB, 0, None) };
    if n.is_invalid() {
        return;
    }
    loop {
        // Tiempo máximo de espera: red de seguridad si el aviso se pierde; el sondeo cubre lo demás.
        let r = unsafe { WaitForMultipleObjects(&[n, stop], false, 2000) };
        if r == WAIT_OBJECT_0 {
            let mut changed = 0u32;
            let _ = unsafe { FindNextPrinterChangeNotification(n, Some(&mut changed), None, None) };
            sweep(&h, printer);
        } else if r != WAIT_TIMEOUT {
            break; // parada (WAIT_OBJECT_0 + 1) o error
        }
    }
    let _ = unsafe { FindClosePrinterChangeNotification(n) };
}

fn start_watch(printer: &str) {
    let mut w = watchers();
    if w.get(printer).is_some_and(|e| !e.thread.is_finished()) {
        return;
    }
    if let Some(old) = w.remove(printer) {
        let _ = old.thread.join();
        let _ = unsafe { CloseHandle(HANDLE(old.stop as *mut core::ffi::c_void)) };
    }
    let Ok(ev) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else { return };
    let stop = ev.0 as isize;
    let name = printer.to_string();
    match std::thread::Builder::new().name(format!("watch-{printer}")).spawn(move || watch(&name, stop)) {
        Ok(thread) => {
            w.insert(printer.to_string(), Watcher { stop, thread });
        }
        Err(_) => {
            let _ = unsafe { CloseHandle(ev) };
        }
    }
}

fn stop_watch(printer: &str) {
    let old = watchers().remove(printer);
    if let Some(old) = old {
        let ev = HANDLE(old.stop as *mut core::ffi::c_void);
        let _ = unsafe { SetEvent(ev) };
        let _ = old.thread.join();
        let _ = unsafe { CloseHandle(ev) };
    }
}

pub fn list_printers() -> Result<Vec<String>, String> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut count) = (0u32, 0u32);
    let _ = unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut count) };
    if needed == 0 {
        return Ok(vec![]);
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
    unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, Some(bytes), &mut needed, &mut count) }
        .map_err(|e| format!("EnumPrintersW: {e}"))?;
    let infos = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_4W, count as usize) };
    Ok(infos.iter().filter_map(|i| text(i.pPrinterName)).collect())
}

/// ¿La cola está en pausa ahora? (para no reanudar después una pausa puesta por el usuario).
pub fn is_paused(printer: &str) -> bool {
    Handle::open(printer).and_then(|h| h.printer_paused()).unwrap_or(false)
}

/// `leave_paused`: al quitar la retención no reanuda la cola (estaba pausada antes de retenerla).
pub fn set_hold(printer: &str, hold: bool, leave_paused: bool) -> Result<(), String> {
    let h = Handle::open(printer)?;
    if hold {
        h.printer_control(PRINTER_CONTROL_PAUSE)?;
        state().holding.insert(printer.to_string());
        start_watch(printer);
    } else {
        {
            let mut s = state();
            s.holding.remove(printer);
            s.spool_wait.remove(printer);
            let tokens: Vec<u32> = s.by_token.iter().filter(|(_, (p, _, _))| p == printer).map(|(t, _)| *t).collect();
            for t in tokens {
                s.released.remove(&t);
            }
        }
        stop_watch(printer);
        // Los trabajos que pausamos individualmente deben volver a salir.
        for j in h.jobs()? {
            if j.paused() {
                let _ = h.job_control(j.id, JOB_CONTROL_RESUME);
            }
        }
        if !leave_paused {
            h.printer_control(PRINTER_CONTROL_RESUME)?;
        }
    }
    Ok(())
}

/// Hay trabajos liberados aún en cola: la cola está activa y conviene sondear rápido.
pub fn busy() -> bool {
    let s = state();
    !s.released.is_empty() || !s.spool_wait.is_empty()
}

pub fn held_job_ids(printer: &str) -> Result<Vec<u32>, String> {
    let h = Handle::open(printer)?;
    let jobs = h.jobs()?;
    let holding = state().holding.contains(printer);
    let guarded = h.guarded();
    let live: HashSet<u32> = jobs.iter().map(|j| token(printer, j)).collect();
    let mut out = vec![];
    let mut still_spooling = false;
    for j in &jobs {
        let t = token(printer, j);
        if state().released.contains(&t) {
            // Liberado pero pausado (p. ej. 0x2011 = PRINTING|PAUSED): atascaría la cola.
            if j.paused() && !j.done() {
                let _ = h.job_control(j.id, JOB_CONTROL_RESUME);
            }
            continue;
        }
        // Modo estricto: el procesador no tuvo veredicto a tiempo, no envió nada y dejó el trabajo «Complete, Retained» con acuse
        // «hold»; se evalúa como cualquier retenido (permitir = reiniciarlo, bloquear = borrarlo).
        // Un retenido que alguien borró (p. ej. desde la cola de Windows) queda «Deleting, Retained» para siempre: se suelta.
        if guarded && j.status & JOB_STATUS_DELETING != 0 && j.status & JOB_STATUS_RETAINED != 0 {
            let _ = h.job_control(j.id, JOB_CONTROL_RELEASE);
            continue;
        }
        if guarded && strict_held(&h, printer, j) {
            out.push(t);
            continue;
        }
        if j.done() {
            continue;
        }
        // Uno en impresión nunca evaluado se devuelve para evaluarlo (permitir o cancelar);
        // pausarlo ya no sirve.
        if holding && !j.printing() && !j.paused() && !h.printer_paused()? {
            let _ = h.job_control(j.id, JOB_CONTROL_PAUSE);
        }
        // Mientras el spooler recibe los datos el nombre del documento es provisional («Local Downlevel Document») y las páginas
        // no existen: evaluarlo entonces permite un trabajo bloqueado (medido). Se evalúa cuando termina de encolarse, salvo
        // que ya haya empezado a imprimir (cola activa), que no admite espera.
        // En una cola con el procesador de SpoolGuard un trabajo que ya «imprime» solo está esperando el veredicto (no ha enviado
        // nada): se espera igual a que termine de encolarse, para evaluarlo con su nombre real.
        if j.spooling() && (!j.printing() || guarded) {
            still_spooling = true;
            continue;
        }
        if holding || j.paused() || j.printing() {
            out.push(t);
        }
    }
    {
        let mut s = state();
        if still_spooling {
            s.spool_wait.insert(printer.to_string());
        } else {
            s.spool_wait.remove(printer);
        }
        let gone: Vec<u32> = s
            .by_token
            .iter()
            .filter(|(t, (p, _, _))| p == printer && !live.contains(t))
            .map(|(t, _)| *t)
            .collect();
        for t in gone {
            if let Some(k) = s.by_token.remove(&t) {
                s.by_job.remove(&k);
            }
            s.released.remove(&t);
        }
    }
    if holding {
        settle(&h, printer);
    }
    Ok(out)
}

pub fn job_info(_work_dir: &Path, id: u32) -> Result<HeldJob, String> {
    let (printer, job_id) = resolve(id)?;
    let h = Handle::open(&printer)?;
    let j = h.job(job_id)?;
    let local = std::env::var("COMPUTERNAME").unwrap_or_default();
    // Windows informa el nombre del equipo que envió el trabajo (`\\EQUIPO`), no su IP; la aplicación puede
    // resolverlo a IP al evaluar. Los trabajos locales no traen equipo de origen.
    let origin_host = j
        .machine
        .as_deref()
        .map(|m| m.trim_start_matches('\\').to_string())
        .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case(&local));
    let strict = h.guarded() && strict_held(&h, &printer, &j);
    let held = !state().released.contains(&id) && (!j.done() || strict) && !(j.spooling() && !j.printing());
    Ok(HeldJob {
        id,
        held,
        job: Job { document: j.document, app: None, user: j.user, origin_ip: None, origin_host, site: None },
        // JOB_INFO_1 trae TotalPages (0 hasta que el spooler lo calcula: se trata como desconocido); el tamaño en bytes
        // (Size) solo está en JOB_INFO_2 y no se consulta.
        pages: Some(u64::from(j.total_pages)).filter(|n| *n > 0),
        bytes: None,
    })
}

pub fn release(id: u32) -> Result<(), String> {
    let (printer, job_id) = resolve(id)?;
    let h = Handle::open(&printer)?;
    let holding = state().holding.contains(&printer);
    if holding {
        // Con la cola reanudada el spooler planifica el trabajo de mayor prioridad que no esté pausado: el
        // liberado pasa a prioridad máxima y los demás se pausan uno a uno, de modo que lo que llegue ahora
        // (aún sin pausar) no puede adelantarse mientras la cola está activa.
        let _ = h.set_priority(job_id, MAX_PRIORITY);
        let jobs = h.jobs()?;
        let others: Vec<u32> = {
            let mut s = state();
            jobs.iter()
                .filter(|j| {
                    let t = token_in(&mut s, &printer, j);
                    j.id != job_id && !j.done() && !j.printing() && !j.paused() && !s.released.contains(&t)
                })
                .map(|j| j.id)
                .collect()
        };
        for o in others {
            // Si el trabajo ya no existe (impreso, borrado) no hay nada que retener.
            if let Err(e) = h.job_control(o, JOB_CONTROL_PAUSE) {
                if h.job(o).is_ok() {
                    return Err(e);
                }
            }
        }
    }
    // Se marca como liberado antes de reanudar (el siguiente sondeo no debe volver a pausarlo),
    // pero se deshace si la reanudación falla para que el trabajo se reevalúe.
    let guard = queue_ops();
    state().released.insert(id);
    let r = (|| -> Result<(), String> {
        let cur = h.job(job_id)?;
        if h.guarded() && strict_held(&h, &printer, &cur) {
            h.job_control(job_id, JOB_CONTROL_RESTART)?; // retenido por el modo estricto: se vuelve a procesar con el veredicto ya escrito
        } else if cur.paused() {
            h.job_control(job_id, JOB_CONTROL_RESUME)?;
        }
        if holding && h.printer_paused()? {
            h.printer_control(PRINTER_CONTROL_RESUME)?;
        }
        Ok(())
    })();
    drop(guard);
    if r.is_err() {
        state().released.remove(&id);
        return r;
    }
    if holding {
        // La cola solo debe estar activa hasta que el spooler tome el trabajo (milisegundos): en cuanto pasa a
        // imprimir (o ya desapareció, impreso) se vuelve a pausar. Si el puerto está ocupado con otro liberado,
        // se deja al vigilante (`settle`) hacerlo cuando le toque.
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_millis(300) {
            match h.job(job_id) {
                Ok(j) if !j.printing() && !j.done() => std::thread::sleep(std::time::Duration::from_millis(1)),
                _ => break,
            }
        }
        settle(&h, &printer);
    }
    Ok(())
}

/// Cancela un trabajo. Lee antes su estado: si ya estaba imprimiendo o impreso, borrarlo no deshace lo que salió y
/// el resultado lo dice (`AlreadyPrinted`) para que el registro no afirme que se bloqueó.
pub fn cancel(id: u32) -> Result<Cancelled, String> {
    let (printer, job_id) = resolve(id)?;
    let h = Handle::open(&printer)?;
    let before = h.job(job_id);
    // En una cola con el procesador de SpoolGuard «imprimiendo» puede ser solo esperar el veredicto: se confirma con su acuse.
    let held_strict = h.guarded() && before.as_ref().is_ok_and(|j| strict_held(&h, &printer, j));
    let waiting = h.guarded() && before.as_ref().is_ok_and(|j| j.printing() && j.status & (JOB_STATUS_PRINTED | JOB_STATUS_COMPLETE) == 0);
    let outcome = match &before {
        Ok(_) if waiting || held_strict => Cancelled::Held, // en modo estricto nada salió: el procesador lo retuvo sin enviar datos
        Ok(j) if j.sent() => Cancelled::AlreadyPrinted,
        Ok(_) => Cancelled::Held,
        Err(_) => Cancelled::Gone,
    };
    if held_strict {
        // Retenido por el modo estricto: nada salió. Soltar la retención lo saca de la cola (borrarlo lo dejaría «Deleting, Retained»).
        let _ = h.job_control(job_id, JOB_CONTROL_RELEASE);
        forget(id);
        return Ok(Cancelled::Held);
    }
    if outcome != Cancelled::Gone {
        if let Err(e) = h.job_control(job_id, JOB_CONTROL_DELETE) {
            // Terminó entre la lectura y el borrado.
            if h.job(job_id).is_ok() {
                return Err(e);
            }
            forget(id);
            return Ok(Cancelled::Gone);
        }
    }
    forget(id);
    if waiting {
        // El procesador anota «deny» al descartar los datos; sin ese acuse no se puede afirmar que no salió.
        return Ok(if ack(&printer, job_id, Duration::from_millis(1500)).as_deref() == Some("deny") { Cancelled::Held } else { Cancelled::AlreadyPrinted });
    }
    Ok(outcome)
}

// ---- Protección previa a la impresión (print processor `spoolguardpp`, crate `winguard`) ----
// Cada trabajo de una cola protegida espera, dentro del spooler y antes de enviar datos a la impresora, el veredicto
// `allow`/`deny` que escribe `write_verdict` (misma idea que el filtro de CUPS; ver docs/proteccion-previa.md).

/// Nombre con el que se registra el print processor (debe coincidir con scripts/guard-windows.ps1).
pub const PROCESSOR: &str = "spoolguardpp";

/// Instalador (se ejecuta elevado: ver `guard_install` en src-tauri).
pub const GUARD_SCRIPT: &str = include_str!("../../scripts/guard-windows.ps1");

/// `%ProgramData%\SpoolGuard\verdicts`: solo escriben SYSTEM, Administradores y el usuario que ejecuta la app.
pub fn verdicts_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("SpoolGuard").join("verdicts")
}

/// Nombre del archivo de veredicto de un trabajo. Misma regla que `winguard::verdict::file_name` (probada con
/// `winguard/tests/names.txt` en ambos crates): sin separadores de ruta ni caracteres reservados, 128 caracteres como máximo.
pub fn verdict_file_name(queue: &str, job_id: u32) -> String {
    let q: String = queue
        .chars()
        .map(|c| if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .take(128)
        .collect();
    format!("{}-{}", if q.is_empty() { "_" } else { &q }, job_id)
}

/// Deja el veredicto (`allow`/`deny`) que el print processor espera: archivo temporal en la misma carpeta y renombrado
/// (el procesador nunca ve un archivo a medias). Sin la carpeta (protección no instalada) no hace nada.
pub fn write_verdict(printer: &str, token: u32, allowed: bool) {
    let dir = verdicts_dir();
    let Ok((queue, job_id)) = resolve(token) else { return };
    if !dir.is_dir() {
        return;
    }
    let name = verdict_file_name(&queue, job_id);
    let tmp = dir.join(format!(".{name}.tmp"));
    let dst = dir.join(&name);
    let _ = printer;
    // Acuse de un trabajo anterior con el mismo id (salvo «hold»: es de este trabajo y el procesador lo usa al reprocesarlo).
    let ack_path = dir.join(format!("{name}.ack"));
    if !std::fs::read(&ack_path).is_ok_and(|b| b.starts_with(b"hold")) {
        let _ = std::fs::remove_file(&ack_path);
    }
    if std::fs::write(&tmp, if allowed { "allow" } else { "deny" }).is_ok() && std::fs::rename(&tmp, &dst).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    purge_verdicts(&dir);
}

/// Latido de la app para el procesador (`verdicts\.alive`): las colas que controla ahora mismo, una por línea. Con latido
/// reciente el procesador espera el veredicto completo en esas colas y deja pasar sin esperar las demás (nadie escribiría
/// un veredicto); sin latido (app cerrada o colgada) espera con falla abierta. Como mucho una vez por segundo.
pub fn heartbeat(queues: &[String]) {
    static LAST: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(Default::default);
    {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some_and(|t| t.elapsed() < Duration::from_millis(1000)) {
            return;
        }
        *l = Some(Instant::now());
    }
    let dir = verdicts_dir();
    if !dir.is_dir() {
        return;
    }
    let body = queues.iter().filter(|q| !q.contains(['\n', '\r'])).cloned().collect::<Vec<_>>().join("\n");
    let tmp = dir.join(".alive.tmp");
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, dir.join(".alive")).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Veredictos y acuses de más de un día se limpian de paso, como mucho cada 10 minutos (sin hilo aparte).
fn purge_verdicts(dir: &Path) {
    static LAST: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(Default::default);
    {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
            return;
        }
        *l = Some(Instant::now());
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name() == "config" || e.file_name().to_string_lossy().starts_with("controlled-") {
                continue; // la configuración y las marcas de control no caducan
            }
            if e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() > 86_400) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Deja la espera del veredicto (segundos, 1-600) y el modo estricto para el procesador (`verdicts\config`), de forma atómica
/// y solo si cambió. Un valor fuera de rango se corrige al seguro (10 s). Sin la carpeta no hace nada.
pub fn write_config(timeout_secs: u64, strict: bool) {
    static LAST: LazyLock<Mutex<Option<String>>> = LazyLock::new(Default::default);
    let dir = verdicts_dir();
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
    if std::fs::write(&tmp, &body).is_ok() {
        if std::fs::rename(&tmp, &dst).is_ok() {
            *last = Some(body);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// Trabajo que el modo estricto dejó retenido: terminado y retenido en la cola («Complete, Retained») y con acuse `hold`.
fn strict_held(_h: &Handle, printer: &str, j: &Spooled) -> bool {
    j.status & JOB_STATUS_COMPLETE != 0 && j.status & JOB_STATUS_RETAINED != 0 && j.status & JOB_STATUS_DELETING == 0 && ack(printer, j.id, Duration::ZERO).as_deref() == Some("hold")
}

/// Acuse del procesador (`deny`, `allow`, `timeout` o `hold`) de un trabajo; espera hasta `wait` a que aparezca.
fn ack(printer: &str, job_id: u32, wait: Duration) -> Option<String> {
    let path = verdicts_dir().join(format!("{}.ack", verdict_file_name(printer, job_id)));
    let t0 = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(&path) {
            return s.split_whitespace().next().map(str::to_string);
        }
        if t0.elapsed() >= wait {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Nombres de los drivers V4 instalados. El spooler no llama al print processor de las colas con driver V4 (medido, ver
/// work/winguard/INFORME.md), así que no se pueden proteger ni cuentan en «protegibles».
fn v4_drivers() -> HashSet<String> {
    let (mut needed, mut count) = (0u32, 0u32);
    let _ = unsafe { EnumPrinterDriversW(PCWSTR::null(), PCWSTR::null(), 2, None, &mut needed, &mut count) };
    if needed == 0 {
        return HashSet::new();
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
    if unsafe { EnumPrinterDriversW(PCWSTR::null(), PCWSTR::null(), 2, Some(bytes), &mut needed, &mut count) }.is_err() {
        return HashSet::new();
    }
    let infos = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const DRIVER_INFO_2W, count as usize) };
    infos.iter().filter(|d| d.cVersion >= 4).filter_map(|d| text(d.pName)).collect()
}

/// (colas con el procesador de SpoolGuard, colas protegibles). Protegibles: locales que usan `winprint` (o ya el nuestro)
/// con driver V3, salvo el fax; las que usan un procesador de otro fabricante o un driver V4 se omiten (como las colas sin
/// PPD en CUPS). No pide permisos.
pub fn guard_status() -> (usize, usize) {
    let (mut needed, mut count) = (0u32, 0u32);
    let _ = unsafe { EnumPrintersW(PRINTER_ENUM_LOCAL, PCWSTR::null(), 2, None, &mut needed, &mut count) };
    if needed == 0 {
        return (0, 0);
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
    if unsafe { EnumPrintersW(PRINTER_ENUM_LOCAL, PCWSTR::null(), 2, Some(bytes), &mut needed, &mut count) }.is_err() {
        return (0, 0);
    }
    let infos = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_2W, count as usize) };
    let v4 = v4_drivers();
    let (mut on, mut total) = (0, 0);
    for i in infos {
        if text(i.pDriverName).is_some_and(|d| v4.contains(&d)) {
            continue;
        }
        let proc = text(i.pPrintProcessor).unwrap_or_default();
        let port = text(i.pPortName).unwrap_or_default();
        if proc.eq_ignore_ascii_case(PROCESSOR) {
            on += 1;
            total += 1;
        } else if proc.eq_ignore_ascii_case("winprint") && !port.eq_ignore_ascii_case("SHRFAX:") {
            total += 1;
        }
    }
    (on, total)
}

#[cfg(test)]
mod guard_tests {
    use super::verdict_file_name;

    #[test]
    fn nombres_compartidos_con_el_procesador() {
        for l in include_str!("../../winguard/tests/names.txt").lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let mut p = l.splitn(3, '\t');
            let (q, id, want) = (p.next().unwrap_or(""), p.next().unwrap_or("0"), p.next().unwrap_or(""));
            assert_eq!(verdict_file_name(q, id.parse().unwrap_or(0)), want, "{l}");
        }
    }
}
