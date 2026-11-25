//! Host de Native Messaging (Chrome/Edge/Firefox): mensajes JSON con prefijo
//! de 4 bytes little-endian por stdin/stdout. Responde a `get_rules`.
use control_core::rules::{Mode, PrinterRules};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::path::PathBuf;

const MAX_MSG: usize = 4 * 1024 * 1024;

/// Lee un mensaje enmarcado; `Ok(None)` en EOF limpio.
fn read_message<R: Read>(r: &mut R) -> io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_MSG {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "mensaje demasiado grande"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_message<W: Write>(w: &mut W, v: &Value) -> io::Result<()> {
    let data = serde_json::to_vec(v)?;
    w.write_all(&(data.len() as u32).to_le_bytes())?;
    w.write_all(&data)?;
    w.flush()
}

fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CONTROL_IMPRESION_DIR") {
        return PathBuf::from(d);
    }
    const ID: &str = "cl.cohasa.controlimpresion";
    let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join(ID)
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support").join(ID)
    } else {
        match std::env::var_os("XDG_DATA_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x).join(ID),
            _ => home().join(".local/share").join(ID),
        }
    }
}

fn norm(d: &str) -> String {
    d.trim().trim_start_matches("*.").trim_matches('.').to_lowercase()
}

/// Sitios cuya regla no está suspendida.
fn active_sites(p: &PrinterRules) -> Vec<String> {
    p.sites.iter().filter(|s| !p.is_paused(s)).cloned().collect()
}

fn push_unique(out: &mut Vec<String>, sites: &[String]) {
    for s in sites.iter().map(|s| norm(s)).filter(|s| !s.is_empty()) {
        if !out.contains(&s) {
            out.push(s);
        }
    }
}

/// Calcula la respuesta de `get_rules` a partir del contenido de config.json.
fn rules_from_config(cfg: &Value) -> Value {
    let (mut block, mut allow) = (Vec::new(), Vec::new());
    let mut any_allow = false;
    // Reglas de cada impresora y la global (aplica a todas).
    let mut all: Vec<&Value> = cfg.get("printers").and_then(Value::as_object).map(|m| m.values().collect()).unwrap_or_default();
    if let Some(g) = cfg.get("global") {
        all.push(g);
    }
    for v in all {
        let Ok(p) = serde_json::from_value::<PrinterRules>(v.clone()) else { continue };
        if !p.enabled {
            continue;
        }
        match p.mode.unwrap_or_default() {
            Mode::Block => push_unique(&mut block, &active_sites(&p)),
            Mode::Allow => {
                any_allow = true;
                push_unique(&mut allow, &active_sites(&p));
            }
        }
    }
    json!({ "block": block, "allow_only": if any_allow { json!(allow) } else { Value::Null } })
}

fn handle(msg: &Value, dir: &std::path::Path) -> Value {
    // La app lee esta marca para mostrar si la extensión está conectada.
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("host_seen"), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string());
    if msg.get("type").and_then(Value::as_str) == Some("print_intent") {
        let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        // Entrada no confiable: solo http(s) y largos acotados.
        let (u, t) = (s("url"), s("title"));
        let lu = u.to_lowercase();
        if !(lu.starts_with("http://") || lu.starts_with("https://")) || u.len() > 300 || t.chars().count() > 300 {
            return json!({ "error": "intención inválida" });
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let kind = if s("kind") == "agent" { "agent" } else { "" };
        return match control_core::intents::record_kind(dir, &s("url"), &s("title"), kind, now) {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "error": e.to_string() }),
        };
    }
    // Versión de la extensión empaquetada en la app y copia a <datos>/extension (el navegador la recarga después).
    match msg.get("type").and_then(Value::as_str) {
        Some("get_version") => {
            return match control_core::extension::versions(dir) {
                Ok((v, inst)) => json!({ "version": v, "installed": inst }),
                Err(e) => json!({ "error": e }),
            }
        }
        Some("update_extension") => {
            return match control_core::extension::update_installed(dir) {
                Ok(v) => json!({ "ok": true, "version": v }),
                Err(e) => json!({ "error": e }),
            }
        }
        _ => {}
    }
    if !matches!(msg.get("type").and_then(Value::as_str), Some("get_rules") | Some("subscribe")) {
        return json!({ "error": "tipo de mensaje desconocido" });
    }
    match std::fs::read_to_string(dir.join("config.json")) {
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(cfg) => rules_from_config(&cfg),
            Err(e) => json!({ "error": format!("config.json inválido: {e}") }),
        },
        // Sin config: sin reglas.
        Err(e) if e.kind() == io::ErrorKind::NotFound => json!({ "block": [], "allow_only": null }),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

/// Serializa las escrituras a stdout (hilo principal y vigilante).
static OUT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn send(v: &Value) -> bool {
    let _g = OUT.lock().unwrap_or_else(|e| e.into_inner());
    let mut o = io::stdout().lock();
    write_message(&mut o, v).is_ok()
}

/// Con `subscribe` (puerto persistente) vigila config.json y empuja las reglas al cambiar.
fn watch(dir: PathBuf) {
    let mtime = || std::fs::metadata(dir.join("config.json")).and_then(|m| m.modified()).ok();
    let mut last = mtime();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let now = mtime();
        if now != last {
            last = now;
            let mut r = handle(&json!({"type":"get_rules"}), &dir);
            r["type"] = json!("rules");
            if !send(&r) {
                break; // la extensión cerró el puerto
            }
        }
    }
}

pub fn main() {
    let dir = data_dir();
    let mut i = io::stdin().lock();
    let mut watching = false;
    while let Ok(Some(msg)) = read_message(&mut i) {
        if msg.get("type").and_then(Value::as_str) == Some("subscribe") && !watching {
            watching = true;
            let d = dir.clone();
            std::thread::spawn(move || watch(d));
        }
        if !send(&handle(&msg, &dir)) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_ida_y_vuelta() {
        let mut buf = Vec::new();
        write_message(&mut buf, &json!({"type":"get_rules"})).unwrap();
        let n = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(n, buf.len() - 4);
        let v = read_message(&mut &buf[..]).unwrap().unwrap();
        assert_eq!(v["type"], "get_rules");
    }

    #[test]
    fn eof_limpio_y_truncado() {
        assert!(read_message(&mut &b""[..]).unwrap().is_none());
        assert!(read_message(&mut &[5u8, 0, 0, 0, b'{'][..]).is_err());
    }

    #[test]
    fn rechaza_mensaje_enorme() {
        let b = (MAX_MSG as u32 + 1).to_le_bytes();
        assert!(read_message(&mut &b[..]).is_err());
    }

    #[test]
    fn reglas_block_allow_y_unicos() {
        let cfg = json!({"printers":{
            "A":{"enabled":true,"mode":"block","sites":["Ejemplo.cl","*.otro.cl"]},
            "B":{"enabled":true,"mode":"block","sites":["ejemplo.cl"]},
            "C":{"enabled":false,"mode":"block","sites":["off.cl"]},
            "D":{"enabled":true,"mode":"allow","sites":["ok.cl"]}
        }});
        let r = rules_from_config(&cfg);
        assert_eq!(r["block"], json!(["ejemplo.cl", "otro.cl"]));
        assert_eq!(r["allow_only"], json!(["ok.cl"]));
    }

    #[test]
    fn sin_allow_es_null() {
        let cfg = json!({"printers":{"A":{"enabled":true,"mode":"block","sites":["x.cl"]}}});
        assert!(rules_from_config(&cfg)["allow_only"].is_null());
        assert_eq!(rules_from_config(&json!({}))["block"], json!([]));
    }

    #[test]
    fn intencion_invalida_se_rechaza() {
        let d = std::env::temp_dir().join("ci-host-test-int");
        for url in ["javascript:alert(1)", "file:///etc/passwd", ""] {
            let r = handle(&json!({"type":"print_intent","url":url,"title":"x"}), &d);
            assert!(r.get("error").is_some(), "{url}");
        }
        let r = handle(&json!({"type":"print_intent","url":"https://a.cl","title":"x".repeat(301)}), &d);
        assert!(r.get("error").is_some());
        let r = handle(&json!({"type":"print_intent","url":"https://a.cl","title":"ok"}), &d);
        assert_eq!(r["ok"], true);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn regla_global_incluida() {
        let cfg = json!({"printers":{}, "global":{"enabled":true,"mode":"block","sites":["Gesprod.catchup.cl"]}});
        assert_eq!(rules_from_config(&cfg)["block"], json!(["gesprod.catchup.cl"]));
        let off = json!({"global":{"enabled":false,"mode":"block","sites":["x.cl"]}});
        assert_eq!(rules_from_config(&off)["block"], json!([]));
    }

    #[test]
    fn version_y_actualizacion_de_la_extension() {
        let root = std::env::temp_dir().join(format!("ci-host-test-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (res, data) = (root.join("res/extension"), root.join("data"));
        std::fs::create_dir_all(&res).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        // Sin ext_source (la app nunca la instaló): error, no pánico.
        assert!(handle(&json!({"type":"get_version"}), &data).get("error").is_some());
        std::fs::write(res.join("manifest.json"), r#"{"version":"0.2.0"}"#).unwrap();
        control_core::extension::install_from(&data, &res).unwrap();
        std::fs::write(res.join("manifest.json"), r#"{"version":"0.3.0"}"#).unwrap();
        let r = handle(&json!({"type":"get_version"}), &data);
        assert_eq!((r["version"].as_str(), r["installed"].as_str()), (Some("0.3.0"), Some("0.2.0")));
        let r = handle(&json!({"type":"update_extension"}), &data);
        assert_eq!((r["ok"].as_bool(), r["version"].as_str()), (Some(true), Some("0.3.0")));
        let r = handle(&json!({"type":"get_version"}), &data);
        assert_eq!(r["installed"], "0.3.0");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tipo_desconocido() {
        let r = handle(&json!({"type":"x"}), &std::env::temp_dir().join("ci-host-test"));
        assert!(r.get("error").is_some());
    }

    #[test]
    fn sitio_suspendido_no_se_envia() {
        let cfg = json!({"printers":{}, "global":{"enabled":true,"mode":"block","sites":["a.cl","b.cl"],"paused":{"a.cl":0,"b.cl":1}}});
        assert_eq!(rules_from_config(&cfg)["block"], json!(["b.cl"]));
    }
}
