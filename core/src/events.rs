//! Registro de eventos en SQLite (un archivo, `events.db`, en la carpeta de datos de la app).
//!
//! - Esquema versionado con `PRAGMA user_version`; migración idempotente desde `log.jsonl`
//!   (el archivo antiguo se renombra a `log.jsonl.migrado`, nunca se borra).
//! - Cola de envío opcional al servidor en la propia tabla (`sent`): 0 = pendiente, 1 = enviado,
//!   2 = no se envía (envío apagado al registrarlo o historial migrado), 3 = rechazado por el servidor.
//! - Este módulo no abre conexiones de red por sí solo: el envío usa un [`Transport`] inyectado
//!   y [`HttpTransport`] solo se construye cuando el usuario activó el envío.
//! - Todas las consultas usan parámetros enlazados; los únicos fragmentos de SQL variables son
//!   constantes del propio código (nunca texto del usuario).

use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Res<T> = Result<T, String>;

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

pub const SCHEMA_VERSION: i64 = 1;
/// Tamaño de lote por defecto del envío al servidor.
pub const BATCH: usize = 100;

// Límites por campo (los mismos que aplica el servidor).
pub const MAX_ID: usize = 64;
pub const MAX_SHORT: usize = 255;
pub const MAX_DOC: usize = 1000;
pub const MAX_REASON: usize = 500;

pub const SENT_PENDING: i64 = 0;
pub const SENT_OK: i64 = 1;
pub const SENT_SKIP: i64 = 2;
pub const SENT_REJECTED: i64 = 3;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// UUID v7 (ordenable por hora): 48 bits de milisegundos + aleatorios del sistema.
pub fn new_id() -> String {
    new_id_at(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

fn new_id_at(ms: u64) -> String {
    let mut r = [0u8; 10];
    getrandom::fill(&mut r).expect("sin fuente de aleatorios del sistema");
    let mut b = [0u8; 16];
    b[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
    b[6] = 0x70 | (r[0] & 0x0f);
    b[7] = r[1];
    b[8] = 0x80 | (r[2] & 0x3f);
    b[9..].copy_from_slice(&r[3..]);
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Un evento de impresión (o un aviso del sistema). Los textos vacíos significan "desconocido".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Event {
    pub id: String,
    pub ts: u64,
    /// Equipo que registró el evento.
    pub host: String,
    /// IP de origen del trabajo (vacío = impreso en este mismo equipo).
    pub ip: String,
    pub user: String,
    pub printer: String,
    /// Puerto o URI de salida (por ejemplo `socket://10.0.0.5:9100`).
    pub port: String,
    /// USB, red, WSD, archivo…
    pub conn_type: String,
    pub document: String,
    pub site: String,
    pub app: String,
    pub pages: Option<u64>,
    pub bytes: Option<u64>,
    /// `allow` | `block` | `notice`.
    pub decision: String,
    pub reason: String,
    /// Estado final: `liberado`, `cancelado`, `aviso`…
    pub status: String,
    /// `desktop` | `server`.
    pub source: String,
}

fn clip(s: &mut String, max: usize) {
    if s.chars().count() > max {
        *s = s.chars().take(max).collect();
    }
}

impl Event {
    /// Evento nuevo con id y hora actuales.
    pub fn new(source: &str, host: &str) -> Event {
        Event { id: new_id(), ts: now_secs(), host: host.into(), source: source.into(), ..Default::default() }
    }

    /// Recorta los campos a los límites comunes (cliente y servidor).
    pub fn clamp(&mut self) {
        clip(&mut self.id, MAX_ID);
        for f in [&mut self.host, &mut self.ip, &mut self.user, &mut self.printer, &mut self.port, &mut self.conn_type, &mut self.site, &mut self.app, &mut self.status, &mut self.source] {
            clip(f, MAX_SHORT);
        }
        clip(&mut self.document, MAX_DOC);
        clip(&mut self.reason, MAX_REASON);
        if !matches!(self.decision.as_str(), "allow" | "block" | "notice") {
            self.decision = "notice".into();
        }
    }
}

/// Filtros de consulta; todos opcionales.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    /// Desde (inclusive), segundos Unix.
    pub from: Option<u64>,
    /// Hasta (inclusive), segundos Unix.
    pub to: Option<u64>,
    pub user: String,
    pub printer: String,
    pub host: String,
    /// `ok` (permitidos y avisos), `bad` (bloqueados) o una decisión exacta (`allow`, `block`, `notice`).
    pub decision: String,
    /// Texto libre (sin mayúsculas) en documento, usuario, impresora, equipo, IP, sitio, aplicación y motivo.
    pub text: String,
}

fn like_escape(s: &str) -> String {
    let mut o = String::from("%");
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o.push('%');
    o
}

fn bind(p: &mut Vec<Value>, v: Value) -> String {
    p.push(v);
    format!("?{}", p.len())
}

impl Filter {
    fn where_sql(&self) -> (String, Vec<Value>) {
        let mut c: Vec<String> = vec![];
        let mut p: Vec<Value> = vec![];
        let cap = |v: u64| Value::Integer(v.min(i64::MAX as u64) as i64);
        if let Some(v) = self.from {
            c.push(format!("ts >= {}", bind(&mut p, cap(v))));
        }
        if let Some(v) = self.to {
            c.push(format!("ts <= {}", bind(&mut p, cap(v))));
        }
        for (col, v) in [("user", &self.user), ("printer", &self.printer), ("host", &self.host)] {
            if !v.is_empty() {
                c.push(format!("{col} = {}", bind(&mut p, Value::Text(v.clone()))));
            }
        }
        match self.decision.as_str() {
            "" | "all" => {}
            "ok" => c.push("decision <> 'block'".into()),
            "bad" => c.push("decision = 'block'".into()),
            d => c.push(format!("decision = {}", bind(&mut p, Value::Text(d.into())))),
        }
        let t = self.text.trim();
        if !t.is_empty() {
            let n = bind(&mut p, Value::Text(like_escape(t)));
            let cols = ["document", "user", "printer", "host", "ip", "site", "app", "reason"];
            c.push(format!("({})", cols.iter().map(|k| format!("{k} LIKE {n} ESCAPE '\\'")).collect::<Vec<_>>().join(" OR ")));
        }
        (if c.is_empty() { String::new() } else { format!(" WHERE {}", c.join(" AND ")) }, p)
    }
}

/// Totales de una agrupación de "Quién imprimió qué".
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SummaryRow {
    pub key: String,
    pub jobs: u64,
    pub allowed: u64,
    pub blocked: u64,
    /// Suma de las páginas conocidas; `None` si ningún trabajo las informó.
    pub pages: Option<u64>,
    /// Trabajos con páginas conocidas (el total es parcial si es menor que `jobs`).
    pub pages_jobs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub rows: Vec<Event>,
    pub total: u64,
}

const COLS: &str = "id, ts, host, ip, user, printer, port, conn_type, document, site, app, pages, bytes, decision, reason, status, source";

fn row_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let opt = |i: usize| -> rusqlite::Result<Option<u64>> { Ok(r.get::<_, Option<i64>>(i)?.map(|v| v.max(0) as u64)) };
    Ok(Event {
        id: r.get(0)?,
        ts: r.get::<_, i64>(1)?.max(0) as u64,
        host: r.get(2)?,
        ip: r.get(3)?,
        user: r.get(4)?,
        printer: r.get(5)?,
        port: r.get(6)?,
        conn_type: r.get(7)?,
        document: r.get(8)?,
        site: r.get(9)?,
        app: r.get(10)?,
        pages: opt(11)?,
        bytes: opt(12)?,
        decision: r.get(13)?,
        reason: r.get(14)?,
        status: r.get(15)?,
        source: r.get(16)?,
    })
}

/// Crea (si no existe) y deja el archivo accesible solo para su dueño (0600 en Unix; en Windows
/// el perfil del usuario ya restringe la carpeta de datos a ese usuario).
fn private_file(path: &Path) -> Res<()> {
    let mut o = fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path).map_err(e)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(e)?;
    }
    Ok(())
}

pub struct EventDb {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl EventDb {
    /// Abre `<dir>/events.db` (la crea), aplica migraciones de esquema e importa `log.jsonl`.
    pub fn open(dir: &Path) -> Res<EventDb> {
        fs::create_dir_all(dir).map_err(e)?;
        let db = EventDb::open_file(&dir.join("events.db"))?;
        db.import_jsonl(dir)?;
        Ok(db)
    }

    pub fn open_file(path: &Path) -> Res<EventDb> {
        private_file(path)?;
        let conn = Connection::open(path).map_err(e)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(e)?;
        // WAL: lectores y escritor no se bloquean; las líneas -wal/-shm heredan los permisos del archivo.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)).map_err(e)?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;").map_err(e)?;
        let db = EventDb { conn: Mutex::new(conn), path: path.to_path_buf() };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn migrate(&self) -> Res<()> {
        let mut c = self.conn.lock().unwrap();
        let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(e)?;
        if v > SCHEMA_VERSION {
            return Err(format!("events.db es de una versión más nueva ({v}); actualiza la aplicación"));
        }
        if v < 1 {
            let tx = c.transaction().map_err(e)?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS events (
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
                    source TEXT NOT NULL DEFAULT 'desktop',
                    sent INTEGER NOT NULL DEFAULT 2
                );
                CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
                CREATE INDEX IF NOT EXISTS idx_events_user ON events(user, ts);
                CREATE INDEX IF NOT EXISTS idx_events_printer ON events(printer, ts);
                CREATE INDEX IF NOT EXISTS idx_events_decision ON events(decision, ts);
                CREATE INDEX IF NOT EXISTS idx_events_pending ON events(ts) WHERE sent = 0;
                CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
                PRAGMA user_version = 1;",
            )
            .map_err(e)?;
            tx.commit().map_err(e)?;
        }
        Ok(())
    }

    fn meta_get(c: &Connection, k: &str) -> Res<Option<String>> {
        c.query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| r.get(0)).optional().map_err(e)
    }

    pub fn schema_version(&self) -> Res<i64> {
        self.conn.lock().unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(e)
    }

    /// Inserta un evento (idempotente por id). `queue` = ponerlo en la cola de envío.
    pub fn insert(&self, ev: &Event, queue: bool) -> Res<()> {
        let mut ev = ev.clone();
        ev.clamp();
        let c = self.conn.lock().unwrap();
        Self::insert_on(&c, &ev, if queue { SENT_PENDING } else { SENT_SKIP })
    }

    fn insert_on(c: &Connection, ev: &Event, sent: i64) -> Res<()> {
        c.prepare_cached(&format!("INSERT OR IGNORE INTO events ({COLS}, sent) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)"))
            .and_then(|mut s| {
                s.execute(params![
                    ev.id,
                    ev.ts.min(i64::MAX as u64) as i64,
                    ev.host,
                    ev.ip,
                    ev.user,
                    ev.printer,
                    ev.port,
                    ev.conn_type,
                    ev.document,
                    ev.site,
                    ev.app,
                    ev.pages.map(|v| v.min(i64::MAX as u64) as i64),
                    ev.bytes.map(|v| v.min(i64::MAX as u64) as i64),
                    ev.decision,
                    ev.reason,
                    ev.status,
                    ev.source,
                    sent
                ])
            })
            .map(|_| ())
            .map_err(e)
    }

    /// Consulta con filtros y paginación, del más reciente al más antiguo.
    pub fn query(&self, f: &Filter, limit: u32, offset: u32) -> Res<Page> {
        let (w, mut p) = f.where_sql();
        let c = self.conn.lock().unwrap();
        let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM events{w}"), params_from_iter(p.iter()), |r| r.get(0)).map_err(e)?;
        p.push(Value::Integer(limit as i64));
        p.push(Value::Integer(offset as i64));
        let (nl, no) = (p.len() - 1, p.len());
        let mut s = c.prepare(&format!("SELECT {COLS} FROM events{w} ORDER BY ts DESC, id DESC LIMIT ?{nl} OFFSET ?{no}")).map_err(e)?;
        let rows = s.query_map(params_from_iter(p.iter()), row_event).map_err(e)?.collect::<Result<Vec<_>, _>>().map_err(e)?;
        Ok(Page { rows, total: total.max(0) as u64 })
    }

    /// Agrupa los trabajos (los avisos del sistema no cuentan) por `user`, `host`, `printer` o `decision`.
    pub fn summarize(&self, f: &Filter, by: &str) -> Res<Vec<SummaryRow>> {
        let expr = match by {
            "user" => "user",
            "host" => "host",
            "printer" => "printer",
            "decision" => "decision",
            _ => return Err("agrupación desconocida".into()),
        };
        let (mut w, p) = f.where_sql();
        w.push_str(if w.is_empty() { " WHERE decision <> 'notice'" } else { " AND decision <> 'notice'" });
        let c = self.conn.lock().unwrap();
        let mut s = c
            .prepare(&format!(
                "SELECT {expr} AS k, COUNT(*), COALESCE(SUM(decision <> 'block'),0), COALESCE(SUM(decision = 'block'),0), SUM(pages), COUNT(pages) FROM events{w} GROUP BY k ORDER BY COUNT(*) DESC, k LIMIT 1000"
            ))
            .map_err(e)?;
        let rows = s
            .query_map(params_from_iter(p.iter()), |r| {
                let n = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)?.max(0) as u64) };
                Ok(SummaryRow { key: r.get(0)?, jobs: n(1)?, allowed: n(2)?, blocked: n(3)?, pages: r.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u64), pages_jobs: n(5)? })
            })
            .map_err(e)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(e)?;
        Ok(rows)
    }

    /// Impresoras distintas que aparecen en el registro (para el filtro de la interfaz).
    pub fn printers(&self) -> Res<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut s = c.prepare("SELECT DISTINCT printer FROM events WHERE printer <> '' ORDER BY printer LIMIT 500").map_err(e)?;
        let v = s.query_map([], |r| r.get(0)).map_err(e)?.collect::<Result<Vec<String>, _>>().map_err(e)?;
        Ok(v)
    }

    pub fn count_blocked_since(&self, since: u64) -> u64 {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT COUNT(*) FROM events WHERE decision = 'block' AND ts >= ?1", [since.min(i64::MAX as u64) as i64], |r| r.get::<_, i64>(0)).unwrap_or(0).max(0) as u64
    }

    pub fn count(&self) -> u64 {
        self.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0)).unwrap_or(0).max(0) as u64
    }

    /// Cuáles de los `ids` ya están en el registro.
    pub fn existing_ids(&self, ids: &[String]) -> Res<std::collections::HashSet<String>> {
        let c = self.conn.lock().unwrap();
        let mut out = std::collections::HashSet::new();
        for chunk in ids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut s = c.prepare(&format!("SELECT id FROM events WHERE id IN ({marks})")).map_err(e)?;
            for r in s.query_map(params_from_iter(chunk.iter()), |r| r.get::<_, String>(0)).map_err(e)? {
                out.insert(r.map_err(e)?);
            }
        }
        Ok(out)
    }

    /// Importa eventos ya validados con sus ids (idempotente: los existentes no se repiten) en una sola
    /// transacción y sin ponerlos en la cola de envío; con `replace` borra antes el registro actual.
    pub fn import_events(&self, evs: &[Event], replace: bool) -> Res<u64> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction().map_err(e)?;
        if replace {
            tx.execute("DELETE FROM events", []).map_err(e)?;
        }
        let before: i64 = tx.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).map_err(e)?;
        for ev in evs {
            let mut ev = ev.clone();
            ev.clamp();
            Self::insert_on(&tx, &ev, SENT_SKIP)?;
        }
        let after: i64 = tx.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).map_err(e)?;
        tx.commit().map_err(e)?;
        Ok((after - before).max(0) as u64)
    }

    /// Borra los eventos más viejos que `days` días (0 = sin límite). Devuelve cuántos.
    pub fn purge(&self, days: u64) -> Res<u64> {
        self.purge_before(if days == 0 { 0 } else { now_secs().saturating_sub(days.saturating_mul(86400)) })
    }

    pub fn purge_before(&self, ts: u64) -> Res<u64> {
        if ts == 0 {
            return Ok(0);
        }
        let c = self.conn.lock().unwrap();
        c.execute("DELETE FROM events WHERE ts < ?1", [ts.min(i64::MAX as u64) as i64]).map(|n| n as u64).map_err(e)
    }

    /// Borra todo el registro (acción explícita del usuario).
    pub fn clear(&self) -> Res<()> {
        self.conn.lock().unwrap().execute("DELETE FROM events", []).map(|_| ()).map_err(e)
    }

    /// CSV con los eventos que cumplen el filtro (más recientes primero). Neutraliza fórmulas de hoja de cálculo.
    pub fn export_csv(&self, f: &Filter) -> Res<String> {
        let (w, p) = f.where_sql();
        let c = self.conn.lock().unwrap();
        let mut s = c.prepare(&format!("SELECT {COLS} FROM events{w} ORDER BY ts DESC, id DESC")).map_err(e)?;
        let mut out = String::from("hora_unix,estado,documento,impresora,usuario,origen,motivo,equipo,puerto,tipo_conexion,sitio,aplicacion,paginas,bytes,decision,estado_final,origen_evento,id\n");
        let it = s.query_map(params_from_iter(p.iter()), row_event).map_err(e)?;
        for ev in it {
            let ev = ev.map_err(e)?;
            let num = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_default();
            out += &format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                ev.ts,
                if ev.decision == "block" { "bloqueado" } else { "permitido" },
                csv_cell(&ev.document),
                csv_cell(&ev.printer),
                csv_cell(&ev.user),
                csv_cell(&ev.ip),
                csv_cell(&ev.reason),
                csv_cell(&ev.host),
                csv_cell(&ev.port),
                csv_cell(&ev.conn_type),
                csv_cell(&ev.site),
                csv_cell(&ev.app),
                num(ev.pages),
                num(ev.bytes),
                ev.decision,
                csv_cell(&ev.status),
                ev.source,
                ev.id
            );
        }
        Ok(out)
    }

    // --- cola de envío ---

    /// Eventos pendientes de enviar, los más antiguos primero.
    pub fn pending(&self, limit: usize) -> Res<Vec<Event>> {
        let c = self.conn.lock().unwrap();
        let mut s = c.prepare(&format!("SELECT {COLS} FROM events WHERE sent = 0 ORDER BY ts, id LIMIT ?1")).map_err(e)?;
        let v = s.query_map([limit as i64], row_event).map_err(e)?.collect::<Result<Vec<_>, _>>().map_err(e)?;
        Ok(v)
    }

    pub fn pending_count(&self) -> u64 {
        self.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM events WHERE sent = 0", [], |r| r.get::<_, i64>(0)).unwrap_or(0).max(0) as u64
    }

    /// Cambia el estado de envío solo de los ids dados que sigan pendientes.
    pub fn mark(&self, ids: &[String], state: i64) -> Res<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction().map_err(e)?;
        let mut n = 0;
        {
            let mut s = tx.prepare("UPDATE events SET sent = ?1 WHERE id = ?2 AND sent = 0").map_err(e)?;
            for id in ids {
                n += s.execute(params![state, id]).map_err(e)?;
            }
        }
        tx.commit().map_err(e)?;
        Ok(n)
    }

    // --- migración desde log.jsonl ---

    /// Renombra `log.jsonl` a `log.jsonl.migrado` (si existe) e importa todo `log.jsonl.migrado*`
    /// que aún no se haya importado. Idempotente aunque el proceso se interrumpa a medias.
    fn import_jsonl(&self, dir: &Path) -> Res<()> {
        let src = dir.join("log.jsonl");
        if src.is_file() {
            let mut dst = dir.join("log.jsonl.migrado");
            if dst.exists() {
                dst = dir.join(format!("log.jsonl.migrado-{}", now_secs()));
                let mut n = 0;
                while dst.exists() {
                    n += 1;
                    dst = dir.join(format!("log.jsonl.migrado-{}-{n}", now_secs()));
                }
            }
            fs::rename(&src, &dst).map_err(e)?;
        }
        let mut names: Vec<String> = fs::read_dir(dir)
            .map_err(e)?
            .filter_map(|x| x.ok())
            .filter_map(|x| x.file_name().into_string().ok())
            .filter(|n| n.starts_with("log.jsonl.migrado"))
            .collect();
        names.sort();
        for n in names {
            self.import_file(&dir.join(&n), &n)?;
        }
        Ok(())
    }

    fn import_file(&self, path: &Path, name: &str) -> Res<()> {
        #[derive(Deserialize)]
        struct Legacy {
            time: u64,
            #[serde(default)]
            printer: String,
            #[serde(default)]
            document: String,
            user: Option<String>,
            origin: Option<String>,
            #[serde(default)]
            allowed: bool,
            #[serde(default)]
            reason: String,
        }
        let key = format!("imported:{name}");
        let mut c = self.conn.lock().unwrap();
        if Self::meta_get(&c, &key)?.is_some() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&fs::read(path).map_err(e)?).into_owned();
        let tx = c.transaction().map_err(e)?;
        let mut n = 0u64;
        for l in text.lines() {
            let Ok(a) = serde_json::from_str::<Legacy>(l) else { continue };
            let notice = a.reason == "Aviso del sistema";
            let mut ev = Event {
                id: new_id_at(a.time.saturating_mul(1000)),
                ts: a.time,
                ip: a.origin.unwrap_or_default(),
                user: a.user.unwrap_or_default(),
                printer: a.printer,
                document: a.document,
                decision: if notice { "notice" } else if a.allowed { "allow" } else { "block" }.into(),
                reason: a.reason,
                status: if notice { "aviso" } else if a.allowed { "liberado" } else { "cancelado" }.into(),
                source: "desktop".into(),
                ..Default::default()
            };
            ev.clamp();
            Self::insert_on(&tx, &ev, SENT_SKIP)?;
            n += 1;
        }
        tx.execute("INSERT OR REPLACE INTO meta (k, v) VALUES (?1, ?2)", params![key, n.to_string()]).map_err(e)?;
        tx.commit().map_err(e)
    }
}

pub fn csv_cell(v: &str) -> String {
    // Una celda que empieza por = + - @ (o tabulación/CR) se interpretaría como fórmula en Excel/Calc.
    let prefix = if v.starts_with(['=', '+', '-', '@', '\t', '\r']) { "'" } else { "" };
    format!("\"{prefix}{}\"", v.replace('"', "\"\""))
}

// --- envío al servidor ---

#[derive(Debug, Clone, PartialEq)]
pub enum SendError {
    /// Red caída, 5xx, respuesta ilegible…: reintentar con espera creciente.
    Retry(String),
    /// 429: reintentar no antes de N segundos.
    RetryAfter(String, u64),
    /// 401/403: token inválido o revocado; se reintenta con espera larga por si el administrador lo corrige.
    Auth(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Retry(m) | SendError::RetryAfter(m, _) | SendError::Auth(m) => f.write_str(m),
        }
    }
}

/// Respuesta del servidor a un lote.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Ack {
    /// Ids guardados (incluye los que ya tenía: es idempotente).
    pub accepted: Vec<String>,
    /// Ids que el servidor rechazó por inválidos (no tiene sentido reenviarlos).
    pub rejected: Vec<String>,
}

pub trait Transport {
    fn send(&self, events: &[Event]) -> Result<Ack, SendError>;
}

/// Envía todo lo pendiente en lotes. Marca como enviados solo los ids que el servidor aceptó.
/// Devuelve cuántos eventos quedaron enviados.
pub fn flush(db: &EventDb, t: &dyn Transport, batch: usize) -> Result<usize, SendError> {
    let mut sent = 0;
    loop {
        let evs = db.pending(batch.max(1)).map_err(SendError::Retry)?;
        if evs.is_empty() {
            return Ok(sent);
        }
        let ack = t.send(&evs)?;
        let in_batch = |id: &&String| evs.iter().any(|x| &x.id == *id);
        let ok: Vec<String> = ack.accepted.iter().filter(in_batch).cloned().collect();
        let bad: Vec<String> = ack.rejected.iter().filter(in_batch).cloned().collect();
        if ok.is_empty() && bad.is_empty() {
            return Err(SendError::Retry("el servidor no aceptó ningún evento del lote".into()));
        }
        db.mark(&ok, SENT_OK).map_err(SendError::Retry)?;
        db.mark(&bad, SENT_REJECTED).map_err(SendError::Retry)?;
        sent += ok.len();
    }
}

/// Espera tras `failures` fallos seguidos: 5 s, 10 s, 20 s… hasta 5 min.
pub fn backoff_secs(failures: u32) -> u64 {
    if failures == 0 {
        return 0;
    }
    (5u64 << (failures - 1).min(6)).min(300)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncStatus {
    pub pending: u64,
    pub last_ok: Option<u64>,
    pub last_error: Option<String>,
}

/// Estado del hilo de envío (reintentos con espera creciente).
#[derive(Default)]
pub struct Syncer {
    failures: u32,
    next_at: u64,
    pub last_ok: Option<u64>,
    pub last_error: Option<String>,
}

impl Syncer {
    /// Olvida los fallos (p. ej. al cambiar la URL o el token).
    pub fn reset(&mut self) {
        self.failures = 0;
        self.next_at = 0;
        self.last_error = None;
    }

    /// Un paso del hilo de envío. `make` crea el transporte (y con él la conexión) solo si el envío
    /// está activado, hay eventos pendientes y toca reintentar: apagado, no se abre ninguna conexión.
    pub fn tick(&mut self, now: u64, enabled: bool, db: &EventDb, make: &dyn Fn() -> Result<Box<dyn Transport>, String>) {
        if !enabled || now < self.next_at || db.pending_count() == 0 {
            return;
        }
        let t = match make() {
            Ok(t) => t,
            Err(m) => return self.fail(now, SendError::Auth(m)),
        };
        match flush(db, t.as_ref(), BATCH) {
            Ok(_) => {
                self.failures = 0;
                self.next_at = now;
                self.last_ok = Some(now);
                self.last_error = None;
            }
            Err(er) => self.fail(now, er),
        }
    }

    fn fail(&mut self, now: u64, er: SendError) {
        self.failures = self.failures.saturating_add(1);
        let mut wait = backoff_secs(self.failures);
        if let SendError::RetryAfter(_, s) = &er {
            wait = wait.max((*s).min(3600));
        }
        if matches!(er, SendError::Auth(_)) {
            wait = wait.max(60);
        }
        self.next_at = now + wait;
        self.last_error = Some(er.to_string());
    }

    pub fn status(&self, db: &EventDb) -> SyncStatus {
        SyncStatus { pending: db.pending_count(), last_ok: self.last_ok, last_error: self.last_error.clone() }
    }
}

/// Transporte HTTP(S) con `Authorization: Bearer`. Solo se construye con el envío activado.
pub struct HttpTransport {
    agent: ureq::Agent,
    url: String,
    token: String,
}

impl HttpTransport {
    pub fn new(base_url: &str, token: &str) -> Res<HttpTransport> {
        let base = base_url.trim().trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) || base.len() < 9 {
            return Err("La dirección del servidor debe empezar por http:// o https://".into());
        }
        if token.trim().is_empty() {
            return Err("Falta el token de equipo".into());
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(8)))
            .http_status_as_error(false)
            // El token no debe seguir una redirección a otro sitio.
            .max_redirects(0)
            .build()
            .into();
        Ok(HttpTransport { agent, url: format!("{base}/api/events/ingest"), token: token.trim().to_string() })
    }
}

impl Transport for HttpTransport {
    fn send(&self, events: &[Event]) -> Result<Ack, SendError> {
        #[derive(Serialize)]
        struct Body<'a> {
            events: &'a [Event],
        }
        let body = serde_json::to_vec(&Body { events }).map_err(|x| SendError::Retry(x.to_string()))?;
        let mut resp = self
            .agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .send(&body[..])
            .map_err(|x| SendError::Retry(format!("sin conexión con el servidor: {x}")))?;
        let code = resp.status().as_u16();
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(60);
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        match code {
            200 => serde_json::from_str::<Ack>(&text).map_err(|x| SendError::Retry(format!("respuesta ilegible del servidor: {x}"))),
            401 | 403 => Err(SendError::Auth("Token de equipo inválido o revocado".into())),
            429 => Err(SendError::RetryAfter("El servidor pide esperar (demasiadas peticiones)".into(), retry_after)),
            c => Err(SendError::Retry(format!("el servidor respondió {c}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn tmp(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("work").join(format!("test-events-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn ev(doc: &str, user: &str, decision: &str, ts: u64) -> Event {
        Event { id: new_id(), ts, host: "pc1".into(), user: user.into(), printer: "Oficina".into(), document: doc.into(), decision: decision.into(), source: "desktop".into(), ..Default::default() }
    }

    #[test]
    fn id_es_uuid_v7_unico() {
        let a = new_id();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "7");
        assert_ne!(a, new_id());
    }

    #[test]
    fn inserta_consulta_filtra_y_pagina() {
        let d = tmp("query");
        let db = EventDb::open(&d).unwrap();
        for i in 0..25 {
            let mut x = ev(&format!("doc{i}.pdf"), if i % 2 == 0 { "ana" } else { "luis" }, if i % 5 == 0 { "block" } else { "allow" }, 1000 + i);
            if i == 3 {
                x.document = "100%_real.docx".into();
            }
            db.insert(&x, false).unwrap();
        }
        let p = db.query(&Filter::default(), 10, 0).unwrap();
        assert_eq!((p.rows.len(), p.total), (10, 25));
        assert_eq!(p.rows[0].document, "doc24.pdf", "más reciente primero");
        assert_eq!(db.query(&Filter::default(), 10, 20).unwrap().rows.len(), 5);
        assert_eq!(db.query(&Filter { user: "ana".into(), ..Default::default() }, 100, 0).unwrap().total, 13);
        assert_eq!(db.query(&Filter { decision: "bad".into(), ..Default::default() }, 100, 0).unwrap().total, 5);
        assert_eq!(db.query(&Filter { decision: "ok".into(), ..Default::default() }, 100, 0).unwrap().total, 20);
        assert_eq!(db.query(&Filter { from: Some(1010), to: Some(1012), ..Default::default() }, 100, 0).unwrap().total, 3);
        assert_eq!(db.query(&Filter { printer: "Nada".into(), ..Default::default() }, 100, 0).unwrap().total, 0);
        // Los comodines de LIKE del usuario son literales; y no hay inyección.
        assert_eq!(db.query(&Filter { text: "100%_".into(), ..Default::default() }, 100, 0).unwrap().total, 1);
        assert_eq!(db.query(&Filter { text: "%".into(), ..Default::default() }, 100, 0).unwrap().total, 1);
        assert_eq!(db.query(&Filter { text: "x' OR '1'='1".into(), ..Default::default() }, 100, 0).unwrap().total, 0);
        assert_eq!(db.query(&Filter { text: "DOC2".into(), user: "ana".into(), decision: "ok".into(), ..Default::default() }, 100, 0).unwrap().total, 3); // doc2, doc22, doc24
        assert_eq!(db.count_blocked_since(1020), 1); // i=20
        assert_eq!(db.printers().unwrap(), vec!["Oficina"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn insercion_idempotente_y_recorte() {
        let d = tmp("idem");
        let db = EventDb::open(&d).unwrap();
        let mut x = ev("a.pdf", "ana", "allow", 5);
        db.insert(&x, true).unwrap();
        x.document = "otro".into();
        db.insert(&x, true).unwrap();
        assert_eq!(db.count(), 1);
        assert_eq!(db.query(&Filter::default(), 5, 0).unwrap().rows[0].document, "a.pdf");
        let mut big = ev(&"x".repeat(5000), "ana", "raro", 6);
        big.reason = "r".repeat(2000);
        db.insert(&big, false).unwrap();
        let r = &db.query(&Filter::default(), 1, 0).unwrap().rows[0];
        assert_eq!((r.document.chars().count(), r.reason.chars().count(), r.decision.as_str()), (MAX_DOC, MAX_REASON, "notice"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn esquema_versionado_y_reabrir() {
        let d = tmp("schema");
        {
            let db = EventDb::open(&d).unwrap();
            assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
            db.insert(&ev("a", "u", "allow", 1), false).unwrap();
        }
        let db = EventDb::open(&d).unwrap();
        assert_eq!(db.count(), 1);
        // Base de una versión futura: no se toca.
        db.conn.lock().unwrap().execute_batch("PRAGMA user_version = 99").unwrap();
        drop(db);
        assert!(EventDb::open(&d).err().unwrap().contains("más nueva"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn migra_log_jsonl_una_sola_vez_y_conserva_el_respaldo() {
        let d = tmp("mig");
        let lines = [
            r#"{"time":100,"printer":"Oficina","document":"a.pdf","user":"ana","origin":"10.0.0.5","allowed":true,"reason":"Sin coincidencia"}"#,
            r#"{"time":200,"printer":"Oficina","document":"b.docx","user":null,"origin":null,"allowed":false,"reason":"Aplicación: *.docx"}"#,
            "esto no es json",
            r#"{"time":300,"printer":"—","document":"Config dañada","user":null,"origin":null,"allowed":true,"reason":"Aviso del sistema"}"#,
        ];
        fs::write(d.join("log.jsonl"), lines.join("\n") + "\n").unwrap();
        let db = EventDb::open(&d).unwrap();
        assert_eq!(db.count(), 3);
        assert!(!d.join("log.jsonl").exists());
        assert_eq!(fs::read_to_string(d.join("log.jsonl.migrado")).unwrap(), lines.join("\n") + "\n", "el respaldo queda intacto");
        let p = db.query(&Filter::default(), 10, 0).unwrap();
        assert_eq!(p.rows[0].decision, "notice");
        assert_eq!((p.rows[1].decision.as_str(), p.rows[1].user.as_str(), p.rows[1].status.as_str()), ("block", "", "cancelado"));
        assert_eq!((p.rows[2].ip.as_str(), p.rows[2].user.as_str()), ("10.0.0.5", "ana"));
        assert_eq!(db.pending_count(), 0, "el historial migrado no se envía");
        // Reabrir no duplica.
        drop(db);
        let db = EventDb::open(&d).unwrap();
        assert_eq!(db.count(), 3);
        // Un log.jsonl nuevo (p. ej. de una versión anterior) se importa aparte, sin perder el primer respaldo.
        fs::write(d.join("log.jsonl"), lines[0].to_string() + "\n").unwrap();
        drop(db);
        let db = EventDb::open(&d).unwrap();
        assert_eq!(db.count(), 4);
        assert!(d.join("log.jsonl.migrado").exists());
        assert_eq!(fs::read_dir(&d).unwrap().filter(|x| x.as_ref().unwrap().file_name().to_string_lossy().starts_with("log.jsonl.migrado")).count(), 2);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn migracion_interrumpida_se_completa_sin_duplicar() {
        // Simula un corte tras renombrar y antes de importar.
        let d = tmp("mig2");
        fs::write(d.join("log.jsonl.migrado"), r#"{"time":1,"printer":"P","document":"d","user":null,"origin":null,"allowed":true,"reason":"r"}"#.to_string() + "\n").unwrap();
        let db = EventDb::open(&d).unwrap();
        assert_eq!(db.count(), 1);
        drop(db);
        assert_eq!(EventDb::open(&d).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn retencion_borra_lo_vencido() {
        let d = tmp("ret");
        let db = EventDb::open(&d).unwrap();
        let n = now_secs();
        db.insert(&ev("viejo", "u", "allow", n - 100 * 86400), false).unwrap();
        db.insert(&ev("medio", "u", "allow", n - 40 * 86400), false).unwrap();
        db.insert(&ev("nuevo", "u", "allow", n - 60), false).unwrap();
        assert_eq!(db.purge(0).unwrap(), 0, "0 = sin límite");
        assert_eq!(db.count(), 3);
        assert_eq!(db.purge(90).unwrap(), 1);
        assert_eq!(db.purge(30).unwrap(), 1);
        assert_eq!(db.query(&Filter::default(), 10, 0).unwrap().rows[0].document, "nuevo");
        db.clear().unwrap();
        assert_eq!(db.count(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn archivo_con_permisos_0600() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("perm");
        // Un events.db preexistente con permisos abiertos se corrige.
        fs::write(d.join("events.db"), b"").unwrap();
        fs::set_permissions(d.join("events.db"), fs::Permissions::from_mode(0o644)).unwrap();
        let db = EventDb::open(&d).unwrap();
        db.insert(&ev("a", "u", "allow", 1), true).unwrap();
        for f in ["events.db", "events.db-wal", "events.db-shm"] {
            let p = d.join(f);
            if p.exists() {
                assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
            }
        }
        assert!(d.join("events.db-wal").exists(), "WAL activo");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn csv_exporta_con_filtro_y_neutraliza_formulas() {
        let d = tmp("csv");
        let db = EventDb::open(&d).unwrap();
        db.insert(&ev("=HYPERLINK(\"x\")", "ana", "block", 10), false).unwrap();
        db.insert(&ev("normal, con \"comillas\"", "luis", "allow", 11), false).unwrap();
        let all = db.export_csv(&Filter::default()).unwrap();
        assert!(all.starts_with("hora_unix,estado,documento,impresora,usuario,origen,motivo,"));
        assert_eq!(all.lines().count(), 3);
        assert!(all.contains("\"'=HYPERLINK(\"\"x\"\")\""));
        assert!(all.contains("\"normal, con \"\"comillas\"\"\""));
        let only = db.export_csv(&Filter { user: "ana".into(), ..Default::default() }).unwrap();
        assert_eq!(only.lines().count(), 2);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn resumen_quien_imprimio_que() {
        let d = tmp("resumen");
        let db = EventDb::open(&d).unwrap();
        let mut a = ev("a", "ana", "allow", 1);
        a.pages = Some(3);
        let b = ev("b", "ana", "block", 2);
        let mut c = ev("c", "ana", "allow", 3);
        c.pages = Some(2);
        let l = ev("d", "luis", "allow", 4);
        let aviso = ev("sistema", "", "notice", 5);
        for e in [&a, &b, &c, &l, &aviso] {
            db.insert(e, false).unwrap();
        }
        let r = db.summarize(&Filter::default(), "user").unwrap();
        assert_eq!(r.len(), 2, "el aviso del sistema no cuenta");
        assert_eq!((r[0].key.as_str(), r[0].jobs, r[0].allowed, r[0].blocked, r[0].pages, r[0].pages_jobs), ("ana", 3, 2, 1, Some(5), 2));
        assert_eq!((r[1].key.as_str(), r[1].pages, r[1].pages_jobs), ("luis", None, 0), "sin páginas informadas no se inventan");
        assert_eq!(db.summarize(&Filter::default(), "decision").unwrap()[0].key, "allow");
        assert_eq!(db.summarize(&Filter { user: "luis".into(), ..Default::default() }, "printer").unwrap().len(), 1);
        assert!(db.summarize(&Filter::default(), "id; DROP TABLE events").is_err());
        assert_eq!(db.count(), 5);
        let _ = fs::remove_dir_all(&d);
    }

    /// Transporte falso: acepta todo o falla, y recuerda lo recibido.
    struct Fake {
        calls: Cell<u32>,
        fail: Cell<Option<SendError>>,
        got: RefCell<Vec<String>>,
        /// Acepta solo estos ids (el resto queda sin responder); None = todos.
        only: RefCell<Option<Vec<String>>>,
        reject: RefCell<Vec<String>>,
    }
    impl Fake {
        fn new() -> Fake {
            Fake { calls: Cell::new(0), fail: Cell::new(None), got: RefCell::new(vec![]), only: RefCell::new(None), reject: RefCell::new(vec![]) }
        }
    }
    impl Transport for &Fake {
        fn send(&self, evs: &[Event]) -> Result<Ack, SendError> {
            self.calls.set(self.calls.get() + 1);
            if let Some(er) = self.fail.take() {
                return Err(er);
            }
            let rej = self.reject.borrow().clone();
            let mut acc = vec![];
            for x in evs {
                if rej.contains(&x.id) {
                    continue;
                }
                if self.only.borrow().as_ref().map(|o| o.contains(&x.id)).unwrap_or(true) {
                    self.got.borrow_mut().push(x.id.clone());
                    acc.push(x.id.clone());
                }
            }
            Ok(Ack { accepted: acc, rejected: rej })
        }
    }

    #[test]
    fn cola_offline_lotes_y_marcado_solo_de_aceptados() {
        let d = tmp("cola");
        let db = EventDb::open(&d).unwrap();
        let ids: Vec<String> = (0..250)
            .map(|i| {
                let x = ev("d", "u", "allow", 1000 + i);
                db.insert(&x, true).unwrap();
                x.id
            })
            .collect();
        db.insert(&ev("no se envía", "u", "allow", 5), false).unwrap();
        assert_eq!(db.pending_count(), 250);

        // Servidor caído: nada se marca y todo se conserva.
        let f = Fake::new();
        f.fail.set(Some(SendError::Retry("caído".into())));
        assert!(flush(&db, &&f, 100).is_err());
        assert_eq!(db.pending_count(), 250);

        // Solo acepta algunos: solo esos se marcan.
        *f.only.borrow_mut() = Some(ids[..5].to_vec());
        assert!(flush(&db, &&f, 100).is_err(), "el resto no se aceptó: error y reintento");
        assert_eq!(db.pending_count(), 245);
        assert_eq!(f.got.borrow().len(), 5);

        // Rechazados permanentes salen de la cola sin bloquearla.
        *f.only.borrow_mut() = None;
        *f.reject.borrow_mut() = vec![ids[5].clone()];
        f.calls.set(0);
        assert_eq!(flush(&db, &&f, 100).unwrap(), 244);
        assert_eq!(f.calls.get(), 3, "lotes de 100");
        assert_eq!(db.pending_count(), 0);
        let conn = db.conn.lock().unwrap();
        let rej: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE sent = 3", [], |r| r.get(0)).unwrap();
        assert_eq!(rej, 1);
        drop(conn);

        // Nada pendiente: no se llama al transporte.
        f.calls.set(0);
        assert_eq!(flush(&db, &&f, 100).unwrap(), 0);
        assert_eq!(f.calls.get(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn ids_ajenos_del_servidor_se_ignoran() {
        struct Liar;
        impl Transport for Liar {
            fn send(&self, _: &[Event]) -> Result<Ack, SendError> {
                Ok(Ack { accepted: vec!["otro-id".into()], rejected: vec![] })
            }
        }
        let d = tmp("liar");
        let db = EventDb::open(&d).unwrap();
        db.insert(&ev("a", "u", "allow", 1), true).unwrap();
        assert!(flush(&db, &Liar, 100).is_err());
        assert_eq!(db.pending_count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn syncer_backoff_reintenta_y_se_recupera() {
        let d = tmp("sync");
        let db = EventDb::open(&d).unwrap();
        db.insert(&ev("a", "u", "allow", 1), true).unwrap();
        let mut s = Syncer::default();
        // El transporte debe ser 'static: se filtra el falso (solo en la prueba).
        let fp: &'static Fake = Box::leak(Box::new(Fake::new()));
        let make = || -> Result<Box<dyn Transport>, String> { Ok(Box::new(fp)) };
        fp.fail.set(Some(SendError::Retry("caído".into())));
        s.tick(100, true, &db, &make);
        assert_eq!(fp.calls.get(), 1);
        assert_eq!(s.last_error.as_deref(), Some("caído"));
        s.tick(102, true, &db, &make); // dentro de la espera de 5 s
        assert_eq!(fp.calls.get(), 1);
        s.tick(105, true, &db, &make);
        assert_eq!(fp.calls.get(), 2);
        assert_eq!(db.pending_count(), 0);
        assert_eq!((s.last_ok, s.last_error.clone()), (Some(105), None));
        assert_eq!([backoff_secs(1), backoff_secs(2), backoff_secs(3), backoff_secs(20)], [5, 10, 20, 300]);
        // 429 respeta Retry-After.
        db.insert(&ev("b", "u", "allow", 2), true).unwrap();
        fp.fail.set(Some(SendError::RetryAfter("lento".into(), 120)));
        s.tick(200, true, &db, &make);
        s.tick(300, true, &db, &make);
        assert_eq!(fp.calls.get(), 3);
        s.tick(321, true, &db, &make);
        assert_eq!(fp.calls.get(), 4);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sin_servidor_configurado_no_se_crea_transporte() {
        let d = tmp("nored");
        let db = EventDb::open(&d).unwrap();
        // Envío apagado: los eventos no entran en la cola y el transporte nunca se construye.
        db.insert(&ev("a", "u", "allow", 1), false).unwrap();
        let made = Cell::new(0);
        let make = || -> Result<Box<dyn Transport>, String> {
            made.set(made.get() + 1);
            Err("no debe llamarse".into())
        };
        let mut s = Syncer::default();
        s.tick(10, false, &db, &make);
        s.tick(11, true, &db, &make); // activado pero sin nada pendiente
        assert_eq!(made.get(), 0);
        // Con la cola llena y el envío apagado tampoco.
        db.insert(&ev("b", "u", "allow", 2), true).unwrap();
        s.tick(12, false, &db, &make);
        assert_eq!(made.get(), 0);
        // URL o token inválidos: ni siquiera se crea el cliente HTTP.
        assert!(HttpTransport::new("", "t").is_err());
        assert!(HttpTransport::new("ftp://x", "t").is_err());
        assert!(HttpTransport::new("http://127.0.0.1:1", "").is_err());
        // Activado con un servidor inexistente: falla limpio y reintenta con espera.
        let make = || -> Result<Box<dyn Transport>, String> { Ok(Box::new(HttpTransport::new("http://127.0.0.1:9", "sgm_x")?)) };
        s.tick(13, true, &db, &make);
        assert!(s.last_error.is_some());
        assert_eq!(db.pending_count(), 1);
        let _ = fs::remove_dir_all(&d);
    }
}
