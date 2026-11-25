//! Prueba de integración real: servidor Go local + cliente Rust enviando lotes reales.
//! Solo corre si `SPOOLGUARD_SERVER_BIN` apunta al binario del servidor (si no, se omite):
//!   go build -o work/db-bin/control-server ./server
//!   SPOOLGUARD_SERVER_BIN=$PWD/work/db-bin/control-server cargo test -p control-core --test server_integration -- --nocapture
//! Usa work/db-data (se borra al terminar) y un PATH vacío para que el servidor no toque impresoras reales.

use control_core::events::*;
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn work() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("work")
}

struct Server {
    child: Option<Child>,
    bin: String,
    dir: PathBuf,
    port: u16,
}

impl Server {
    fn start(&mut self) {
        let nobin = self.dir.join("nobin");
        fs::create_dir_all(&nobin).unwrap();
        let child = Command::new(&self.bin)
            .env("CONTROL_LISTEN", format!("127.0.0.1:{}", self.port))
            .env("CONTROL_IMPRESION_DIR", self.dir.join("srv"))
            .env("CONTROL_ADMIN_PASSWORD", "clave-de-prueba")
            .env("PATH", &nobin)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("no se pudo lanzar el servidor");
        self.child = Some(child);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(15) {
            if ureq::get(&format!("http://127.0.0.1:{}/healthz", self.port)).call().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("el servidor no arrancó");
    }
    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn api(&self, session: &str, cmd: &str, body: &str) -> serde_json::Value {
        let mut r = ureq::post(&format!("{}/api/{cmd}", self.url())).header("Authorization", &format!("Bearer {session}")).header("Content-Type", "application/json").send(body.as_bytes()).unwrap();
        serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
    }
    fn login(&self) -> String {
        let mut r = ureq::post(&format!("{}/api/login", self.url())).send(r#"{"password":"clave-de-prueba"}"#.as_bytes()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap();
        v["token"].as_str().unwrap().to_string()
    }
    fn total(&self) -> u64 {
        let s = self.login();
        self.api(&s, "query_events", r#"{"page":1,"size":1}"#)["total"].as_u64().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn ev(i: u64) -> Event {
    Event { id: new_id(), ts: 1_900_000_000u64.min(now()) - 1000 + i, host: "pc-rust".into(), user: "ana".into(), printer: "Oficina".into(), document: format!("doc{i}.pdf"), decision: "allow".into(), status: "liberado".into(), source: "desktop".into(), ..Default::default() }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[test]
fn cliente_rust_contra_servidor_go() {
    let Ok(bin) = std::env::var("SPOOLGUARD_SERVER_BIN") else {
        eprintln!("SPOOLGUARD_SERVER_BIN no definido: prueba de integración omitida");
        return;
    };
    let dir = work().join("db-data");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut srv = Server { child: None, bin, dir: dir.clone(), port };
    srv.start();

    // Token de equipo creado por el administrador.
    let s = srv.login();
    let tok = srv.api(&s, "create_ext_token", r#"{"name":"PC Rust","kind":"device"}"#)["token"].as_str().unwrap().to_string();
    assert!(tok.starts_with("sgm_"));
    let url = srv.url();
    let make = || -> Result<Box<dyn Transport>, String> { Ok(Box::new(HttpTransport::new(&url, &tok)?)) };

    // 1) Lote real: 250 eventos en lotes de 100.
    let cdir = dir.join("cliente");
    let db = EventDb::open(&cdir).unwrap();
    let evs: Vec<Event> = (0..250).map(ev).collect();
    for e in &evs {
        db.insert(e, true).unwrap();
    }
    let t = HttpTransport::new(&srv.url(), &tok).unwrap();
    assert_eq!(flush(&db, &t, 100).unwrap(), 250);
    assert_eq!(db.pending_count(), 0);
    assert_eq!(srv.total(), 250, "llegaron todos");
    let page = srv.api(&srv.login(), "query_events", r#"{"filter":{"text":"doc7.pdf"},"page":1,"size":5}"#);
    assert_eq!(page["rows"][0]["device"], "PC Rust", "la etiqueta la pone el servidor");

    // 2) Reenviar los mismos ids (cola nueva con los mismos eventos) no duplica.
    let db2 = EventDb::open(&dir.join("cliente2")).unwrap();
    for e in &evs {
        db2.insert(e, true).unwrap();
    }
    assert_eq!(flush(&db2, &t, 100).unwrap(), 250, "el servidor los da por aceptados");
    assert_eq!(srv.total(), 250, "sin duplicados");

    // 3) Token inválido: error de autenticación y todo queda pendiente.
    db.insert(&ev(1000), true).unwrap();
    let bad = HttpTransport::new(&srv.url(), "sgm_inventado").unwrap();
    assert!(matches!(flush(&db, &bad, 100), Err(SendError::Auth(_))));
    assert_eq!(db.pending_count(), 1);

    // 4) Servidor caído: los eventos se acumulan; al volver se envían (con el Syncer y su espera).
    srv.stop();
    for i in 1001..1006 {
        db.insert(&ev(i), true).unwrap();
    }
    assert_eq!(db.pending_count(), 6);
    let mut sy = Syncer::default();
    sy.tick(1000, true, &db, &make);
    assert!(sy.last_error.as_deref().unwrap_or("").contains("sin conexión"), "{:?}", sy.last_error);
    assert_eq!(db.pending_count(), 6, "se conservan en la cola local");
    sy.tick(1001, true, &db, &make); // dentro de la espera: no reintenta
    srv.start();
    sy.tick(1002, true, &db, &make);
    assert_eq!(db.pending_count(), 6, "aún dentro del backoff");
    sy.tick(1100, true, &db, &make);
    assert_eq!(db.pending_count(), 0, "{:?}", sy.last_error);
    assert_eq!(srv.total(), 256);
    drop(srv); // borra work/db-data
    assert!(!dir.exists());
}
