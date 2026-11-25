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
    path::Path,
    sync::{LazyLock, Mutex, MutexGuard},
    thread::JoinHandle,
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
        if j.spooling() && !j.printing() {
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
    let held = !state().released.contains(&id) && !j.done() && !(j.spooling() && !j.printing());
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
        if h.job(job_id)?.paused() {
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
    let outcome = match &before {
        Ok(j) if j.sent() => Cancelled::AlreadyPrinted,
        Ok(_) => Cancelled::Held,
        Err(_) => Cancelled::Gone,
    };
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
    Ok(outcome)
}
