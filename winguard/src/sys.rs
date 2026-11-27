//! Llamadas a la API de Windows (declaradas a mano; firmas verificadas contra winsplp.h/winspool.h del SDK 10.0.19041
//! y contra el crate `windows` 0.62). Todo `unsafe` queda aquí.
use std::ffi::c_void;

pub type Handle = *mut c_void;
pub type Bool = i32;

/// `PRINTPROCESSOROPENDATA` (winsplp.h).
#[repr(C)]
pub struct OpenData {
    pub dev_mode: *mut c_void,
    pub datatype: *mut u16,
    pub parameters: *mut u16,
    pub document_name: *mut u16,
    pub job_id: u32,
    pub output_file: *mut u16,
    pub printer_name: *mut u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SystemTime {
    pub year: u16,
    pub month: u16,
    pub day_of_week: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FileTime {
    pub low: u32,
    pub high: u32,
}

/// `JOB_INFO_1W` (winspool.h).
#[repr(C)]
pub struct JobInfo1 {
    pub job_id: u32,
    pub printer: *const u16,
    pub machine: *const u16,
    pub user: *const u16,
    pub document: *const u16,
    pub datatype: *const u16,
    pub status_text: *const u16,
    pub status: u32,
    pub priority: u32,
    pub position: u32,
    pub total_pages: u32,
    pub pages_printed: u32,
    pub submitted: SystemTime,
}

pub const JOB_STATUS_DELETING: u32 = 0x4;
pub const JOB_STATUS_SPOOLING: u32 = 0x8;
pub const JOB_STATUS_DELETED: u32 = 0x100;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *const c_void;
    fn GetSystemDirectoryW(buf: *mut u16, size: u32) -> u32;
    fn SystemTimeToFileTime(st: *const SystemTime, ft: *mut FileTime) -> Bool;
    pub fn SetLastError(code: u32);
    fn GetLastError() -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    fn GetUserNameW(buf: *mut u16, size: *mut u32) -> Bool;
    fn OpenThreadToken(thread: Handle, access: u32, as_self: Bool, token: *mut Handle) -> Bool;
    fn RevertToSelf() -> Bool;
    fn SetThreadToken(thread: *mut Handle, token: Handle) -> Bool;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThread() -> Handle;
    fn CloseHandle(h: Handle) -> Bool;
}

const TOKEN_IMPERSONATE: u32 = 0x4;
const TOKEN_QUERY: u32 = 0x8;

/// El spooler llama al procesador **suplantando al dueño del trabajo** (medido: `GetUserNameW` devuelve su nombre). Con esa
/// identidad no se podría leer la carpeta de veredictos (solo SYSTEM, administradores y el usuario de la app) ni escribir el
/// acuse. Esto ejecuta `f` con la identidad del propio servicio (SYSTEM) y restaura la suplantación al terminar, también si
/// `f` hace panic (`default`). Si el hilo no suplanta a nadie, solo ejecuta `f`.
pub fn as_system<T>(default: T, f: impl FnOnce() -> T) -> T {
    let mut tok: Handle = std::ptr::null_mut();
    // SAFETY: llamadas simples a advapi32 sobre el hilo actual; el token se cierra siempre.
    let impersonating = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_IMPERSONATE | TOKEN_QUERY, 1, &mut tok) } != 0 && !tok.is_null();
    if !impersonating {
        return std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(default);
    }
    if unsafe { RevertToSelf() } == 0 {
        unsafe { CloseHandle(tok) };
        return std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(default);
    }
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(default);
    unsafe {
        SetThreadToken(std::ptr::null_mut(), tok);
        CloseHandle(tok);
    }
    r
}

/// Usuario con cuyo token corre el hilo (el del trabajo si el spooler lo suplanta).
pub fn user_name() -> String {
    let mut buf = vec![0u16; 257];
    let mut n = 257u32;
    if unsafe { GetUserNameW(buf.as_mut_ptr(), &mut n) } == 0 || n == 0 {
        return String::new();
    }
    String::from_utf16_lossy(buf.get(..(n as usize).saturating_sub(1)).unwrap_or(&[]))
}

#[link(name = "winspool")]
extern "system" {
    fn OpenPrinterW(name: *const u16, handle: *mut Handle, defaults: *const c_void) -> Bool;
    fn ClosePrinter(handle: Handle) -> Bool;
    fn SetJobW(handle: Handle, job_id: u32, level: u32, job: *mut u8, command: u32) -> Bool;
    fn GetJobW(handle: Handle, job_id: u32, level: u32, buf: *mut u8, size: u32, needed: *mut u32) -> Bool;
}

pub const ERROR_MOD_NOT_FOUND: u32 = 126;
/// Error con el que falla un trabajo retenido por el modo estricto (ERROR_NOT_READY = 21: «el dispositivo no está listo»).
pub const ERROR_PRINT_CANCELLED_HOLD: u32 = 21;

// Firmas de las funciones de winprint.dll (winsplp.h).
pub type OpenFn = unsafe extern "system" fn(*mut u16, *mut OpenData) -> Handle;
pub type PrintFn = unsafe extern "system" fn(Handle, *mut u16) -> Bool;
pub type CloseFn = unsafe extern "system" fn(Handle) -> Bool;
pub type ControlFn = unsafe extern "system" fn(Handle, u32) -> Bool;
pub type CapsFn = unsafe extern "system" fn(*mut u16, u32, *mut u8, u32, *mut u32) -> u32;
pub type EnumFn = unsafe extern "system" fn(*mut u16, *mut u16, u32, *mut u8, u32, *mut u32, *mut u32) -> Bool;

/// Las seis funciones reales de `winprint.dll`.
pub struct Real {
    pub open: OpenFn,
    pub print: PrintFn,
    pub close: CloseFn,
    pub control: ControlFn,
    pub caps: Option<CapsFn>,
    pub enum_datatypes: EnumFn,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Carga `winprint.dll` desde la carpeta de print processors del sistema (ruta absoluta: nunca desde el PATH).
pub fn load_real() -> Option<Real> {
    let mut dir = vec![0u16; 260];
    let n = unsafe { GetSystemDirectoryW(dir.as_mut_ptr(), 260) } as usize;
    if n == 0 || n >= 260 {
        return None;
    }
    let mut path = String::from_utf16_lossy(dir.get(..n)?);
    path.push_str(r"\spool\prtprocs\x64\winprint.dll");
    let m = unsafe { LoadLibraryW(wide(&path).as_ptr()) };
    if m.is_null() {
        return None;
    }
    let sym = |name: &[u8]| unsafe { GetProcAddress(m, name.as_ptr()) };
    let (o, p, c, k, e) = (
        sym(b"OpenPrintProcessor\0"),
        sym(b"PrintDocumentOnPrintProcessor\0"),
        sym(b"ClosePrintProcessor\0"),
        sym(b"ControlPrintProcessor\0"),
        sym(b"EnumPrintProcessorDatatypesW\0"),
    );
    if o.is_null() || p.is_null() || c.is_null() || k.is_null() || e.is_null() {
        return None;
    }
    let caps = sym(b"GetPrintProcessorCapabilities\0");
    // SAFETY: los símbolos de winprint.dll tienen exactamente estas firmas (winsplp.h; verificado con dumpbin).
    unsafe {
        Some(Real {
            open: std::mem::transmute::<*const c_void, OpenFn>(o),
            print: std::mem::transmute::<*const c_void, PrintFn>(p),
            close: std::mem::transmute::<*const c_void, CloseFn>(c),
            control: std::mem::transmute::<*const c_void, ControlFn>(k),
            caps: if caps.is_null() { None } else { Some(std::mem::transmute::<*const c_void, CapsFn>(caps)) },
            enum_datatypes: std::mem::transmute::<*const c_void, EnumFn>(e),
        })
    }
}

/// Texto de un `PWSTR` (acotado a 1024 unidades; nulo = vacío).
pub fn text(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut v = Vec::new();
    for i in 0..1024usize {
        // SAFETY: cadena terminada en cero entregada por el spooler; se corta en 1024 por seguridad.
        let c = unsafe { *p.add(i) };
        if c == 0 {
            break;
        }
        v.push(c);
    }
    String::from_utf16_lossy(&v)
}

/// Estado y hora de envío (FILETIME → segundos Unix) de un trabajo, por una consulta corta al spooler.
pub struct JobState {
    pub status: u32,
    pub submitted_unix: Option<u64>,
    pub document: String,
}

/// Conexión a la cola para consultar el estado del trabajo (se abre una vez por espera).
pub struct Queue(Handle);

#[repr(C)]
struct PrinterDefaults {
    datatype: *mut u16,
    devmode: *mut c_void,
    access: u32,
}

impl Queue {
    pub fn open(name: &str) -> Option<Queue> {
        let mut h: Handle = std::ptr::null_mut();
        let ok = unsafe { OpenPrinterW(wide(name).as_ptr(), &mut h, std::ptr::null()) };
        if ok == 0 || h.is_null() {
            None
        } else {
            Some(Queue(h))
        }
    }

    /// Abre la cola con permiso de administrar (el procesador corre como SYSTEM): hace falta para `SetJob`.
    pub fn open_admin(name: &str) -> Option<Queue> {
        let mut h: Handle = std::ptr::null_mut();
        let d = PrinterDefaults { datatype: std::ptr::null_mut(), devmode: std::ptr::null_mut(), access: 0x000F_000C };
        let ok = unsafe { OpenPrinterW(wide(name).as_ptr(), &mut h, &d as *const PrinterDefaults as *const c_void) };
        if ok == 0 || h.is_null() {
            None
        } else {
            Some(Queue(h))
        }
    }

    /// `SetJob` con un comando (JOB_CONTROL_*). Devuelve el código de error de Windows si falla.
    pub fn control(&self, id: u32, cmd: u32) -> Result<(), u32> {
        if unsafe { SetJobW(self.0, id, 0, std::ptr::null_mut(), cmd) } != 0 {
            Ok(())
        } else {
            Err(unsafe { GetLastError() })
        }
    }

    pub fn job(&self, id: u32) -> Option<JobState> {
        let mut needed = 0u32;
        unsafe { GetJobW(self.0, id, 1, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 || needed > 1 << 16 {
            return None;
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = unsafe { GetJobW(self.0, id, 1, buf.as_mut_ptr() as *mut u8, needed, &mut needed) };
        if ok == 0 {
            return None;
        }
        // SAFETY: el spooler devolvió un JOB_INFO_1W válido en `buf` (alineado a 8 bytes).
        let j = unsafe { &*(buf.as_ptr() as *const JobInfo1) };
        let mut ft = FileTime::default();
        let ok = unsafe { SystemTimeToFileTime(&j.submitted, &mut ft) } != 0;
        let t100 = (u64::from(ft.high) << 32) | u64::from(ft.low);
        Some(JobState {
            status: j.status,
            submitted_unix: ok.then(|| (t100 / 10_000_000).saturating_sub(11_644_473_600)),
            document: text(j.document),
        })
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        unsafe { ClosePrinter(self.0) };
    }
}
