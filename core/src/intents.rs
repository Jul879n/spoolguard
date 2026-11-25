//! Intenciones de impresión informadas por la extensión del navegador.
//! El spooler solo ve el título de la página; la extensión aporta el dominio.

use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

const FILE: &str = "intents.jsonl";
const MAX_AGE_SECS: u64 = 120;
const MAX_LINES: usize = 200;

#[derive(Serialize, Deserialize)]
struct Intent {
    time: u64,
    host: String,
    title: String,
    /// "agent" cuando la página mandó la orden a un agente local de impresión (QZ Tray), "" si usó el diálogo.
    #[serde(default)]
    kind: String,
}

/// Un trabajo de un agente local (QZ Tray) lleva un nombre arbitrario ("Untitled"): se atribuye al
/// sitio que ordenó imprimir hace pocos segundos.
const AGENT_WINDOW_SECS: u64 = 15;

pub fn host_of_url(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or("");
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = auth.rsplit('@').next().unwrap_or("");
    // IPv6 entre corchetes: [::1]:8080
    if let Some(r) = host.strip_prefix('[') {
        return r.split(']').next().unwrap_or("").to_lowercase();
    }
    host.split(':').next().unwrap_or("").to_lowercase()
}

pub fn record(dir: &Path, url: &str, title: &str, now: u64) -> std::io::Result<()> {
    record_kind(dir, url, title, "", now)
}

pub fn record_kind(dir: &Path, url: &str, title: &str, kind: &str, now: u64) -> std::io::Result<()> {
    let host = host_of_url(url);
    if host.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE);
    // Recorta el archivo para que no crezca sin límite.
    if let Ok(s) = fs::read_to_string(&path) {
        let lines: Vec<&str> = s.lines().collect();
        if lines.len() > MAX_LINES {
            fs::write(&path, lines[lines.len() - MAX_LINES / 2..].join("\n") + "\n")?;
        }
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(&Intent { time: now, host, title: title.to_string(), kind: kind.to_string() }).unwrap())
}

/// Dominio de la intención reciente cuyo título coincide con el nombre del documento; si no hay
/// coincidencia, el de la última orden a un agente local de impresión (QZ Tray) en los últimos segundos.
pub fn find_site(dir: &Path, document: &str, now: u64) -> Option<String> {
    let doc = document.to_lowercase();
    if doc.is_empty() {
        return None;
    }
    let intents: Vec<Intent> = fs::read_to_string(dir.join(FILE))
        .ok()?
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Intent>(l).ok())
        .filter(|i| now.saturating_sub(i.time) <= MAX_AGE_SECS)
        .collect();
    let by_title = intents.iter().find(|i| {
        let t = i.title.trim().to_lowercase();
        t.chars().count() >= 8 && (doc.contains(&t) || t.contains(&doc))
    });
    by_title
        .or_else(|| intents.iter().find(|i| i.kind == "agent" && now.saturating_sub(i.time) <= AGENT_WINDOW_SECS))
        .map(|i| i.host.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_ipv6_y_puerto() {
        assert_eq!(host_of_url("http://[::1]:8080/x"), "::1");
        assert_eq!(host_of_url("https://User@Ejemplo.cl:443/a?b"), "ejemplo.cl");
    }

    #[test]
    fn asocia_titulo_con_dominio() {
        let d = std::env::temp_dir().join(format!("ci-intents-{}", std::process::id()));
        record(&d, "https://www.Facebook.com/x?y=1", "Mi página", 1000).unwrap();
        assert_eq!(find_site(&d, "Mi página - Google Chrome", 1010).as_deref(), Some("www.facebook.com"));
        assert_eq!(find_site(&d, "otra cosa", 1010), None);
        assert_eq!(find_site(&d, "Mi página", 1000 + MAX_AGE_SECS + 1), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn trabajo_de_agente_local_se_atribuye_al_sitio() {
        let d = std::env::temp_dir().join(format!("ci-intents-agent-{}", std::process::id()));
        record_kind(&d, "https://Gesprod.catchup.cl/printing", "Impresión y etiquetado", "agent", 2000).unwrap();
        // QZ Tray manda el trabajo como "Untitled": sin título coincidente, se usa la orden reciente.
        assert_eq!(find_site(&d, "Untitled", 2005).as_deref(), Some("gesprod.catchup.cl"));
        // Fuera de la ventana corta ya no se atribuye.
        assert_eq!(find_site(&d, "Untitled", 2000 + AGENT_WINDOW_SECS + 1), None);
        // Una intención normal (diálogo del navegador) no sirve de respaldo.
        record(&d, "https://otro.cl", "Página de otro sitio", 3000).unwrap();
        assert_eq!(find_site(&d, "Untitled", 3001), None);
        let _ = fs::remove_dir_all(&d);
    }
}
