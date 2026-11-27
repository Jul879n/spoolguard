//! Parte pura (sin Windows): nombre del archivo de veredicto y su contenido. La app (`core/src/spooler.rs`) usa la
//! misma regla; ambas se prueban con `tests/names.txt`.

/// Tiempo máximo que un trabajo espera el veredicto (después de terminar de encolarse).
pub const WAIT_MS: u64 = 10_000;
/// Rango válido de la espera configurable (segundos).
pub const MIN_WAIT_S: u64 = 1;
pub const MAX_WAIT_S: u64 = 600;

/// Configuración de la protección (archivo `verdicts\config`, líneas `timeout=<1-600>` y `strict=<0|1>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Cuánto se espera el veredicto, en ms.
    pub timeout_ms: u64,
    /// Estricto: sin veredicto a tiempo el trabajo NO se imprime (queda retenido). Apagado = falla abierta.
    pub strict: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { timeout_ms: WAIT_MS, strict: false }
    }
}

/// Interpreta el archivo de configuración. Cada valor ausente o inválido (no numérico, fuera de 1-600, no 0/1) vale
/// su valor seguro por defecto (10 s, falla abierta); un archivo de más de 256 bytes se ignora entero.
pub fn parse_config(bytes: &[u8]) -> Config {
    let mut c = Config::default();
    if bytes.len() > 256 {
        return c;
    }
    let (mut got_t, mut got_s) = (false, false);
    for line in bytes.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        let Some(eq) = line.iter().position(|b| *b == b'=') else { continue };
        let (k, rest) = line.split_at(eq);
        let (k, v) = (k.trim_ascii(), rest.get(1..).unwrap_or_default().trim_ascii());
        if k == b"timeout" && !got_t {
            got_t = true;
            if !v.is_empty() && v.len() <= 3 && v.iter().all(u8::is_ascii_digit) {
                let n: u64 = v.iter().fold(0, |a, d| a * 10 + u64::from(d - b'0'));
                if (MIN_WAIT_S..=MAX_WAIT_S).contains(&n) {
                    c.timeout_ms = n * 1000;
                }
            }
        } else if k == b"strict" && !got_s {
            got_s = true;
            c.strict = v == b"1";
        }
    }
    c
}

/// Largo máximo (en caracteres) del nombre de cola dentro del nombre del archivo.
pub const MAX_NAME: usize = 128;

/// Nombre de cola seguro como parte de un nombre de archivo: sin separadores de ruta ni caracteres reservados de
/// Windows, sin controles y con largo acotado. Sin separadores no hay forma de salir de la carpeta de veredictos.
pub fn sanitize(queue: &str) -> String {
    let s: String = queue
        .chars()
        .map(|c| if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .take(MAX_NAME)
        .collect();
    if s.is_empty() {
        "_".into()
    } else {
        s
    }
}

/// `<cola saneada>-<id del trabajo>`.
pub fn file_name(queue: &str, job_id: u32) -> String {
    format!("{}-{}", sanitize(queue), job_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

/// Interpreta el contenido del archivo. Cualquier otra cosa no es un veredicto (se sigue esperando).
pub fn parse(bytes: &[u8]) -> Option<Verdict> {
    let t = bytes.trim_ascii();
    if t.eq_ignore_ascii_case(b"deny") {
        Some(Verdict::Deny)
    } else if t.eq_ignore_ascii_case(b"allow") {
        Some(Verdict::Allow)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_compartidos_con_la_app() {
        for l in include_str!("../tests/names.txt").lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (q, id, want) = {
                let mut p = l.splitn(3, '\t');
                (p.next().unwrap_or(""), p.next().unwrap_or("0"), p.next().unwrap_or(""))
            };
            assert_eq!(file_name(q, id.parse().unwrap_or(0)), want, "{l}");
        }
    }

    #[test]
    fn sin_traversal() {
        for q in ["..\\..\\x", "../x", "C:\\a", "a\0b", "\u{7f}"] {
            let n = file_name(q, 1);
            assert!(!n.contains(['\\', '/', ':', '\0']), "{n}");
        }
        assert!(file_name(&"a".repeat(1000), 5).len() <= MAX_NAME + 12);
    }

    #[test]
    fn configuracion() {
        let d = Config::default();
        assert_eq!(d, Config { timeout_ms: 10_000, strict: false });
        assert_eq!(parse_config(b"timeout=3\nstrict=1\n"), Config { timeout_ms: 3_000, strict: true });
        assert_eq!(parse_config(b"timeout=600\r\nstrict=0\r\n"), Config { timeout_ms: 600_000, strict: false });
        for bad in ["timeout=0", "timeout=601", "timeout=-1", "timeout=abc", "timeout=", "timeout=10s", "timeout=0010", "timeout=1e2"] {
            assert_eq!(parse_config(bad.as_bytes()).timeout_ms, 10_000, "{bad}");
        }
        assert_eq!(parse_config(b"timeout=0010").timeout_ms, 10_000);
        for bad in ["strict=2", "strict=true", "strict=", "strict=yes", "strict=-1"] {
            assert!(!parse_config(bad.as_bytes()).strict, "{bad}");
        }
        assert_eq!(parse_config(b""), d);
        assert_eq!(parse_config(b"\xff\xfe\x00"), d);
        assert_eq!(parse_config(&vec![b'a'; 300]), d);
        assert!(parse_config(b"strict=1\nstrict=0").strict); // vale la primera
        assert_eq!(parse_config(b"timeout=5\ntimeout=9").timeout_ms, 5_000);
    }

    #[test]
    fn contenido() {
        assert_eq!(parse(b"deny"), Some(Verdict::Deny));
        assert_eq!(parse(b" ALLOW\r\n"), Some(Verdict::Allow));
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"allowx"), None);
        assert_eq!(parse(b"den"), None);
    }
}
