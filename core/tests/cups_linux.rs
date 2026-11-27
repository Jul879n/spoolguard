//! Prueba de integración real con CUPS en Linux y macOS (cola file:///dev/null, o la de CUPS_E2E_URI:
//! en macOS el backend file:// está restringido: usa p. ej. socket://127.0.0.1:19100 con un listener).
//! Solo corre con `CUPS_E2E=1` (la CI la lanza con privilegios para `lpadmin`);
//! sin esa variable se omite para no romper `cargo test` en equipos sin CUPS.
#![cfg(unix)]

use control_core::cups;
use control_core::rules::{Decision, Mode, PrinterRules};
use std::{
    path::{Path, PathBuf},
    process::Command,
    thread::sleep,
    time::{Duration, Instant},
};

const Q: &str = "CtrlTest";

fn sh(cmd: &str, args: &[&str]) -> Result<String, String> {
    let o = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(format!("{cmd} {args:?}: {}", String::from_utf8_lossy(&o.stderr).trim()))
    }
}

/// Espera acotada hasta que `f` devuelva true.
fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        sleep(Duration::from_millis(300));
    }
    panic!("tiempo agotado esperando: {what}");
}

/// Borra la cola al salir, también si la prueba falla.
struct Queue;
impl Drop for Queue {
    fn drop(&mut self) {
        let _ = Command::new("lpadmin").args(["-x", Q]).output();
    }
}

fn create_queue() -> Queue {
    let uri = std::env::var("CUPS_E2E_URI").unwrap_or_else(|_| "file:///dev/null".into());
    let _ = Command::new("lpadmin").args(["-x", Q]).output(); // restos de una corrida previa
    // Se prueban los modelos en orden: el genérico de CUPS (sample.drv) y luego raw.
    let mut errs = vec![];
    for model in ["drv:///sample.drv/generic.ppd", "raw"] {
        match sh("lpadmin", &["-p", Q, "-E", "-v", &uri, "-m", model]) {
            Ok(_) => return Queue,
            Err(e) => errs.push(e),
        }
    }
    panic!("no se pudo crear la cola: {errs:?}");
}

fn submit(title: &str, file: &Path) -> u32 {
    let out = sh("lp", &["-d", Q, "-t", title, file.to_str().unwrap()]).expect("lp");
    // "request id is CtrlTest-7 (1 file(s))"
    out.split_whitespace()
        .find_map(|w| w.strip_prefix(&format!("{Q}-")))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no se pudo leer el id de: {out}"))
}

#[test]
fn bloquear_y_liberar_en_cups_real() {
    if std::env::var("CUPS_E2E").as_deref() != Ok("1") {
        eprintln!("omitida: define CUPS_E2E=1 (requiere CUPS y permisos de lpadmin)");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cups_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.txt");
    std::fs::write(&file, "x\n").unwrap();

    let _q = create_queue();
    cups::set_hold(Q, true, false).expect("set_hold");

    let rules = PrinterRules {
        enabled: true,
        mode: Some(Mode::Block),
        apps: vec!["*.docx".into()],
        ..Default::default()
    };

    // 1) informe.docx: retenido, bloqueado y cancelado.
    let id1 = submit("informe.docx", &file);
    wait_until("job 1 retenido", 20, || cups::held_job_ids(Q).map(|v| v.contains(&id1)).unwrap_or(false));
    let info = cups::job_info(&dir, id1).expect("job_info");
    assert_eq!(info.job.document, "informe.docx");
    assert!(info.held, "el trabajo debe estar pending-held");
    assert_eq!(rules.decide(&info.job), Decision::Deny);
    cups::cancel(id1).expect("cancel");
    wait_until("cola vacía", 20, || cups::held_job_ids(Q).map(|v| v.is_empty()).unwrap_or(false));

    // 2) notas.txt: permitido, se libera y sale de la cola.
    let id2 = submit("notas.txt", &file);
    wait_until("job 2 retenido", 20, || cups::held_job_ids(Q).map(|v| v.contains(&id2)).unwrap_or(false));
    let info = cups::job_info(&dir, id2).expect("job_info");
    assert_eq!(info.job.document, "notas.txt");
    assert!(info.held);
    assert_eq!(rules.decide(&info.job), Decision::Allow);
    cups::release(id2).expect("release");
    wait_until("job 2 impreso (fuera de la cola)", 30, || {
        cups::held_job_ids(Q).map(|v| !v.contains(&id2)).unwrap_or(false)
    });

}
