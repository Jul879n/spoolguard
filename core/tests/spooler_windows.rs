//! Prueba real del spooler de Windows con una impresora virtual (Microsoft Print to PDF).
#![cfg(windows)]
use control_core::spooler::{self, Cancelled};
use std::{path::Path, process::Command, thread::sleep, time::Duration};

/// Ejecuta PowerShell con límite de 60 s para no colgar el CI.
fn ps(script: &str) -> String {
    eprintln!(">> ps: {script}");
    let mut c = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let t = std::time::Instant::now();
    while c.try_wait().unwrap().is_none() {
        if t.elapsed() > Duration::from_secs(60) {
            let _ = c.kill();
            return "TIMEOUT".into();
        }
        sleep(Duration::from_millis(200));
    }
    let o = c.wait_with_output().unwrap();
    let r = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    eprintln!("<< {}", r.trim());
    r
}

/// Envía un trabajo RAW directo con la API del spooler (StartDocPrinter) y lo deja en cola.
fn submit(p: &str, doc: &str) {
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::Graphics::Printing::*,
    };
    let w = |t: &str| -> Vec<u16> { t.encode_utf16().chain(std::iter::once(0)).collect() };
    let (name, docw, raw) = (w(p), w(doc), w("RAW"));
    unsafe {
        let mut h = PRINTER_HANDLE::default();
        OpenPrinterW(PCWSTR(name.as_ptr()), &mut h, None).expect("OpenPrinterW");
        let info = DOC_INFO_1W { pDocName: PWSTR(docw.as_ptr() as *mut u16), pOutputFile: PWSTR::null(), pDatatype: PWSTR(raw.as_ptr() as *mut u16) };
        let id = StartDocPrinterW(h, 1, &info);
        assert!(id != 0, "StartDocPrinterW devolvió 0");
        StartPagePrinter(h).expect("StartPagePrinter");
        let mut n = 0u32;
        let data = b"hola\r\n";
        WritePrinter(h, data.as_ptr() as *const _, data.len() as u32, &mut n).expect("WritePrinter");
        let _ = EndPagePrinter(h);
        let _ = EndDocPrinter(h);
        let _ = ClosePrinter(h);
    }
}

fn wait_jobs(p: &str, n: usize) -> Vec<u32> {
    for _ in 0..30 {
        let ids = spooler::held_job_ids(p).unwrap();
        if ids.len() >= n {
            return ids;
        }
        sleep(Duration::from_millis(500));
    }
    spooler::held_job_ids(p).unwrap()
}

/// Con la cola reanudada para liberar un trabajo, los que llegan a continuación no deben imprimirse:
/// el vigilante de notificaciones los pausa al instante (sin esperar al sondeo).
#[test]
fn rafaga_tras_liberar_no_se_filtra() {
    let p = "CtrlTestRafaga";
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let port = format!(r"{base}\ctrltest-rafaga.prn");
    let out = ps(&format!(
        "Get-PrintJob -PrinterName {p} -ErrorAction SilentlyContinue | Remove-PrintJob -ErrorAction SilentlyContinue; \
         Remove-Printer -Name {p} -ErrorAction SilentlyContinue; Remove-PrinterPort -Name '{port}' -ErrorAction SilentlyContinue; \
         Add-PrinterDriver -Name 'Generic / Text Only' -ErrorAction SilentlyContinue; \
         Add-PrinterPort -Name '{port}' -ErrorAction Stop; \
         Add-Printer -Name {p} -DriverName 'Generic / Text Only' -PortName '{port}' -ErrorAction Stop; 'ok'"
    ));
    assert!(out.contains("ok"), "no se pudo crear la impresora: {out}");
    let t0 = std::time::SystemTime::now();
    spooler::set_hold(p, true, false).unwrap();
    submit(p, "libre.txt");
    let ids = wait_jobs(p, 1);
    assert_eq!(ids.len(), 1);
    spooler::release(ids[0]).unwrap();
    for i in 0..12 {
        submit(p, &format!("retenido-{i}.txt"));
    }
    sleep(Duration::from_secs(4));
    let held = spooler::held_job_ids(p).unwrap();
    let secs = t0.elapsed().unwrap().as_secs();
    let printed = ps(&format!(
        "@(Get-WinEvent -FilterHashtable @{{LogName='Microsoft-Windows-PrintService/Operational';Id=307;StartTime=(Get-Date).AddSeconds(-{})}} -ErrorAction SilentlyContinue | Where-Object {{ $_.Message -match 'retenido-' }}).Count",
        secs + 5
    ));
    for id in &held {
        let _ = spooler::cancel(*id);
    }
    spooler::set_hold(p, false, false).unwrap();
    ps(&format!("Get-PrintJob -PrinterName {p} | Remove-PrintJob -ErrorAction SilentlyContinue; Remove-Printer -Name {p}; Remove-PrinterPort -Name '{port}' -ErrorAction SilentlyContinue"));
    assert_eq!(held.len(), 12, "los 12 trabajos nuevos deben seguir retenidos");
    assert!(printed.trim().ends_with('0'), "se imprimieron trabajos retenidos: {printed}");
}

#[test]
fn retiene_lista_libera_y_cancela() {
    // Nombre propio (no pisa colas del equipo) y puerto de archivo: con PORTPROMPT: el spooler imprime
    // aunque la cola esté en pausa, y el driver "Print to PDF" rechaza datos RAW.
    let p = "CtrlTestAuto";
    // Ruta larga (sin 8.3 como ADMINI~1: el spooler rechaza el "~" con 0x800700A1).
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let port = format!(r"{base}\ctrltest-auto.prn");
    let out = ps(&format!(
        "Get-PrintJob -PrinterName {p} -ErrorAction SilentlyContinue | Remove-PrintJob -ErrorAction SilentlyContinue; \
         Remove-Printer -Name {p} -ErrorAction SilentlyContinue; Remove-PrinterPort -Name '{port}' -ErrorAction SilentlyContinue; \
         Add-PrinterDriver -Name 'Generic / Text Only' -ErrorAction SilentlyContinue; \
         Add-PrinterPort -Name '{port}' -ErrorAction Stop; \
         Add-Printer -Name {p} -DriverName 'Generic / Text Only' -PortName '{port}' -ErrorAction Stop; 'ok'"
    ));
    assert!(out.contains("ok"), "no se pudo crear la impresora: {out}");

    assert!(spooler::list_printers().unwrap().iter().any(|n| n == p), "no aparece en list_printers");
    eprintln!("set_hold");
    spooler::set_hold(p, true, false).unwrap();
    eprintln!("hold ok");

    submit(p, "doc-uno.txt");
    let ids = wait_jobs(p, 1);
    if ids.is_empty() {
        eprintln!("DIAG jobs: {}", ps(&format!("Get-PrintJob -PrinterName {p} | Format-List *")));
        eprintln!("DIAG printer: {}", ps(&format!("Get-Printer -Name {p} | Format-List Name,PrinterStatus,JobCount,PortName,DriverName")));
    }
    assert_eq!(ids.len(), 1, "debe haber 1 trabajo retenido");
    let info = spooler::job_info(Path::new("."), ids[0]).unwrap();
    assert!(info.held, "el trabajo debe estar retenido");
    println!("trabajo: {:?} usuario: {}", info.job.document, info.job.user.clone().unwrap_or_default());

    // Cancelar: el trabajo desaparece de la cola.
    assert_eq!(spooler::cancel(ids[0]).unwrap(), Cancelled::Held);
    sleep(Duration::from_secs(2));
    assert!(ps(&format!("(Get-PrintJob -PrinterName {p} | Measure-Object).Count")).trim().ends_with('0'));

    // Liberar: un segundo trabajo deja de estar retenido.
    submit(p, "doc-dos.txt");
    let ids = wait_jobs(p, 1);
    assert_eq!(ids.len(), 1);
    spooler::release(ids[0]).unwrap();
    sleep(Duration::from_secs(1));
    assert!(!spooler::job_info(Path::new("."), ids[0]).map(|i| i.held).unwrap_or(false), "debe quedar liberado");

    spooler::set_hold(p, false, false).unwrap();
    ps(&format!("Get-PrintJob -PrinterName {p} | Remove-PrintJob; Remove-Printer -Name {p}; Remove-PrinterPort -Name '{port}' -ErrorAction SilentlyContinue"));
}

/// Cola con puerto TCP lento: el trabajo liberado se queda «imprimiendo» (el receptor tarda en leer).
fn slow_queue(p: &str, tcp_port: u16) -> std::thread::JoinHandle<()> {
    use std::{io::Read, net::TcpListener};
    let l = TcpListener::bind(("127.0.0.1", tcp_port)).expect("bind");
    let h = std::thread::spawn(move || {
        if let Ok((mut c, _)) = l.accept() {
            sleep(Duration::from_secs(5));
            let mut b = vec![];
            let _ = c.read_to_end(&mut b);
        }
    });
    let out = ps(&format!(
        "Get-PrintJob -PrinterName {p} -ErrorAction SilentlyContinue | Remove-PrintJob -ErrorAction SilentlyContinue; \
         Remove-Printer -Name {p} -ErrorAction SilentlyContinue; Remove-PrinterPort -Name 'IP_127.0.0.1_{tcp_port}' -ErrorAction SilentlyContinue; \
         Add-PrinterDriver -Name 'Generic / Text Only' -ErrorAction SilentlyContinue; \
         Add-PrinterPort -Name 'IP_127.0.0.1_{tcp_port}' -PrinterHostAddress 127.0.0.1 -PortNumber {tcp_port} -ErrorAction Stop; \
         Add-Printer -Name {p} -DriverName 'Generic / Text Only' -PortName 'IP_127.0.0.1_{tcp_port}' -ErrorAction Stop; 'ok'"
    ));
    assert!(out.contains("ok"), "no se pudo crear la impresora: {out}");
    h
}

fn drop_queue(p: &str, tcp_port: u16) {
    ps(&format!("Get-PrintJob -PrinterName {p} | Remove-PrintJob -ErrorAction SilentlyContinue; Remove-Printer -Name {p}; Remove-PrinterPort -Name 'IP_127.0.0.1_{tcp_port}' -ErrorAction SilentlyContinue"));
}

/// El registro no debe mentir: cancelar un trabajo que ya está imprimiendo no lo detiene del todo y se informa
/// (`AlreadyPrinted`); uno que ya no está en la cola se informa como `Gone`.
#[test]
fn cancelar_un_trabajo_que_ya_salio_lo_dice() {
    let (p, tcp) = ("CtrlTestCancel", 19199);
    let listener = slow_queue(p, tcp);
    spooler::set_hold(p, true, false).unwrap();

    submit(p, "salio.txt");
    let ids = wait_jobs(p, 1);
    assert_eq!(ids.len(), 1);
    spooler::release(ids[0]).unwrap();
    sleep(Duration::from_millis(800)); // el receptor no lee: el trabajo queda en impresión
    assert_eq!(spooler::cancel(ids[0]).unwrap(), Cancelled::AlreadyPrinted);

    // Borrado por otro antes de cancelar.
    submit(p, "desaparece.txt");
    let ids = wait_jobs(p, 1);
    assert_eq!(ids.len(), 1);
    ps(&format!("Get-PrintJob -PrinterName {p} | Where-Object {{ $_.DocumentName -eq 'desaparece.txt' }} | Remove-PrintJob"));
    sleep(Duration::from_millis(500));
    assert_eq!(spooler::cancel(ids[0]).unwrap(), Cancelled::Gone);

    spooler::set_hold(p, false, false).unwrap();
    drop_queue(p, tcp);
    let _ = listener.join();
}

/// Ráfaga mezclada con la cola real: ningún bloqueado sale y todos los permitidos salen. Es lo que fallaba cuando
/// `release` reanudaba la cola durante toda la impresión del permitido.
#[test]
fn rafaga_mezclada_sin_fugas() {
    let p = "CtrlTestMezcla";
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let port = format!(r"{base}\ctrltest-mezcla.prn");
    let out = ps(&format!(
        "Get-PrintJob -PrinterName {p} -ErrorAction SilentlyContinue | Remove-PrintJob -ErrorAction SilentlyContinue; \
         Remove-Printer -Name {p} -ErrorAction SilentlyContinue; Remove-PrinterPort -Name '{port}' -ErrorAction SilentlyContinue; \
         Add-PrinterDriver -Name 'Generic / Text Only' -ErrorAction SilentlyContinue; \
         Add-PrinterPort -Name '{port}' -ErrorAction Stop; \
         Add-Printer -Name {p} -DriverName 'Generic / Text Only' -PortName '{port}' -ErrorAction Stop; 'ok'"
    ));
    assert!(out.contains("ok"), "no se pudo crear la impresora: {out}");
    let t0 = std::time::SystemTime::now();
    spooler::set_hold(p, true, false).unwrap();
    let sender = std::thread::spawn(move || {
        for i in 0..40 {
            submit(p, &if i % 2 == 0 { format!("blq-{i:02}.docx") } else { format!("ok-{i:02}.xlsx") });
        }
    });
    let (mut allowed, mut denied) = (0, 0);
    let end = std::time::Instant::now() + Duration::from_secs(40);
    while allowed + denied < 40 && std::time::Instant::now() < end {
        for id in spooler::held_job_ids(p).unwrap() {
            let Ok(info) = spooler::job_info(Path::new("."), id) else { continue };
            if !info.held {
                continue;
            }
            if info.job.document.starts_with("ok-") {
                if spooler::release(id).is_ok() {
                    allowed += 1;
                }
            } else if spooler::cancel(id).is_ok() {
                denied += 1;
            }
        }
        sleep(Duration::from_millis(50));
    }
    sender.join().unwrap();
    sleep(Duration::from_secs(3));
    let secs = t0.elapsed().unwrap().as_secs() + 5;
    // El evento 307 no trae el título del documento en todas las ediciones de Windows (aquí dice «Print Document»),
    // así que no se filtra por nombre: como los bloqueados se cancelan, deben salir exactamente los 20 permitidos.
    let printed = ps(&format!(
        "@(Get-WinEvent -FilterHashtable @{{LogName='Microsoft-Windows-PrintService/Operational';Id=307;StartTime=(Get-Date).AddSeconds(-{secs})}} -ErrorAction SilentlyContinue | Where-Object {{ $_.Message -match 'printed on {p} ' }}).Count"
    ))
    .trim()
    .to_string();
    spooler::set_hold(p, false, false).unwrap();
    ps(&format!("Get-PrintJob -PrinterName {p} | Remove-PrintJob -ErrorAction SilentlyContinue; Remove-Printer -Name {p}; Remove-PrinterPort -Name '{port}' -ErrorAction SilentlyContinue"));
    assert_eq!((allowed, denied), (20, 20));
    assert_eq!(printed, "20", "impresos (deben ser solo los 20 permitidos; más = fuga de bloqueados): {printed}");
}
