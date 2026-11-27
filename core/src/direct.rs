//! Detección de impresión directa (mejor esfuerzo): compara el contador de la propia impresora, consultado por
//! IPP, con lo que esta app liberó. Fuente 1: `printer-impressions-completed` (Get-Printer-Attributes).
//! Fuente 2: trabajos terminados que la impresora recuerda (Get-Jobs completed, `job-impressions-completed`).
//! Si ninguna existe, la impresora figura como "no verificable". Mismo comportamiento que
//! `internal/app/direct.go` del servidor. NO se probó con hardware real.

use crate::discover::{encode_request, get_printer_attributes, parse_groups, printer_uri, Transport};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// Un trabajo liberado puede imprimirse algo después de lo que la app registró.
pub const GRACE_SECS: u64 = 120;
pub const SRC_PRINTER: &str = "printer-impressions-completed";
pub const SRC_JOBS: &str = "job-impressions-completed";
/// Tope de ids de trabajos recordados por impresora.
const MAX_SEEN: usize = 4096;
pub const DEFAULT_SECS: u64 = 300;
pub const MIN_SECS: u64 = 30;
pub const MAX_SECS: u64 = 86400;

/// Trabajo terminado que recuerda la impresora (`impressions` = -1 si no lo informa).
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedJob {
    pub id: i64,
    pub impressions: i64,
}

/// Get-Jobs (`which-jobs=completed`) con `job-id` y `job-impressions-completed`.
pub fn get_completed_jobs(tr: Transport, host: &str, port: u16) -> Result<Vec<CompletedJob>, String> {
    let req = encode_request(0x000A, &printer_uri(host, port), &["job-id", "job-impressions-completed"], &[(0x44, "which-jobs", b"completed"), (0x21, "limit", &[0, 0, 0, 100])]);
    let (status, groups) = parse_groups(&tr(host, port, &req)?)?;
    if status >= 0x0100 {
        return Err(format!("IPP status 0x{status:04x}"));
    }
    let num = |a: &[(String, String)], k: &str| a.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.parse::<i64>().ok());
    Ok(groups.iter().filter(|(g, _)| *g == 0x02).filter_map(|(_, a)| Some(CompletedJob { id: num(a, "job-id")?, impressions: num(a, "job-impressions-completed").filter(|v| *v >= 0).unwrap_or(-1) })).collect())
}

/// Resultado de una consulta: valor acumulado (fuente 1) o conjunto de trabajos (fuente 2).
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    Counter(i64),
    Jobs(Vec<CompletedJob>),
}

impl Reading {
    fn source(&self) -> &'static str {
        match self {
            Reading::Counter(_) => SRC_PRINTER,
            Reading::Jobs(_) => SRC_JOBS,
        }
    }
}

pub fn read_counter(tr: Transport, host: &str, port: u16) -> Result<Reading, String> {
    let attrs = get_printer_attributes(tr, host, port, &[SRC_PRINTER])?;
    if let Some(n) = attrs.iter().find(|(k, _)| k == SRC_PRINTER).and_then(|(_, v)| v.parse::<i64>().ok()).filter(|n| *n >= 0) {
        return Ok(Reading::Counter(n));
    }
    get_completed_jobs(tr, host, port).map(Reading::Jobs).map_err(|e| format!("la impresora no expone contador por IPP: {e}"))
}

/// Estado por impresora; los campos públicos son los que ve la interfaz.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub printer: String,
    pub host: String,
    /// `SRC_PRINTER` | `SRC_JOBS` | vacío (no verificable).
    pub source: String,
    /// Último valor del contador (fuente 1).
    pub total: i64,
    pub last_poll: u64,
    pub error: String,
    pub alerts: u32,
    #[serde(skip)]
    prev_poll: u64,
    #[serde(skip)]
    seen: HashSet<i64>,
    #[serde(skip)]
    ready: bool,
}

impl Status {
    pub fn new(printer: &str, host: &str) -> Status {
        Status { printer: printer.into(), host: host.into(), ..Default::default() }
    }
}

/// Estado de todas las impresoras vigiladas; apagar la detección lo reinicia (se pierde la línea base).
#[derive(Debug, Default)]
pub struct Monitor {
    pub last_at: u64,
    pub printers: BTreeMap<String, Status>,
}

impl Monitor {
    pub fn reset(&mut self) {
        *self = Monitor::default();
    }
    pub fn due(&self, now: u64, interval: u64) -> bool {
        now.saturating_sub(self.last_at) >= interval
    }
    pub fn status(&self) -> Vec<Status> {
        self.printers.values().cloned().collect()
    }
}

/// Aumento sospechoso: `delta` en `unit` ("impresiones" o "trabajos").
#[derive(Debug, PartialEq)]
pub struct Alert {
    pub delta: i64,
    pub unit: &'static str,
}

impl Alert {
    pub fn text(&self, printer: &str) -> String {
        format!("Impresión directa sospechosa en {printer}: +{} {} según la impresora sin trabajos liberados por esta app que lo expliquen", self.delta, self.unit)
    }
}

/// ¿Lo que se liberó para la cola (páginas de cada trabajo permitido desde `from`) explica el aumento?
/// Sin trabajos liberados no hay explicación. Con trabajos, solo se descarta la alerta si no se puede comparar
/// (alguno sin páginas) o si el aumento no supera lo liberado (en trabajos si la unidad es "trabajos").
pub fn explained_by_local(released: &[Option<u64>], delta: i64, unit: &str) -> bool {
    if released.is_empty() {
        return false;
    }
    if unit == "trabajos" {
        return delta <= released.len() as i64;
    }
    if released.contains(&None) {
        return true;
    }
    delta <= released.iter().flatten().map(|p| *p as i64).sum::<i64>()
}

/// Aplica una lectura al estado. La primera lectura (o un cambio de fuente) fija la línea base y nunca alerta.
/// `explained(desde, aumento, unidad)` dice si lo liberado localmente explica el aumento.
pub fn poll(st: &mut Status, rd: Result<Reading, String>, at: u64, explained: &dyn Fn(u64, i64, &str) -> bool) -> Option<Alert> {
    st.last_poll = at;
    let rd = match rd {
        Ok(r) => r,
        Err(e) => {
            st.error = format!("no verificable: {e}");
            (st.source, st.ready) = (String::new(), false);
            return None;
        }
    };
    st.error.clear();
    let since = st.prev_poll;
    if !st.ready || st.source != rd.source() {
        (st.source, st.ready, st.prev_poll) = (rd.source().into(), true, at);
        st.seen.clear();
        match rd {
            Reading::Counter(n) => st.total = n,
            Reading::Jobs(j) => (st.total, st.seen) = (0, j.iter().map(|x| x.id).collect()),
        }
        return None;
    }
    let (delta, unit) = match rd {
        Reading::Counter(n) => {
            let d = n - st.total;
            st.total = n; // si baja (reinicio del contador) solo se rebasa
            (d, "impresiones")
        }
        Reading::Jobs(jobs) => {
            let (mut nj, mut imp, mut known) = (0i64, 0i64, true);
            for j in &jobs {
                if st.seen.contains(&j.id) {
                    continue;
                }
                if st.seen.len() < MAX_SEEN {
                    st.seen.insert(j.id);
                }
                nj += 1;
                if j.impressions >= 0 {
                    imp += j.impressions;
                } else {
                    known = false;
                }
            }
            if known { (imp, "impresiones") } else { (nj, "trabajos") }
        }
    };
    st.prev_poll = at;
    if delta <= 0 || explained(since.saturating_sub(GRACE_SECS), delta, unit) {
        return None;
    }
    st.alerts += 1;
    Some(Alert { delta, unit })
}

/// Una pasada para una impresora: consulta por IPP (`tr` inyectable) y aplica la lectura.
pub fn poll_target(st: &mut Status, tr: Transport, host: &str, port: u16, at: u64, explained: &dyn Fn(u64, i64, &str) -> bool) -> Option<Alert> {
    poll(st, read_counter(tr, host, port), at, explained)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn attr(b: &mut Vec<u8>, tag: u8, name: &str, v: &[u8]) {
        b.push(tag);
        b.extend((name.len() as u16).to_be_bytes());
        b.extend(name.as_bytes());
        b.extend((v.len() as u16).to_be_bytes());
        b.extend(v);
    }
    fn int(n: i32) -> [u8; 4] {
        n.to_be_bytes()
    }
    fn resp(status: u16, groups: &[(u8, Vec<(&str, i32)>)]) -> Vec<u8> {
        let mut b = vec![1, 1, (status >> 8) as u8, status as u8, 0, 0, 0, 1];
        for (g, attrs) in groups {
            b.push(*g);
            for (n, v) in attrs {
                attr(&mut b, 0x21, n, &int(*v));
            }
        }
        b.push(3);
        b
    }

    /// Impresora falsa: contador (fuente 1) y/o trabajos recordados (fuente 2), ajustables entre pasadas.
    #[derive(Default)]
    struct Fake {
        counter: Mutex<Option<i32>>,
        jobs: Mutex<Option<Vec<(i32, i32)>>>, // (id, impresiones; -1 = no informa)
    }
    impl Fake {
        fn call(&self, req: &[u8]) -> Result<Vec<u8>, String> {
            match req[3] {
                0x0B => Ok(match *self.counter.lock().unwrap() {
                    Some(n) => resp(0, &[(0x04, vec![(SRC_PRINTER, n)])]),
                    None => resp(0, &[(0x04, vec![])]),
                }),
                0x0A => match &*self.jobs.lock().unwrap() {
                    Some(j) => Ok(resp(0, &j.iter().map(|(id, n)| (0x02, if *n >= 0 { vec![("job-id", *id), (SRC_JOBS, *n)] } else { vec![("job-id", *id)] })).collect::<Vec<_>>())),
                    None => Ok(resp(0x0400, &[])),
                },
                _ => Err("op".into()),
            }
        }
        fn step(&self, st: &mut Status, at: u64, released: &[Option<u64>]) -> Option<Alert> {
            let tr = |_: &str, _: u16, r: &[u8]| self.call(r);
            poll_target(st, &tr, "h", 631, at, &|_, d, u| explained_by_local(released, d, u))
        }
    }

    #[test]
    fn trabajos_completados_por_ipp() {
        let f = Fake::default();
        *f.jobs.lock().unwrap() = Some(vec![(7, 3), (8, -1)]);
        let tr = |_: &str, _: u16, r: &[u8]| f.call(r);
        assert_eq!(get_completed_jobs(&tr, "h", 631).unwrap(), vec![CompletedJob { id: 7, impressions: 3 }, CompletedJob { id: 8, impressions: -1 }]);
        *f.jobs.lock().unwrap() = None;
        assert!(get_completed_jobs(&tr, "h", 631).is_err());
    }

    #[test]
    fn linea_base_sin_alerta_y_delta_fuente_1() {
        let f = Fake::default();
        let mut st = Status::default();
        *f.counter.lock().unwrap() = Some(100);
        assert_eq!(f.step(&mut st, 1000, &[]), None);
        assert_eq!((st.source.as_str(), st.total, st.alerts), (SRC_PRINTER, 100, 0));
        assert_eq!(f.step(&mut st, 1300, &[]), None); // sin cambio
        *f.counter.lock().unwrap() = Some(105);
        assert_eq!(f.step(&mut st, 1600, &[]), Some(Alert { delta: 5, unit: "impresiones" }));
        assert_eq!((st.total, st.alerts), (105, 1));
    }

    #[test]
    fn delta_fuente_2_con_trabajos_nuevos() {
        let f = Fake::default();
        let mut st = Status::default();
        *f.jobs.lock().unwrap() = Some(vec![(1, 4)]);
        assert_eq!(f.step(&mut st, 1000, &[]), None); // base: el trabajo 1 ya existía
        assert_eq!(st.source, SRC_JOBS);
        *f.jobs.lock().unwrap() = Some(vec![(1, 4), (2, 3), (3, 2)]);
        assert_eq!(f.step(&mut st, 1300, &[]), Some(Alert { delta: 5, unit: "impresiones" }));
        assert_eq!(f.step(&mut st, 1600, &[]), None); // ya vistos
        *f.jobs.lock().unwrap() = Some(vec![(1, 4), (2, 3), (3, 2), (4, -1), (5, 2)]);
        assert_eq!(f.step(&mut st, 1900, &[]), Some(Alert { delta: 2, unit: "trabajos" })); // uno sin páginas: cuenta trabajos
    }

    #[test]
    fn sin_alerta_si_lo_explican_los_trabajos_liberados() {
        let f = Fake::default();
        let mut st = Status::default();
        *f.counter.lock().unwrap() = Some(10);
        f.step(&mut st, 1000, &[]);
        *f.counter.lock().unwrap() = Some(15);
        assert_eq!(f.step(&mut st, 1300, &[Some(3), Some(2)]), None); // 5 <= 5
        *f.counter.lock().unwrap() = Some(25);
        assert_eq!(f.step(&mut st, 1600, &[Some(3), Some(2)]), Some(Alert { delta: 10, unit: "impresiones" })); // supera lo liberado
        *f.counter.lock().unwrap() = Some(40);
        assert_eq!(f.step(&mut st, 1900, &[Some(3), None]), None); // páginas desconocidas: no se puede comparar
        assert!(!explained_by_local(&[], 1, "impresiones"));
        assert!(explained_by_local(&[None, None], 2, "trabajos") && !explained_by_local(&[None], 2, "trabajos"));
    }

    #[test]
    fn la_ventana_de_gracia_parte_de_la_consulta_anterior() {
        let f = Fake::default();
        let mut st = Status::default();
        *f.counter.lock().unwrap() = Some(0);
        f.step(&mut st, 1000, &[]);
        *f.counter.lock().unwrap() = Some(1);
        let from = std::cell::Cell::new(0);
        let tr = |_: &str, _: u16, r: &[u8]| f.call(r);
        poll_target(&mut st, &tr, "h", 631, 1300, &|d, _, _| { from.set(d); false });
        assert_eq!(from.get(), 1000 - GRACE_SECS);
    }

    #[test]
    fn cambio_de_fuente_rebasa_sin_alerta() {
        let f = Fake::default();
        let mut st = Status::default();
        *f.counter.lock().unwrap() = Some(10);
        f.step(&mut st, 1000, &[]);
        *f.counter.lock().unwrap() = None; // deja de exponer el contador: pasa a la fuente 2
        *f.jobs.lock().unwrap() = Some(vec![(1, 9), (2, 9)]);
        assert_eq!(f.step(&mut st, 1300, &[]), None);
        assert_eq!(st.source, SRC_JOBS);
        *f.jobs.lock().unwrap() = None; // ni eso: no verificable y se pierde la base
        assert_eq!(f.step(&mut st, 1600, &[]), None);
        assert!(st.error.starts_with("no verificable") && st.source.is_empty());
        *f.counter.lock().unwrap() = Some(500);
        assert_eq!(f.step(&mut st, 1900, &[]), None); // vuelve a empezar con base nueva
        assert_eq!((st.source.as_str(), st.total, st.error.as_str()), (SRC_PRINTER, 500, ""));
    }

    #[test]
    fn contador_que_baja_no_alerta() {
        let f = Fake::default();
        let mut st = Status::default();
        *f.counter.lock().unwrap() = Some(1000);
        f.step(&mut st, 1000, &[]);
        *f.counter.lock().unwrap() = Some(3); // reinicio del contador
        assert_eq!(f.step(&mut st, 1300, &[]), None);
        assert_eq!(st.total, 3);
        *f.counter.lock().unwrap() = Some(4);
        assert_eq!(f.step(&mut st, 1600, &[]), Some(Alert { delta: 1, unit: "impresiones" })); // y sigue detectando desde la nueva base
    }

    #[test]
    fn monitor_y_texto() {
        let mut m = Monitor::default();
        assert!(!m.due(1, 30) && m.due(30, 30));
        m.last_at = 100;
        assert!(!m.due(120, 30) && m.due(130, 30));
        m.printers.insert("P".into(), Status { printer: "P".into(), ..Default::default() });
        assert_eq!(m.status().len(), 1);
        m.reset();
        assert!(m.printers.is_empty() && m.last_at == 0);
        assert_eq!(Alert { delta: 5, unit: "trabajos" }.text("Oficina"), "Impresión directa sospechosa en Oficina: +5 trabajos según la impresora sin trabajos liberados por esta app que lo expliquen");
    }
}
