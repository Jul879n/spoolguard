//! Vectores compartidos con la implementación Go (`internal/app/backup.go`): `tests/vectors/backup.json`.
use control_core::backup::*;
use control_core::events::Event;
use serde_json::{json, Value};
use std::collections::HashSet;

fn snap(v: Option<&Value>) -> Snapshot {
    v.map(|v| serde_json::from_value(v.clone()).expect("snapshot inválido")).unwrap_or_default()
}

fn raw_of(c: &Value) -> Vec<u8> {
    if let Some(r) = c.get("raw").and_then(Value::as_str) {
        return r.as_bytes().to_vec();
    }
    if let Some(n) = c.pointer("/gen/pad").and_then(Value::as_u64) {
        return format!("{{\"format\":\"spoolguard-backup\",\"version\":1,\"pad\":\"{}\"}}", "a".repeat(n as usize)).into_bytes();
    }
    if let Some(d) = c.pointer("/gen/deep").and_then(Value::as_u64) {
        return format!("{}{}", "[".repeat(d as usize), "]".repeat(d as usize)).into_bytes();
    }
    serde_json::to_vec(&c["file"]).unwrap()
}

fn lookup<'a>(v: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(v, |acc, k| acc.get(k).unwrap_or(&Value::Null))
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

#[test]
fn vectores_de_copia() {
    let f: Value = serde_json::from_str(include_str!("vectors/backup.json")).expect("backup.json inválido");
    let now = f["now"].as_i64().unwrap();
    let cases = f["cases"].as_array().unwrap();
    assert!(cases.len() >= 25);
    for c in cases {
        let name = c["name"].as_str().unwrap();
        match c["kind"].as_str().unwrap() {
            "parse_error" => {
                let e = parse(&raw_of(c), now).err().unwrap_or_else(|| panic!("debía fallar: {name}"));
                assert!(e.contains(c["error"].as_str().unwrap()), "{name}: «{e}»");
            }
            "modes_error" => {
                let e = Modes::parse(&c["modo"]).err().unwrap_or_else(|| panic!("debía fallar: {name}"));
                assert!(e.contains(c["error"].as_str().unwrap()), "{name}: «{e}»");
            }
            "import" => {
                let p = parse(&raw_of(c), now).unwrap_or_else(|e| panic!("{name}: {e}"));
                let ctx = Ctx {
                    local: strs(&c["local"]),
                    existing_ids: strs(&c["existing_ids"]).into_iter().collect::<HashSet<_>>(),
                    event_count: c["event_count"].as_u64().unwrap_or(0),
                    server: c["server"].as_bool().unwrap_or(false),
                };
                let cur = snap(c.get("current"));
                let m = Modes::parse(&c["modo"]).unwrap();
                let pl = plan(&cur, &p, &m, &ctx);
                let ex = &c["expect"];
                assert_eq!(pl.ok, ex["ok"].as_bool().unwrap(), "{name}: ok ({:?})", pl.preview["errores"]);
                assert_eq!(pl.preview["ok"], json!(pl.ok), "{name}");
                if let Some(want) = ex.get("errors_contain") {
                    let all = strs(&pl.preview["errores"]).join("\n");
                    for w in strs(want) {
                        assert!(all.contains(&w), "{name}: falta error «{w}» en\n{all}");
                    }
                }
                if let Some(want) = ex.get("warnings_contain") {
                    let all = strs(&pl.preview["avisos"]).join("\n");
                    for w in strs(want) {
                        assert!(all.contains(&w), "{name}: falta aviso «{w}» en\n{all}");
                    }
                }
                if let Some(pv) = ex.get("preview").and_then(Value::as_object) {
                    for (k, v) in pv {
                        assert_eq!(lookup(&pl.preview, k), v, "{name}: preview.{k}");
                    }
                }
                if pl.ok {
                    if let Some(n) = ex.get("next") {
                        assert_eq!(serde_json::to_value(&pl.next).unwrap(), serde_json::to_value(snap(Some(n))).unwrap(), "{name}: next");
                    }
                }
                if let Some(a) = ex.get("applied") {
                    let got: Vec<String> = pl.applied.iter().map(|s| s.to_string()).collect();
                    assert_eq!(got, strs(a), "{name}: applied");
                }
                if let Some(ids) = ex.get("event_ids") {
                    let got: Vec<String> = pl.events.iter().map(|e| e.id.clone()).collect();
                    assert_eq!(got, strs(ids), "{name}: event_ids");
                }
                if let Some(r) = ex.get("replace_events") {
                    assert_eq!(pl.replace_events, r.as_bool().unwrap(), "{name}: replace_events");
                }
            }
            "export" => {
                let state = snap(c.get("state"));
                let events: Vec<Event> = serde_json::from_value(c["events"].clone()).unwrap();
                let inv: Vec<InvItem> = serde_json::from_value(c["inventory"].clone()).unwrap();
                let meta = Meta { created: c["meta"]["created"].as_i64().unwrap(), host: c["meta"]["host"].as_str().unwrap().into(), app_version: c["meta"]["app_version"].as_str().unwrap().into() };
                let file = export(&state, &strs(&c["categories"]), &events, &inv, &meta);
                assert_eq!(file, c["expect_file"], "{name}: archivo exportado");
                // Ida y vuelta: importar ese archivo en un equipo vacío (reemplazar) da lo mismo.
                let p = parse(&serde_json::to_vec(&file).unwrap(), now).unwrap();
                assert!(p.errors.is_empty(), "{name}: {:?}", p.errors);
                let server = c["server"].as_bool().unwrap_or(false);
                let ctx = Ctx { server, ..Default::default() };
                let pl = plan(&Snapshot::default(), &p, &Modes::all(Op::Reemplazar), &ctx);
                assert!(pl.ok);
                assert_eq!(pl.next, state, "{name}: ida y vuelta");
                assert_eq!(pl.events, events, "{name}: eventos de ida y vuelta");
                // Idempotente: con esos ids ya presentes y modo combinar no se repite ningún evento.
                let ctx2 = Ctx { server, existing_ids: events.iter().map(|e| e.id.clone()).collect(), ..Default::default() };
                let pl2 = plan(&pl.next, &p, &Modes::all(Op::Combinar), &ctx2);
                assert!(pl2.events.is_empty(), "{name}: no idempotente");
                assert_eq!(pl2.next, state, "{name}: combinar sobre lo mismo no cambia nada");
            }
            k => panic!("tipo de caso desconocido: {k}"),
        }
    }
}

#[test]
fn el_archivo_exportado_nunca_trae_secretos() {
    // El archivo solo contiene campos de la lista blanca: ninguna clave de secretos.
    let all: Vec<String> = CATS.iter().map(|s| s.to_string()).collect();
    let file = export(&Snapshot::default(), &all, &[], &[], &Meta { created: 1, host: "h".into(), app_version: "v".into() });
    let t = file.to_string().to_lowercase();
    for w in ["password", "hash", "salt", "token", "session", "secret"] {
        assert!(!t.contains(w), "el archivo contiene «{w}»: {t}");
    }
}

#[test]
fn archivo_hostil_con_listas_gigantes_se_rechaza() {
    let big = json!({"format": "spoolguard-backup", "version": 1, "reglas": {"printers": {"P": {"apps": vec!["a"; 3000]}}}});
    let p = parse(&serde_json::to_vec(&big).unwrap(), 1_800_000_000).unwrap();
    assert!(!p.errors.is_empty());
    let pl = plan(&Snapshot::default(), &p, &Modes::all(Op::Reemplazar), &Ctx::default());
    assert!(!pl.ok);
}
