//! Copia de seguridad `.spoolguard.json`: formato, validación y plan de importación.
//! Lógica pura (sin disco ni red), idéntica a `internal/app/backup.go`; los vectores
//! `tests/vectors/backup.json` validan que ambas implementaciones no diverjan.
//!
//! Lo que se exporta sale SIEMPRE de un [`Snapshot`] (lista blanca de campos): la contraseña, su
//! hash y sal, los tokens y la sesión no existen en ese tipo, así que no pueden filtrarse.
use crate::events::Event;
use crate::rules::{host_of, parse_ip, Mode, PrinterRules};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};

pub const FORMAT: &str = "spoolguard-backup";
pub const VERSION: i64 = 1;
pub const MAX_BYTES: usize = 10 << 20;
pub const MAX_DEPTH: usize = 32;
pub const MAX_EVENTS: usize = 50_000;
pub const DEFAULT_EVENTS: usize = 10_000;
const MAX_PRINTERS: usize = 500;
const MAX_LIST: usize = 2000;
const MAX_BLOCKS: usize = 1000;
const MAX_NOTES: usize = 50;
pub const CATS: [&str; 5] = ["reglas", "parametros", "bloqueo_rapido", "registro", "inventario"];

/// Ajustes que viajan en la copia (el resto son del equipo: contraseña, tokens, envío al servidor…).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    pub unknown_policy: String,
    pub interval_secs: u64,
    pub notify_block: bool,
    pub notify_allow: bool,
    pub theme: String,
    pub retention_days: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params { unknown_policy: "allow".into(), interval_secs: 1, notify_block: true, notify_allow: false, theme: "system".into(), retention_days: 90 }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct PartialParams {
    unknown_policy: Option<String>,
    interval_secs: Option<u64>,
    notify_block: Option<bool>,
    notify_allow: Option<bool>,
    theme: Option<String>,
    retention_days: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickBlock {
    pub host: String,
    pub ips: Vec<String>,
    pub by: String,
    pub time: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickMark {
    pub by: String,
    pub time: u64,
}

/// Bloqueo rápido del servidor (quick.json).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quick {
    pub blocks: Vec<QuickBlock>,
    pub paused_printers: BTreeMap<String, QuickMark>,
}

/// Estado exportable/importable del equipo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub printers: BTreeMap<String, PrinterRules>,
    pub global: PrinterRules,
    pub params: Params,
    pub quick: Quick,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InvItem {
    pub name: String,
    pub kind: String,
    pub uri: String,
    // Detalle opcional: solo se exporta si se conoce; al importar el inventario es informativo y se ignora.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub driver: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub conn_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_supported: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controlled: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Meta {
    pub created: i64,
    pub host: String,
    pub app_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Omitir,
    Combinar,
    Reemplazar,
}

#[derive(Debug, Clone, Copy)]
pub struct Modes {
    pub reglas: Op,
    pub parametros: Op,
    pub bloqueo_rapido: Op,
    pub registro: Op,
}

fn op_of(s: &str) -> Result<Op, String> {
    match s {
        "combinar" => Ok(Op::Combinar),
        "reemplazar" => Ok(Op::Reemplazar),
        "omitir" => Ok(Op::Omitir),
        x => Err(format!("Modo desconocido: {}", clip(x, 40))),
    }
}

impl Modes {
    /// `"combinar"`/`"reemplazar"` para todo, u objeto `{categoria: modo}` (lo que falte se omite).
    pub fn parse(v: &Value) -> Result<Modes, String> {
        match v {
            Value::Null => Ok(Modes::all(Op::Combinar)),
            Value::String(s) => Ok(Modes::all(op_of(s)?)),
            Value::Object(m) => {
                let g = |k: &str| -> Result<Op, String> {
                    match m.get(k) {
                        None | Some(Value::Null) => Ok(Op::Omitir),
                        Some(Value::String(s)) => op_of(s),
                        _ => Err("Modo inválido".into()),
                    }
                };
                Ok(Modes { reglas: g("reglas")?, parametros: g("parametros")?, bloqueo_rapido: g("bloqueo_rapido")?, registro: g("registro")? })
            }
            _ => Err("Modo inválido".into()),
        }
    }
    pub fn all(o: Op) -> Modes {
        Modes { reglas: o, parametros: o, bloqueo_rapido: o, registro: o }
    }
}

fn op_name(o: Op) -> &'static str {
    match o {
        Op::Omitir => "omitir",
        Op::Combinar => "combinar",
        Op::Reemplazar => "reemplazar",
    }
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[derive(Default)]
struct Notes {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Notes {
    fn err(&mut self, s: String) {
        if self.errors.len() < MAX_NOTES {
            self.errors.push(s);
        }
    }
    fn warn(&mut self, s: String) {
        if self.warnings.len() < MAX_NOTES {
            self.warnings.push(s);
        }
    }
    /// Claves que la copia no usa. Las sensibles se nombran como tales; nunca se aplican.
    fn unknown(&mut self, path: &str, m: &Map<String, Value>, known: &[&str]) {
        for k in m.keys().filter(|k| !known.contains(&k.as_str())) {
            let kk = clip(k, 60);
            let l = k.to_lowercase();
            if ["__proto__", "constructor", "prototype"].contains(&l.as_str()) {
                self.warn(format!("{path}: clave peligrosa ignorada: {kk}"));
            } else if ["password", "passwd", "hash", "salt", "token", "secret", "session", "cookie", "credential", "apikey", "api_key"].iter().any(|w| l.contains(w)) {
                self.warn(format!("{path}: campo sensible ignorado por seguridad: {kk}"));
            } else {
                self.warn(format!("{path}: campo desconocido ignorado: {kk}"));
            }
        }
    }
}

/// Profundidad máxima de anidamiento (ignora el contenido de las cadenas).
pub fn depth_ok(b: &[u8], max: usize) -> bool {
    let (mut d, mut in_s, mut esc) = (0usize, false, false);
    for &c in b {
        if in_s {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_s = false;
            }
            continue;
        }
        match c {
            b'"' => in_s = true,
            b'[' | b'{' => {
                d += 1;
                if d > max {
                    return false;
                }
            }
            b']' | b'}' => d = d.saturating_sub(1),
            _ => {}
        }
    }
    true
}

fn bad_text(s: &str, max: usize) -> bool {
    s.chars().count() > max || s.chars().any(|c| c.is_control())
}

fn host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || (c as u32 >= 128 && !c.is_whitespace() && !c.is_control())
}

/// Valida un patrón con las mismas reglas que usa el motor (`rules.rs`).
fn check_app(p: &str) -> Result<(), &'static str> {
    let t = p.trim();
    if t.is_empty() {
        return Err("patrón vacío");
    }
    if bad_text(p, 255) {
        return Err("demasiado largo o con caracteres de control");
    }
    if let Some(ext) = t.strip_prefix("*.") {
        if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("extensión inválida");
        }
    }
    Ok(())
}

fn check_site(p: &str) -> Result<(), &'static str> {
    if bad_text(p, 253) {
        return Err("demasiado largo o con caracteres de control");
    }
    let h = host_of(p);
    if h.is_empty() || !h.chars().all(host_char) {
        return Err("dominio inválido");
    }
    Ok(())
}

fn check_ip(p: &str) -> Result<(), &'static str> {
    let t = p.trim();
    if bad_text(p, 64) {
        return Err("IP o CIDR inválido");
    }
    match t.split_once('/') {
        Some((base, n)) => {
            let Some(ip) = parse_ip(base) else { return Err("IP o CIDR inválido") };
            let width = if ip.is_ipv4() { 32 } else { 128 };
            if n.is_empty() || n.len() > 3 || !n.bytes().all(|b| b.is_ascii_digit()) || n.parse::<u32>().map_or(true, |v| v > width) {
                return Err("prefijo CIDR inválido");
            }
            Ok(())
        }
        None => parse_ip(t).map(|_| ()).ok_or("IP o CIDR inválido"),
    }
}

fn check_name(n: &str) -> Result<(), &'static str> {
    if n.trim().is_empty() || bad_text(n, 255) {
        return Err("nombre de impresora inválido");
    }
    if n.starts_with('-') || ["__proto__", "constructor", "prototype"].contains(&n) {
        return Err("nombre de impresora no permitido");
    }
    Ok(())
}

fn rules_from(v: &Value, path: &str, n: &mut Notes) -> Option<PrinterRules> {
    let m = match v {
        Value::Null => return Some(PrinterRules::default()),
        Value::Object(m) => m,
        _ => {
            n.err(format!("{path}: las reglas deben ser un objeto"));
            return None;
        }
    };
    n.unknown(path, m, &["enabled", "mode", "apps", "sites", "ips"]);
    let mut r = PrinterRules::default();
    let mut ok = true;
    match m.get("enabled") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => r.enabled = *b,
        _ => {
            n.err(format!("{path}.enabled: debe ser verdadero o falso"));
            ok = false;
        }
    }
    match m.get("mode") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s == "block" => r.mode = Some(Mode::Block),
        Some(Value::String(s)) if s == "allow" => r.mode = Some(Mode::Allow),
        _ => {
            n.err(format!("{path}.mode: debe ser «block» o «allow»"));
            ok = false;
        }
    }
    type Check = fn(&str) -> Result<(), &'static str>;
    let lists: [(&str, Check); 3] = [("apps", check_app), ("sites", check_site), ("ips", check_ip)];
    for (k, check) in lists {
        let mut out = vec![];
        match m.get(k) {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) if a.len() <= MAX_LIST => {
                for (i, it) in a.iter().enumerate() {
                    match it.as_str() {
                        Some(s) => match check(s) {
                            Ok(()) => out.push(s.to_string()),
                            Err(why) => {
                                n.err(format!("{path}.{k}[{i}] «{}»: {why}", clip(s, 40)));
                                ok = false;
                            }
                        },
                        None => {
                            n.err(format!("{path}.{k}[{i}]: debe ser texto"));
                            ok = false;
                        }
                    }
                }
            }
            Some(Value::Array(_)) => {
                n.err(format!("{path}.{k}: demasiados elementos (máx. {MAX_LIST})"));
                ok = false;
            }
            _ => {
                n.err(format!("{path}.{k}: debe ser una lista de textos"));
                ok = false;
            }
        }
        match k {
            "apps" => r.apps = out,
            "sites" => r.sites = out,
            _ => r.ips = out,
        }
    }
    ok.then_some(r)
}

fn params_from(m: &Map<String, Value>, n: &mut Notes) -> PartialParams {
    n.unknown("parametros", m, &["unknown_policy", "interval_secs", "notify_block", "notify_allow", "theme", "retention_days"]);
    let mut p = PartialParams::default();
    let get = |k: &str| m.get(k).filter(|v| !v.is_null());
    if let Some(v) = get("unknown_policy") {
        match v.as_str() {
            Some(s @ ("allow" | "block")) => p.unknown_policy = Some(s.into()),
            _ => n.err("parametros.unknown_policy: debe ser «allow» o «block»".into()),
        }
    }
    if let Some(v) = get("theme") {
        match v.as_str() {
            Some(s @ ("light" | "dark" | "system")) => p.theme = Some(s.into()),
            _ => n.err("parametros.theme: debe ser «light», «dark» o «system»".into()),
        }
    }
    for (k, lo, hi) in [("interval_secs", 1u64, 60u64), ("retention_days", 1, 3650)] {
        if let Some(v) = get(k) {
            match v.as_u64().filter(|x| (lo..=hi).contains(x)) {
                Some(x) if k == "interval_secs" => p.interval_secs = Some(x),
                Some(x) => p.retention_days = Some(x),
                None => n.err(format!("parametros.{k}: debe ser un entero entre {lo} y {hi}")),
            }
        }
    }
    for k in ["notify_block", "notify_allow"] {
        if let Some(v) = get(k) {
            match v.as_bool() {
                Some(b) if k == "notify_block" => p.notify_block = Some(b),
                Some(b) => p.notify_allow = Some(b),
                None => n.err(format!("parametros.{k}: debe ser verdadero o falso")),
            }
        }
    }
    p
}

fn quick_from(m: &Map<String, Value>, n: &mut Notes) -> Quick {
    n.unknown("bloqueo_rapido", m, &["blocks", "paused_printers", "version"]);
    let mut q = Quick::default();
    match m.get("blocks") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) if a.len() <= MAX_BLOCKS => {
            for (i, b) in a.iter().enumerate() {
                let p = format!("bloqueo_rapido.blocks[{i}]");
                let Some(o) = b.as_object() else { n.err(format!("{p}: debe ser un objeto")); continue };
                let host = o.get("host").and_then(Value::as_str).unwrap_or("");
                let by = o.get("by").and_then(Value::as_str).unwrap_or("");
                let time = o.get("time").and_then(Value::as_u64);
                let ips: Option<Vec<String>> = o.get("ips").and_then(Value::as_array).and_then(|a| a.iter().map(|x| x.as_str().map(String::from)).collect());
                if host.is_empty() || bad_text(host, 253) || host.chars().any(char::is_whitespace) || bad_text(by, 255) || time.is_none() || ips.is_none() {
                    n.err(format!("{p}: equipo, IPs, autor u hora inválidos"));
                    continue;
                }
                let ips = ips.unwrap();
                if ips.len() > 64 || ips.iter().any(|x| check_ip(x).is_err()) {
                    n.err(format!("{p}: IP o CIDR inválido"));
                    continue;
                }
                q.blocks.push(QuickBlock { host: host.into(), ips, by: by.into(), time: time.unwrap() });
            }
        }
        _ => n.err("bloqueo_rapido.blocks: lista inválida o demasiado grande".into()),
    }
    match m.get("paused_printers") {
        None | Some(Value::Null) => {}
        Some(Value::Object(o)) if o.len() <= MAX_PRINTERS => {
            for (k, v) in o {
                let by = v.get("by").and_then(Value::as_str).unwrap_or("");
                let time = v.get("time").and_then(Value::as_u64);
                if check_name(k).is_err() || bad_text(by, 255) || time.is_none() {
                    n.err(format!("bloqueo_rapido.paused_printers «{}»: inválido", clip(k, 40)));
                    continue;
                }
                q.paused_printers.insert(k.clone(), QuickMark { by: by.into(), time: time.unwrap() });
            }
        }
        _ => n.err("bloqueo_rapido.paused_printers: debe ser un objeto de tamaño razonable".into()),
    }
    q
}

fn okid(s: &str) -> bool {
    (8..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn event_from(v: &Value, now: i64) -> Result<Event, String> {
    let m = v.as_object().ok_or("no es un objeto")?;
    let text = |k: &str, max: usize| -> Result<String, String> {
        match m.get(k) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) if s.chars().count() <= max && !s.contains('\0') => Ok(s.clone()),
            _ => Err(format!("{k} inválido")),
        }
    };
    let num = |k: &str| -> Result<Option<u64>, String> {
        match m.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(x) => x.as_u64().filter(|n| *n <= 1 << 50).map(Some).ok_or(format!("{k} fuera de rango")),
        }
    };
    let id = text("id", 64)?;
    if !okid(&id) {
        return Err("id inválido".into());
    }
    let ts = m.get("ts").and_then(Value::as_i64).filter(|t| *t >= 1_000_000_000 && *t <= now + 86400).ok_or("hora fuera de rango")?;
    let decision = text("decision", 16)?;
    if !["allow", "block", "notice"].contains(&decision.as_str()) {
        return Err("decisión inválida".into());
    }
    Ok(Event {
        id,
        ts: ts as u64,
        host: text("host", 255)?,
        ip: text("ip", 255)?,
        user: text("user", 255)?,
        printer: text("printer", 255)?,
        port: text("port", 255)?,
        conn_type: text("conn_type", 255)?,
        document: text("document", 1000)?,
        site: text("site", 255)?,
        app: text("app", 255)?,
        pages: num("pages")?,
        bytes: num("bytes")?,
        decision,
        reason: text("reason", 500)?,
        status: text("status", 255)?,
        source: text("source", 255)?,
    })
}

/// Contenido validado de un archivo de copia. Nada de esto se ha aplicado todavía.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub legacy: bool,
    pub version: i64,
    pub created: Option<i64>,
    pub host: String,
    pub app_version: String,
    pub global: Option<PrinterRules>,
    pub printers: Option<BTreeMap<String, PrinterRules>>,
    params: Option<PartialParams>,
    pub quick: Option<Quick>,
    pub events: Option<Vec<Event>>,
    pub events_discarded: usize,
    pub events_dup_in_file: usize,
    pub inventory: Option<Vec<InvItem>>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Parsed {
    pub fn has(&self, cat: &str) -> bool {
        match cat {
            "reglas" => self.global.is_some() || self.printers.is_some(),
            "parametros" => self.params.is_some(),
            "bloqueo_rapido" => self.quick.is_some(),
            "registro" => self.events.is_some(),
            "inventario" => self.inventory.is_some(),
            _ => false,
        }
    }
    pub fn categories(&self) -> Vec<&'static str> {
        CATS.iter().copied().filter(|c| self.has(c)).collect()
    }
}

/// Quita usuario/contraseña y consulta de un URI de impresora antes de exportarlo.
pub fn sanitize_uri(u: &str) -> String {
    let mut s = u.to_string();
    if let Some((scheme, rest)) = u.split_once("://") {
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (auth, tail) = rest.split_at(end);
        let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
        s = format!("{scheme}://{auth}{tail}");
    }
    if let Some(i) = s.find('?') {
        s.truncate(i);
    }
    clip(&s, 500)
}

/// Valida el archivo completo. Los fallos graves (tamaño, JSON, formato, versión) devuelven `Err`;
/// el resto se acumula en `errors` (bloquean aplicar) y `warnings` (solo informan).
pub fn parse(raw: &[u8], now: i64) -> Result<Parsed, String> {
    if raw.len() > MAX_BYTES {
        return Err("El archivo es demasiado grande (máx. 10 MiB)".into());
    }
    let raw = raw.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(raw);
    if !depth_ok(raw, MAX_DEPTH) {
        return Err("El archivo está demasiado anidado".into());
    }
    let v: Value = serde_json::from_slice(raw).map_err(|_| "El archivo no es un JSON válido".to_string())?;
    let Value::Object(top) = v else { return Err("El archivo no es una copia de SpoolGuard (se esperaba un objeto)".into()) };
    let mut n = Notes::default();
    let mut p = Parsed::default();
    let legacy = !top.contains_key("format") && ["printers", "global", "settings"].iter().any(|k| top.contains_key(*k));
    if legacy {
        // Archivo de reglas de versiones anteriores (config.json completo): se toman reglas y ajustes permitidos.
        p.legacy = true;
        let known = ["printers", "global", "settings"];
        n.unknown("archivo", &top, &known);
        p.warnings.push("Formato antiguo de reglas: se importan reglas y ajustes; no se incluyen contraseña ni tokens".into());
        if let Some(g) = top.get("global").filter(|g| !g.is_null()) {
            p.global = rules_from(g, "reglas.global", &mut n);
        }
        match top.get("printers") {
            None | Some(Value::Null) => {}
            Some(Value::Object(o)) => p.printers = printers_from(o, &mut n),
            _ => n.err("printers: debe ser un objeto".into()),
        }
        match top.get("settings") {
            None | Some(Value::Null) => {}
            Some(Value::Object(o)) => {
                // Solo los ajustes de la lista blanca; el resto (envío al servidor, pausa, contraseña) se ignora.
                let mut allowed = Map::new();
                for (k, val) in o {
                    if ["unknown_policy", "interval_secs", "notify_block", "notify_allow", "theme", "retention_days"].contains(&k.as_str()) {
                        allowed.insert(k.clone(), val.clone());
                    } else {
                        n.unknown("settings", &Map::from_iter([(k.clone(), Value::Null)]), &["paused", "send_events", "server_url"]);
                    }
                }
                p.params = Some(params_from(&allowed, &mut n));
            }
            _ => n.err("settings: debe ser un objeto".into()),
        }
    } else {
        if top.get("format").and_then(Value::as_str) != Some(FORMAT) {
            return Err("El archivo no es una copia de SpoolGuard (formato desconocido)".into());
        }
        let ver = match top.get("version") {
            Some(Value::Number(x)) => x.as_i64(),
            _ => None,
        };
        match ver {
            Some(v) if v > VERSION => return Err(format!("Versión de formato no soportada ({v}): este programa admite hasta la {VERSION}; actualiza SpoolGuard")),
            Some(v) if v >= 1 => p.version = v,
            _ => return Err("Versión de formato inválida".into()),
        }
        n.unknown("archivo", &top, &["format", "version", "created", "host", "app_version", "categories", "reglas", "parametros", "bloqueo_rapido", "registro", "inventario"]);
        p.created = top.get("created").and_then(Value::as_i64);
        p.host = top.get("host").and_then(Value::as_str).map(|s| clip(s, 255)).unwrap_or_default();
        p.app_version = top.get("app_version").and_then(Value::as_str).map(|s| clip(s, 64)).unwrap_or_default();
        if let Some(c) = top.get("categories") {
            match c.as_array() {
                Some(a) => {
                    for x in a {
                        if !x.as_str().is_some_and(|s| CATS.contains(&s)) {
                            n.warn("categories: categoría desconocida ignorada".into());
                            break;
                        }
                    }
                }
                None => n.err("categories: debe ser una lista".into()),
            }
        }
        match top.get("reglas") {
            None | Some(Value::Null) => {}
            Some(Value::Object(m)) => {
                n.unknown("reglas", m, &["global", "printers"]);
                if let Some(g) = m.get("global").filter(|g| !g.is_null()) {
                    p.global = rules_from(g, "reglas.global", &mut n);
                }
                match m.get("printers") {
                    None | Some(Value::Null) => {}
                    Some(Value::Object(o)) => p.printers = printers_from(o, &mut n),
                    _ => n.err("reglas.printers: debe ser un objeto".into()),
                }
            }
            _ => n.err("reglas: debe ser un objeto".into()),
        }
        match top.get("parametros") {
            None | Some(Value::Null) => {}
            Some(Value::Object(m)) => p.params = Some(params_from(m, &mut n)),
            _ => n.err("parametros: debe ser un objeto".into()),
        }
        match top.get("bloqueo_rapido") {
            None | Some(Value::Null) => {}
            Some(Value::Object(m)) => p.quick = Some(quick_from(m, &mut n)),
            _ => n.err("bloqueo_rapido: debe ser un objeto".into()),
        }
        match top.get("registro") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) if a.len() <= MAX_EVENTS => {
                let (mut evs, mut seen) = (Vec::with_capacity(a.len()), HashSet::new());
                let mut first_bad: Option<String> = None;
                for v in a {
                    match event_from(v, now) {
                        Ok(e) if seen.insert(e.id.clone()) => evs.push(e),
                        Ok(_) => p.events_dup_in_file += 1,
                        Err(why) => {
                            p.events_discarded += 1;
                            first_bad.get_or_insert(why);
                        }
                    }
                }
                if let Some(w) = first_bad {
                    n.warn(format!("registro: {} eventos descartados por inválidos (p. ej. {w})", p.events_discarded));
                }
                p.events = Some(evs);
            }
            Some(Value::Array(_)) => n.err(format!("registro: demasiados eventos (máx. {MAX_EVENTS})")),
            _ => n.err("registro: debe ser una lista".into()),
        }
        match top.get("inventario") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) if a.len() <= MAX_PRINTERS => {
                let mut items = vec![];
                for v in a {
                    let s = |k: &str| v.get(k).and_then(Value::as_str).map(|x| clip(x, 500)).unwrap_or_default();
                    let name = s("name");
                    if check_name(&name).is_ok() {
                        items.push(InvItem { name, kind: clip(&s("kind"), 40), uri: sanitize_uri(&s("uri")), ..Default::default() });
                    }
                }
                if items.len() != a.len() {
                    n.warn("inventario: se ignoraron elementos inválidos".into());
                }
                p.inventory = Some(items);
            }
            _ => n.err("inventario: debe ser una lista de tamaño razonable".into()),
        }
    }
    p.warnings.extend(n.warnings);
    p.errors = n.errors;
    Ok(p)
}

fn printers_from(o: &Map<String, Value>, n: &mut Notes) -> Option<BTreeMap<String, PrinterRules>> {
    if o.len() > MAX_PRINTERS {
        n.err(format!("reglas.printers: demasiadas impresoras (máx. {MAX_PRINTERS})"));
        return None;
    }
    let mut out = BTreeMap::new();
    for (k, v) in o {
        if let Err(why) = check_name(k) {
            n.err(format!("impresora «{}»: {why}", clip(k, 40)));
            continue;
        }
        if let Some(r) = rules_from(v, &format!("reglas.printers[{}]", clip(k, 40)), n) {
            out.insert(k.clone(), r);
        }
    }
    Some(out)
}

/// Contexto del equipo que importa.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    /// Impresoras que existen en este equipo.
    pub local: Vec<String>,
    /// Ids (de los eventos del archivo) que ya están en el registro.
    pub existing_ids: HashSet<String>,
    /// Eventos actuales del registro (para avisar de lo que borra «reemplazar»).
    pub event_count: u64,
    /// Servidor (admite bloqueo rápido).
    pub server: bool,
}

pub struct Plan {
    pub next: Snapshot,
    pub preview: Value,
    /// Eventos nuevos a insertar (con ids propios del archivo; los ya existentes no se repiten).
    pub events: Vec<Event>,
    pub replace_events: bool,
    pub applied: Vec<&'static str>,
    pub ok: bool,
}

fn union(a: &[String], b: &[String]) -> Vec<String> {
    let mut o = a.to_vec();
    for s in b {
        if !o.contains(s) {
            o.push(s.clone());
        }
    }
    o
}

/// Combinar: listas unidas; activación y modo del archivo (el modo vacío conserva el actual).
fn merge_rules(a: &PrinterRules, b: &PrinterRules) -> PrinterRules {
    PrinterRules { enabled: b.enabled, mode: b.mode.or(a.mode), apps: union(&a.apps, &b.apps), sites: union(&a.sites, &b.sites), ips: union(&a.ips, &b.ips), ..Default::default() }
}

fn params_value(p: &Params) -> Map<String, Value> {
    match serde_json::to_value(p) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Qué pasaría al importar `p` sobre `cur`, sin aplicar nada.
pub fn plan(cur: &Snapshot, p: &Parsed, m: &Modes, ctx: &Ctx) -> Plan {
    let mut next = cur.clone();
    let mut warns = p.warnings.clone();
    let mut applied = vec![];
    let mut pv = Map::new();
    let local: HashSet<&String> = ctx.local.iter().collect();
    let on = |o: Op, c: &str| o != Op::Omitir && p.has(c);

    // Reglas
    let mut reglas = Value::Null;
    if on(m.reglas, "reglas") {
        applied.push("reglas");
        let replace = m.reglas == Op::Reemplazar;
        let mut list: Vec<Value> = vec![];
        let (mut nuevas, mut modif, mut elim, mut iguales) = (0, 0, 0, 0);
        if let Some(fp) = &p.printers {
            if replace {
                next.printers = fp.clone();
            } else {
                for (k, r) in fp {
                    let merged = cur.printers.get(k).map_or_else(|| r.clone(), |c| merge_rules(c, r));
                    next.printers.insert(k.clone(), merged);
                }
            }
            for k in fp.keys() {
                let estado = match (cur.printers.get(k), next.printers.get(k)) {
                    (None, _) => "nueva",
                    (Some(a), Some(b)) if a == b => "igual",
                    _ => "modificada",
                };
                match estado {
                    "nueva" => nuevas += 1,
                    "igual" => iguales += 1,
                    _ => modif += 1,
                }
                list.push(json!({"nombre": k, "estado": estado, "existe": local.contains(k)}));
            }
            if replace {
                for k in cur.printers.keys().filter(|k| !fp.contains_key(*k)) {
                    elim += 1;
                    list.push(json!({"nombre": k, "estado": "eliminada", "existe": local.contains(k)}));
                }
            }
            let missing: Vec<&str> = fp.keys().filter(|k| !local.contains(k)).map(String::as_str).take(10).collect();
            if !missing.is_empty() {
                warns.push(format!("Impresoras que no existen en este equipo ({}): se importan sus reglas pero no se activa la retención de sus colas", missing.join(", ")));
            }
        }
        if let Some(g) = &p.global {
            next.global = if replace { g.clone() } else { merge_rules(&cur.global, g) };
        }
        let global = if next.global == cur.global { "igual" } else { "modificado" };
        reglas = json!({"global": global, "impresoras": list, "nuevas": nuevas, "modificadas": modif, "eliminadas": elim, "iguales": iguales});
    }

    // Parámetros
    let mut parametros = Value::Null;
    if on(m.parametros, "parametros") {
        applied.push("parametros");
        let pp = p.params.clone().unwrap_or_default();
        let base = if m.parametros == Op::Reemplazar { Params::default() } else { cur.params.clone() };
        next.params = Params {
            unknown_policy: pp.unknown_policy.unwrap_or(base.unknown_policy),
            interval_secs: pp.interval_secs.unwrap_or(base.interval_secs),
            notify_block: pp.notify_block.unwrap_or(base.notify_block),
            notify_allow: pp.notify_allow.unwrap_or(base.notify_allow),
            theme: pp.theme.unwrap_or(base.theme),
            retention_days: pp.retention_days.unwrap_or(base.retention_days),
        };
        let (a, b) = (params_value(&cur.params), params_value(&next.params));
        let cambios: Vec<Value> = b.iter().filter(|(k, v)| a.get(*k) != Some(v)).map(|(k, v)| json!({"campo": k, "de": a[k], "a": v})).collect();
        parametros = json!({"cambios": cambios});
    }

    // Bloqueo rápido (solo servidor)
    let mut quick = Value::Null;
    if p.has("bloqueo_rapido") && !ctx.server {
        warns.push("El bloqueo rápido solo existe en el servidor: se ignora en este equipo".into());
    } else if on(m.bloqueo_rapido, "bloqueo_rapido") {
        applied.push("bloqueo_rapido");
        let fq = p.quick.clone().unwrap_or_default();
        let replace = m.bloqueo_rapido == Op::Reemplazar;
        if replace {
            next.quick = fq.clone();
        } else {
            for b in &fq.blocks {
                next.quick.blocks.retain(|x| x.host != b.host);
                next.quick.blocks.push(b.clone());
            }
            for (k, v) in &fq.paused_printers {
                next.quick.paused_printers.insert(k.clone(), v.clone());
            }
        }
        let nb = fq.blocks.iter().filter(|b| !cur.quick.blocks.iter().any(|x| x.host == b.host)).count();
        let np = fq.paused_printers.keys().filter(|k| !cur.quick.paused_printers.contains_key(*k)).count();
        let rb = if replace { cur.quick.blocks.iter().filter(|b| !fq.blocks.iter().any(|x| x.host == b.host)).count() } else { 0 };
        let rp = if replace { cur.quick.paused_printers.keys().filter(|k| !fq.paused_printers.contains_key(*k)).count() } else { 0 };
        quick = json!({"bloqueos_nuevos": nb, "pausadas_nuevas": np, "bloqueos_eliminados": rb, "pausadas_eliminadas": rp});
        let gone: Vec<&str> = fq.paused_printers.keys().filter(|k| !local.contains(k)).map(String::as_str).take(10).collect();
        if !gone.is_empty() {
            warns.push(format!("Impresoras pausadas que no existen en este equipo ({}): no se retienen", gone.join(", ")));
        }
    }

    // Registro
    let mut registro = Value::Null;
    let (mut events, mut replace_events) = (vec![], false);
    if on(m.registro, "registro") {
        applied.push("registro");
        replace_events = m.registro == Op::Reemplazar;
        let evs = p.events.clone().unwrap_or_default();
        let mut dup = p.events_dup_in_file;
        for e in evs {
            if !replace_events && ctx.existing_ids.contains(&e.id) {
                dup += 1;
            } else {
                events.push(e);
            }
        }
        registro = json!({"a_importar": events.len(), "duplicados": dup, "descartados": p.events_discarded, "a_borrar": if replace_events { ctx.event_count } else { 0 }});
    }

    // Inventario: informativo
    let mut inventario = Value::Null;
    if let Some(inv) = &p.inventory {
        let falta = inv.iter().filter(|i| !local.contains(&i.name)).count();
        if falta > 0 && !ctx.local.is_empty() {
            warns.push(format!("Inventario: {falta} impresoras del archivo no existen en este equipo"));
        }
        inventario = json!({"impresoras": inv.len()});
    }

    let ok = p.errors.is_empty();
    pv.insert("ok".into(), json!(ok));
    pv.insert("legacy".into(), json!(p.legacy));
    pv.insert("version".into(), json!(p.version));
    pv.insert("creado".into(), json!(p.created));
    pv.insert("equipo".into(), json!(p.host));
    pv.insert("app_version".into(), json!(p.app_version));
    pv.insert("categorias".into(), json!(p.categories()));
    let modos = json!({"reglas": op_name(m.reglas), "parametros": op_name(m.parametros), "bloqueo_rapido": op_name(m.bloqueo_rapido), "registro": op_name(m.registro)});
    pv.insert("modos".into(), modos);
    pv.insert("reglas".into(), reglas);
    pv.insert("parametros".into(), parametros);
    pv.insert("bloqueo_rapido".into(), quick);
    pv.insert("registro".into(), registro);
    pv.insert("inventario".into(), inventario);
    pv.insert("errores".into(), json!(p.errors));
    pv.insert("avisos".into(), json!(warns));
    Plan { next, preview: Value::Object(pv), events, replace_events, applied, ok }
}

/// Archivo de copia a partir del estado y las categorías pedidas (solo campos de la lista blanca).
pub fn export(snap: &Snapshot, cats: &[String], events: &[Event], inv: &[InvItem], meta: &Meta) -> Value {
    let has = |c: &str| cats.iter().any(|x| x == c);
    let chosen: Vec<&str> = CATS.iter().copied().filter(|c| has(c)).collect();
    let mut o = Map::new();
    o.insert("format".into(), json!(FORMAT));
    o.insert("version".into(), json!(VERSION));
    o.insert("created".into(), json!(meta.created));
    o.insert("host".into(), json!(meta.host));
    o.insert("app_version".into(), json!(meta.app_version));
    o.insert("categories".into(), json!(chosen));
    if has("reglas") {
        o.insert("reglas".into(), json!({"global": snap.global, "printers": snap.printers}));
    }
    if has("parametros") {
        o.insert("parametros".into(), json!(snap.params));
    }
    if has("bloqueo_rapido") {
        o.insert("bloqueo_rapido".into(), json!(snap.quick));
    }
    if has("registro") {
        o.insert("registro".into(), json!(events));
    }
    if has("inventario") {
        let v: Vec<InvItem> = inv.iter().map(|i| InvItem { uri: sanitize_uri(&i.uri), ..i.clone() }).collect();
        o.insert("inventario".into(), json!(v));
    }
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profundidad_ignora_cadenas() {
        assert!(depth_ok(br#"{"a":"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[["}"#, 4));
        assert!(!depth_ok(&b"[".repeat(40), 32));
    }

    #[test]
    fn uri_sin_credenciales() {
        assert_eq!(sanitize_uri("ipp://admin:clave@10.0.0.5/ipp/print?x=1"), "ipp://10.0.0.5/ipp/print");
        assert_eq!(sanitize_uri("socket://10.0.0.5:9100"), "socket://10.0.0.5:9100");
        assert_eq!(sanitize_uri("USB001"), "USB001");
    }
}
